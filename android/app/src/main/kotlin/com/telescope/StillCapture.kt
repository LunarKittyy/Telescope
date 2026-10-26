package com.telescope

import android.content.Context
import android.graphics.ImageFormat
import android.hardware.camera2.CameraCaptureSession
import android.hardware.camera2.CameraCharacteristics
import android.hardware.camera2.CameraDevice
import android.hardware.camera2.CameraExtensionSession
import android.hardware.camera2.CameraManager
import android.hardware.camera2.CameraMetadata
import android.hardware.camera2.CaptureFailure
import android.hardware.camera2.CaptureRequest
import android.hardware.camera2.CaptureResult
import android.hardware.camera2.DngCreator
import android.hardware.camera2.TotalCaptureResult
import android.hardware.camera2.params.ExtensionSessionConfiguration
import android.hardware.camera2.params.OutputConfiguration
import android.hardware.camera2.params.SessionConfiguration
import android.media.Image
import android.media.ImageReader
import android.os.Build
import android.os.Handler
import android.os.SystemClock
import android.view.Surface
import java.io.File
import java.io.FileOutputStream
import java.util.concurrent.Executor
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicInteger

// Takes /v1/still's photos on the camera thread: next to the stream where the phone allows it, else in a session of their own.
class StillCapture(
    private val context: Context,
    private val controller: CameraSessionController,
    private val jobs: StillJobs,
    // A photo is being taken: the server lets a viewer go a while without frames (a long exposure, a paused video)
    private val onBusy: (Boolean) -> Unit,
    private val onError: (String, Throwable) -> Unit,
) {
    companion object {
        private const val TAG = "StillCapture"
        private const val SETTLE_MIN_MS = 300L   // its own session: auto exposure and focus get this long at least
        private const val SETTLE_MAX_MS = 2_000L // and no longer than this
        private const val FOCUS_TRIES = 3         // a frame taken with the lens still moving is taken again
        private const val FOCUS_SETTLE_FRAMES = 3 // frames for the lens to get to the next focus distance
        private const val SHOT_MS = 5_000L        // time allowed per picture, on top of its exposure
        private const val SETUP_MS = 20_000L      // and for setting the session up
        private const val RAW_IMAGES = 3          // RAW buffers held at once (a DNG holds its image until it's written)
        private const val MAX_RES_RAW_IMAGES = 2  // and at full resolution, where each is 100 MB or more
        private const val IMAGES = 4              // compressed buffers held at once
        private const val MAX_WRITES = 4          // pictures waiting for the writer before no more are taken in

        // How a photo output goes into a session: on a lens of a multi-lens camera, and in the full resolution mode
        @androidx.annotation.RequiresApi(Build.VERSION_CODES.P)
        fun outputConfig(still: StillOutput, physId: String?): OutputConfiguration =
            OutputConfiguration(still.surface).also { cfg ->
                physId?.let { cfg.setPhysicalCameraId(it) }
                if (still.maxRes && Build.VERSION.SDK_INT >= Build.VERSION_CODES.S)
                    cfg.addSensorPixelModeUsed(CameraMetadata.SENSOR_PIXEL_MODE_MAXIMUM_RESOLUTION)
            }

        fun imageFormat(format: String): Int = when (format) {
            StillOptions.HEIC -> ImageFormat.HEIC
            StillOptions.ULTRA_HDR -> ImageFormat.JPEG_R
            StillOptions.RAW -> ImageFormat.RAW_SENSOR
            else -> ImageFormat.JPEG
        }

        fun extension(format: String): String = when (format) {
            StillOptions.HEIC -> "heic"
            StillOptions.RAW -> "dng"
            else -> "jpg"
        }

        // EXIF orientation values (ExifInterface.ORIENTATION_*), which DngCreator takes
        fun exifOrientation(rotation: Int): Int = when (rotation) {
            90 -> 6
            180 -> 3
            270 -> 8
            else -> 1
        }
    }

    // A photo output and the reader its pictures come out of; format is a StillOptions name
    private class Output(val format: String, val reader: ImageReader, val still: StillOutput, val maxImages: Int) {
        val acquired = AtomicInteger()  // images taken from the reader and not closed yet
    }

    // The outputs a request needs, kept in the stream's session for the next photo once they're in it
    private class Outputs(val key: String, val list: List<Output>) {
        val stills = list.map { it.still }
        fun close() = list.forEach { runCatching { it.reader.close() } }
    }

    private inner class Run(val id: String, val request: StillRequest, val caps: StillCaps, val generation: Int,
                            val array: SensorBox?, val maxResArray: SensorBox?) {
        var active = true
        var outputs: Outputs? = null
        var own: Outputs? = null      // its own session's outputs, closed when it's done
        var meter: ImageReader? = null
        var session: CameraCaptureSession? = null  // its own session
        var extension: Any? = null    // its CameraExtensionSession, Android 12
        var paused = false
        val expected = request.shots.size * request.filesPerShot
        var written = 0
        val tries = IntArray(request.shots.size)
        val results = HashMap<Long, TotalCaptureResult>()
        val images = HashMap<Long, MutableList<Pair<Output, Image>>>()
        val discard = HashSet<Long>()
        // Pictures wait for their capture result (DNGs need it, and it tells stale pictures apart); the maker's modes have none
        val needsResult = request.extension == null
        val requests = arrayOfNulls<CaptureRequest>(request.shots.size)
        var target: CameraCaptureSession? = null  // the session its pictures are asked of
        var firstTs = Long.MAX_VALUE  // when its first frame started: pictures from before are an earlier photo's
    }

    private val manager = context.getSystemService(Context.CAMERA_SERVICE) as CameraManager
    private val writer = Executors.newSingleThreadExecutor { r -> Thread(r, "still-writer").apply { isDaemon = true } }
    private var current: Run? = null      // camera thread only
    private var waiting: Waiting? = null  // a photo on its timer, camera thread only
    private var kept: Outputs? = null     // camera thread only
    private val refused = HashSet<String>()  // output sets this lens and stream configuration wouldn't take
    private val writes = AtomicInteger()      // pictures handed to the writer and not written yet
    @Volatile private var stopped = false

    private class Waiting(val id: String, val cameraId: String, val go: Runnable)

    private fun handler(): Handler? = controller.stillHandler()

    // From any thread: shoot the request in StillJobs as id with cameraId's lens, after its timer
    fun start(id: String, request: StillRequest, caps: StillCaps, cameraId: String) {
        onBusy(true)
        val h = handler()
        lateinit var go: Runnable
        go = Runnable {
            if (waiting?.go === go) waiting = null
            begin(id, request, caps, cameraId)
        }
        if (h == null || !h.post { waiting = Waiting(id, cameraId, go) } || !h.postDelayed(go, request.timerMs)) {
            h?.removeCallbacks(go)
            jobs.fail(id, "camera_off", "The camera isn't running")
            onBusy(false)
        }
    }

    // From any thread, once the controller was asked for another lens: a photo on its timer for the old lens fails now
    fun lensChanged() {
        handler()?.post {
            val w = waiting ?: return@post
            if (controller.stillCamera()?.id == w.cameraId) return@post
            waiting = null
            handler()?.removeCallbacks(w.go)
            jobs.fail(w.id, "interrupted", "The lens changed before the photo")
            onBusy(false)
        }
    }

    // The stream is stopping: anything in flight ends, the readers go
    fun stop() {
        stopped = true
        val h = handler()
        if (h == null || !h.post { release(); writer.shutdown() }) { release(); writer.shutdown() }
    }

    private fun release() {
        waiting?.let { w -> handler()?.removeCallbacks(w.go); jobs.fail(w.id, "stopped", "The stream stopped"); onBusy(false) }
        waiting = null
        current?.let { end(it, "stopped", "The stream stopped", restore = false) }
        kept?.close()
        kept = null
    }

    private fun begin(id: String, request: StillRequest, caps: StillCaps, cameraId: String) {
        if (stopped) return
        val device = controller.stillDevice()
        if (device == null || controller.stillSession() == null && !request.ownSession) {
            jobs.fail(id, "camera_off", "The camera isn't running")
            onBusy(false)
            return
        }
        val cam = controller.stillCamera()
        if (cam?.id != cameraId) {
            // The lens changed during the timer: its sizes and what it can do were the other lens's
            jobs.fail(id, "interrupted", "The lens changed before the photo")
            onBusy(false)
            return
        }
        val maxResArray = if (request.maxRes && Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) runCatching {
            manager.getCameraCharacteristics(cam.id).get(CameraCharacteristics.SENSOR_INFO_ACTIVE_ARRAY_SIZE_MAXIMUM_RESOLUTION)
                ?.let { SensorBox(it.left, it.top, it.width(), it.height()) }
        }.getOrNull() else null
        val r = Run(id, request, caps, controller.stillGeneration(), cam.activeArray, maxResArray)
        current = r
        handler()?.postDelayed({ if (current === r && r.active) end(r, "timeout", "The phone took too long") },
            SETUP_MS + request.exposureMs + request.shots.size * SHOT_MS)
        try {
            if (request.extension != null) { shootExtension(r, device); return }
            val key = outputsKey(request)
            val refusal = "${controller.stillStreamKey()}|$key"
            val old = kept
            if (old != null && old.key == key && controller.stillOutputsInSession(old.stills)) {
                r.outputs = old
                beside(r)
                return
            }
            // Readers that dropped out of the session (a reopen, a paused photo) can go now
            if (old != null && !controller.stillOutputsInSession(old.stills)) { old.close(); kept = null }
            if (refusal in refused) { alone(r); return }
            val fresh = makeOutputs(request, caps, key)
            if (controller.stillsFit(fresh.stills) == false) { fresh.close(); refused += refusal; alone(r); return }
            controller.useStillOutputs(fresh.stills) { ok ->
                // The session has let go of the old ones either way: in it now are the new ones or none
                kept?.close()
                kept = if (ok) fresh else null
                if (!ok) { fresh.close(); refused += refusal }
                if (!r.active) return@useStillOutputs
                r.outputs = if (ok) fresh else null
                if (ok) beside(r) else alone(r)
            }
        } catch (e: Exception) {
            onError("still.begin", e)
            end(r, "failed", "The phone couldn't take the photo")
        }
    }

    private fun outputsKey(request: StillRequest): String =
        listOfNotNull(request.format, StillOptions.RAW.takeIf { request.raw }, "max".takeIf { request.maxRes }).joinToString("+")

    private fun makeOutputs(request: StillRequest, caps: StillCaps, key: String): Outputs {
        val formats = listOfNotNull(request.format, StillOptions.RAW.takeIf { request.raw })
        return Outputs(key, formats.map { format ->
            val size = when {
                format == StillOptions.RAW && request.maxRes -> caps.maxResRaw
                format == StillOptions.RAW -> caps.raw
                request.maxRes -> caps.maxResJpeg
                format == StillOptions.HEIC -> caps.heic
                format == StillOptions.ULTRA_HDR -> caps.ultraHdr
                else -> caps.jpeg
            } ?: error("no size for $format")
            val images = when {
                format != StillOptions.RAW -> IMAGES
                request.maxRes -> MAX_RES_RAW_IMAGES
                else -> RAW_IMAGES
            }
            val reader = ImageReader.newInstance(size.first, size.second, imageFormat(format), images)
            Output(format, reader, StillOutput(reader.surface, request.maxRes), images)
        })
    }

    // ── Next to the stream ─────────────────────────────────────────────────

    private fun beside(r: Run) {
        val session = controller.stillSession() ?: return end(r, "camera_off", "The camera isn't running")
        jobs.capturing(r.id, paused = false)
        listen(r, r.outputs!!)
        shoot(r, session, streamTargets = !r.request.maxRes)
    }

    // ── A session of its own, the video paused ─────────────────────────────

    private fun alone(r: Run) {
        val device = controller.stillDevice() ?: return end(r, "camera_off", "The camera isn't running")
        val outputs = makeOutputs(r.request, r.caps, outputsKey(r.request))
        r.own = outputs
        r.outputs = outputs
        if (!controller.pauseForStill()) return end(r, "camera_off", "The camera isn't running")
        r.paused = true
        jobs.capturing(r.id, paused = true)
        listen(r, outputs)
        openAlone(r, device, withMeter = true)
    }

    // withMeter: a small output for auto exposure and focus to settle on first, as they don't run on photos
    private fun openAlone(r: Run, device: CameraDevice, withMeter: Boolean) {
        val physId = controller.stillPhysicalId()
        val meter = if (withMeter) meterReader() else null
        r.meter = meter
        val stills = r.outputs!!.stills
        val callback = object : CameraCaptureSession.StateCallback() {
            override fun onConfigured(s: CameraCaptureSession) {
                if (!r.active || current !== r) { s.close(); return }
                r.session = s
                if (meter != null) settle(r, s, meter) else shoot(r, s, streamTargets = false)
            }
            override fun onConfigureFailed(s: CameraCaptureSession) {
                if (!r.active) return
                meter?.close()
                r.meter = null
                if (withMeter) openAlone(r, device, withMeter = false)
                else end(r, "unsupported", "The phone can't take this photo")
            }
            override fun onClosed(s: CameraCaptureSession) {
                if (r.active && r.session === s) end(r, "interrupted", "The camera was busy with something else")
            }
        }
        try {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
                val cfgs = stills.map { outputConfig(it, physId) } +
                    listOfNotNull(meter?.let { m -> OutputConfiguration(m.surface).also { c -> physId?.let(c::setPhysicalCameraId) } })
                device.createCaptureSession(SessionConfiguration(SessionConfiguration.SESSION_REGULAR, cfgs,
                    Executor { cmd -> handler()?.post(cmd) }, callback))
            } else {
                @Suppress("DEPRECATION")
                device.createCaptureSession(stills.map { it.surface } + listOfNotNull(meter?.surface), callback, handler())
            }
        } catch (e: Exception) {
            onError("still.openAlone", e)
            end(r, "failed", "The phone couldn't take the photo")
        }
    }

    // A small YUV output, drained as it fills, for the repeating request auto exposure and focus run on
    private fun meterReader(): ImageReader? = runCatching {
        val cam = controller.stillCamera() ?: return null
        val map = manager.getCameraCharacteristics(cam.id).get(CameraCharacteristics.SCALER_STREAM_CONFIGURATION_MAP)
        val size = map?.getOutputSizes(ImageFormat.YUV_420_888)?.filter { it.width >= 320 && it.width <= 1920 }
            ?.minByOrNull { kotlin.math.abs(it.width * it.height - 640 * 480) } ?: return null
        ImageReader.newInstance(size.width, size.height, ImageFormat.YUV_420_888, 2).also { reader ->
            reader.setOnImageAvailableListener({ it.acquireLatestImage()?.close() }, handler())
        }
    }.getOrNull()

    // Runs the stream's settings on the meter until exposure and focus settle, then shoots
    private fun settle(r: Run, s: CameraCaptureSession, meter: ImageReader) {
        val device = controller.stillDevice() ?: return end(r, "camera_off", "The camera isn't running")
        val started = SystemClock.elapsedRealtime()
        var shot = false
        fun go() {
            if (shot || !r.active) return
            shot = true
            shoot(r, s, streamTargets = false)
        }
        try {
            val request = controller.stillRequest(device, CameraDevice.TEMPLATE_PREVIEW).apply { addTarget(meter.surface) }.build()
            s.setRepeatingRequest(request, object : CameraCaptureSession.CaptureCallback() {
                override fun onCaptureCompleted(session: CameraCaptureSession, req: CaptureRequest, result: TotalCaptureResult) {
                    if (SystemClock.elapsedRealtime() - started >= SETTLE_MIN_MS && settled(result)) go()
                }
            }, handler())
            handler()?.postDelayed({ go() }, SETTLE_MAX_MS)
        } catch (e: Exception) {
            onError("still.settle", e)
            go()
        }
    }

    private fun settled(result: TotalCaptureResult): Boolean {
        val ae = result.get(CaptureResult.CONTROL_AE_STATE)
        val af = result.get(CaptureResult.CONTROL_AF_STATE)
        val aeDone = ae == null || result.get(CaptureResult.CONTROL_AE_MODE) == CaptureResult.CONTROL_AE_MODE_OFF ||
            ae == CaptureResult.CONTROL_AE_STATE_CONVERGED || ae == CaptureResult.CONTROL_AE_STATE_LOCKED ||
            ae == CaptureResult.CONTROL_AE_STATE_FLASH_REQUIRED
        val afDone = af == null || (af != CaptureResult.CONTROL_AF_STATE_PASSIVE_SCAN && af != CaptureResult.CONTROL_AF_STATE_ACTIVE_SCAN)
        return aeDone && afDone
    }

    // ── Pictures ───────────────────────────────────────────────────────────

    private fun listen(r: Run, outputs: Outputs) {
        for (output in outputs.list) {
            discardWaiting(output)  // left over from an earlier photo on a kept reader
            output.reader.setOnImageAvailableListener({ drain(r, output) }, handler())
        }
    }

    // Takes in what fits in the buffers and the writer's queue; the rest waits until a picture is closed or written
    private fun drain(r: Run, output: Output) {
        if (current !== r || !r.active) { discardWaiting(output); return }
        while (output.acquired.get() < output.maxImages && writes.get() < MAX_WRITES) {
            val image = try { output.reader.acquireNextImage() } catch (_: Exception) { null } ?: return
            output.acquired.incrementAndGet()
            onImage(r, output, image)
        }
    }

    private fun drainAll() {
        val r = current ?: return
        r.outputs?.list?.forEach { drain(r, it) }
    }

    // From any thread: the picture's buffer goes back, and anything waiting behind it is taken in
    private fun close(output: Output, image: Image) {
        runCatching { image.close() }
        output.acquired.decrementAndGet()
        handler()?.post { drainAll() }
    }

    // Pictures nobody wants any more (a photo that ended, an earlier one's): closed as they come
    private fun discardWaiting(output: Output) {
        while (output.acquired.get() < output.maxImages) {
            val image = try { output.reader.acquireNextImage() } catch (_: Exception) { null } ?: return
            image.close()
        }
    }

    // streamTargets: the stream's outputs come along in each request, so the video keeps getting frames
    private fun shoot(r: Run, session: CameraCaptureSession, streamTargets: Boolean) {
        val device = controller.stillDevice() ?: return end(r, "camera_off", "The camera isn't running")
        r.target = session
        try {
            val burst = mutableListOf<CaptureRequest>()
            var lastFocus: Float? = null
            r.request.shots.forEachIndexed { i, shot ->
                // The lens moves to a new focus distance over a few frames: those go to the video or the meter first
                if (shot.focus != null && shot.focus != lastFocus) repeat(FOCUS_SETTLE_FRAMES) { settleFrame(r, device, shot.focus)?.let(burst::add) }
                lastFocus = shot.focus
                r.requests[i] = buildShot(r, device, shot, i, streamTargets).also(burst::add)
            }
            if (burst.size == 1) session.capture(burst[0], shotCallback(r), handler())
            else session.captureBurst(burst, shotCallback(r), handler())
        } catch (e: Exception) {
            onError("still.shoot", e)
            end(r, "failed", "The phone couldn't take the photo")
        }
    }

    // A frame at this focus distance for the stream's outputs (or the meter), none of the photo's; null without either
    private fun settleFrame(r: Run, device: CameraDevice, focus: Float): CaptureRequest? {
        val targets = r.meter?.let { listOf(it.surface) } ?: if (r.paused) emptyList() else controller.stillStreamTargets()
        if (targets.isEmpty()) return null
        return controller.stillRequest(device, CameraDevice.TEMPLATE_PREVIEW).apply {
            targets.forEach { addTarget(it) }
            set(CaptureRequest.CONTROL_AF_MODE, CaptureRequest.CONTROL_AF_MODE_OFF)
            set(CaptureRequest.LENS_FOCUS_DISTANCE, focus)
        }.build()
    }

    private fun buildShot(r: Run, device: CameraDevice, shot: StillShot, index: Int, streamTargets: Boolean): CaptureRequest {
        val req = r.request
        return controller.stillRequest(device, CameraDevice.TEMPLATE_STILL_CAPTURE).apply {
            r.outputs!!.list.forEach { addTarget(it.reader.surface) }
            // A long or bracketed exposure would flash through the video: those frames stay out of it
            if (streamTargets && shot.exposureNs == null) controller.stillStreamTargets().forEach { addTarget(it) }
            set(CaptureRequest.JPEG_QUALITY, req.quality.toByte())
            set(CaptureRequest.JPEG_ORIENTATION, req.rotation)
            if (req.nrHq) set(CaptureRequest.NOISE_REDUCTION_MODE, CaptureRequest.NOISE_REDUCTION_MODE_HIGH_QUALITY)
            if (req.edgeHq) set(CaptureRequest.EDGE_MODE, CaptureRequest.EDGE_MODE_HIGH_QUALITY)
            if (req.hotPixelHq) set(CaptureRequest.HOT_PIXEL_MODE, CaptureRequest.HOT_PIXEL_MODE_HIGH_QUALITY)
            if (req.distortion != null && Build.VERSION.SDK_INT >= Build.VERSION_CODES.P)
                set(CaptureRequest.DISTORTION_CORRECTION_MODE,
                    if (req.distortion) CaptureRequest.DISTORTION_CORRECTION_MODE_HIGH_QUALITY else CaptureRequest.DISTORTION_CORRECTION_MODE_OFF)
            if (shot.exposureNs != null && shot.iso != null) {
                set(CaptureRequest.CONTROL_AE_MODE, CaptureRequest.CONTROL_AE_MODE_OFF)
                set(CaptureRequest.SENSOR_EXPOSURE_TIME, shot.exposureNs)
                set(CaptureRequest.SENSOR_SENSITIVITY, shot.iso)
                set(CaptureRequest.SENSOR_FRAME_DURATION, shot.exposureNs)
            }
            if (shot.focus != null) {
                set(CaptureRequest.CONTROL_AF_MODE, CaptureRequest.CONTROL_AF_MODE_OFF)
                set(CaptureRequest.LENS_FOCUS_DISTANCE, shot.focus)
            }
            if (req.maxRes && Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
                set(CaptureRequest.SENSOR_PIXEL_MODE, CameraMetadata.SENSOR_PIXEL_MODE_MAXIMUM_RESOLUTION)
                toMaxRes(this, r)
            }
            if (req.raw) set(CaptureRequest.STATISTICS_LENS_SHADING_MAP_MODE, CaptureRequest.STATISTICS_LENS_SHADING_MAP_MODE_ON)
            setTag(index)
        }.build()
    }

    // The crop and metering regions are measured on the full resolution array in that mode, not the usual one
    private fun toMaxRes(builder: CaptureRequest.Builder, r: Run) {
        val from = r.array ?: return
        val to = r.maxResArray ?: return
        fun scaled(rect: android.graphics.Rect): android.graphics.Rect {
            val b = StillOptions.scaleBox(SensorBox(rect.left, rect.top, rect.width(), rect.height()), from, to)
            return android.graphics.Rect(b.left, b.top, b.left + b.width, b.top + b.height)
        }
        builder.get(CaptureRequest.SCALER_CROP_REGION)?.let { builder.set(CaptureRequest.SCALER_CROP_REGION, scaled(it)) }
        for (key in listOf(CaptureRequest.CONTROL_AF_REGIONS, CaptureRequest.CONTROL_AE_REGIONS)) {
            val regions = builder.get(key) ?: continue
            builder.set(key, regions.map { m ->
                val rect = scaled(m.rect)
                android.hardware.camera2.params.MeteringRectangle(rect, m.meteringWeight)
            }.toTypedArray())
        }
    }

    private fun shotCallback(r: Run) = object : CameraCaptureSession.CaptureCallback() {
        override fun onCaptureStarted(session: CameraCaptureSession, request: CaptureRequest, timestamp: Long, frameNumber: Long) {
            if (current !== r || !r.active || timestamp >= r.firstTs) return
            r.firstTs = timestamp
            // Pictures an earlier photo left on a kept reader never get a result here, so they'd hold their buffers
            r.images.keys.filter { it < timestamp }.forEach { ts -> r.images.remove(ts)?.forEach { (output, image) -> close(output, image) } }
        }

        override fun onCaptureCompleted(session: CameraCaptureSession, request: CaptureRequest, result: TotalCaptureResult) {
            if (current !== r || !r.active) return
            val index = request.tag as? Int ?: return
            val ts = result.get(CaptureResult.SENSOR_TIMESTAMP) ?: return
            val focus = r.request.shots.getOrNull(index)?.focus
            if (focus != null && result.get(CaptureResult.LENS_STATE) == CaptureResult.LENS_STATE_MOVING &&
                r.tries[index] < FOCUS_TRIES) {
                r.tries[index]++
                r.discard += ts
                r.images.remove(ts)?.forEach { (output, image) -> close(output, image) }
                retry(r, index)
                return
            }
            jobs.shotTaken(r.id)
            r.results[ts] = result
            r.images.remove(ts)?.forEach { (output, image) -> save(r, output, image, result) }
        }

        override fun onCaptureFailed(session: CameraCaptureSession, request: CaptureRequest, failure: CaptureFailure) {
            if (current !== r || !r.active) return
            val index = request.tag as? Int ?: return
            if (r.tries[index] < 1 && failure.reason != CaptureFailure.REASON_FLUSHED) { r.tries[index]++; retry(r, index); return }
            end(r, "failed", "The camera couldn't take the photo")
        }
    }

    private fun retry(r: Run, index: Int) {
        val session = r.target ?: return
        try {
            session.capture(r.requests[index]!!, shotCallback(r), handler())
        } catch (e: Exception) {
            onError("still.retry", e)
            end(r, "failed", "The camera couldn't take the photo")
        }
    }

    private fun onImage(r: Run, output: Output, image: Image) {
        val ts = image.timestamp
        if (ts in r.discard || ts < r.firstTs && r.firstTs != Long.MAX_VALUE) { close(output, image); return }
        if (!r.needsResult) { save(r, output, image, null); return }
        val result = r.results[ts]
        if (result != null) save(r, output, image, result)
        else r.images.getOrPut(ts) { mutableListOf() } += output to image
    }

    // A compressed picture is copied out and freed at once; a RAW one holds its image until the writer has the DNG
    private fun save(r: Run, output: Output, image: Image, result: TotalCaptureResult?) {
        val file = jobs.newFile(r.id, extension(output.format)) ?: run { close(output, image); return }
        writes.incrementAndGet()
        if (output.format != StillOptions.RAW) {
            val bytes = try {
                val buf = image.planes[0].buffer
                ByteArray(buf.remaining()).also { buf.get(it) }
            } finally { close(output, image) }
            writer.execute { write(r, file) { out -> out.write(bytes) } }
            return
        }
        val cam = controller.stillCamera()
        val id = cam?.id
        if (id == null || result == null) { close(output, image); writes.decrementAndGet(); return }
        val chars = manager.getCameraCharacteristics(id)
        val physical = if (cam.logicalId != null && Build.VERSION.SDK_INT >= Build.VERSION_CODES.S)
            result.physicalCameraTotalResults?.get(id) else null
        val rotation = r.request.rotation
        writer.execute {
            try {
                write(r, file) { out ->
                    DngCreator(chars, physical ?: result).use { dng ->
                        dng.setOrientation(exifOrientation(rotation))
                        dng.writeImage(out, image)
                    }
                }
            } finally { close(output, image) }
        }
    }

    // On the writer thread
    private fun write(r: Run, file: File, body: (FileOutputStream) -> Unit) {
        try {
            FileOutputStream(file).use(body)
            jobs.fileDone(r.id, file)
            handler()?.post {
                r.written++
                if (r.active && r.written >= r.expected) end(r, null, null)
            }
        } catch (e: Exception) {
            file.delete()
            onError("still.write", e)
            handler()?.post { if (r.active) end(r, "failed", "The phone couldn't save the photo") }
        } finally {
            writes.decrementAndGet()
            handler()?.post { drainAll() }
        }
    }

    // ── The maker's modes ──────────────────────────────────────────────────

    private fun shootExtension(r: Run, device: CameraDevice) {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.S) return end(r, "unsupported", "This phone has no Night, HDR or Bokeh")
        val cam = controller.stillCamera() ?: return end(r, "camera_off", "The camera isn't running")
        val type = StillOptions.EXTENSIONS.entries.first { it.value == r.request.extension }.key
        val ext = manager.getCameraExtensionCharacteristics(cam.logicalId ?: cam.id)
        val size = ext.getExtensionSupportedSizes(type, ImageFormat.JPEG).maxByOrNull { it.width.toLong() * it.height }
            ?: return end(r, "unsupported", "This lens has no '${r.request.extension}' mode")
        val reader = ImageReader.newInstance(size.width, size.height, ImageFormat.JPEG, IMAGES)
        val outputs = Outputs("ext", listOf(Output(StillOptions.JPEG, reader, StillOutput(reader.surface, false), IMAGES)))
        r.own = outputs
        r.outputs = outputs
        if (!controller.pauseForStill()) return end(r, "camera_off", "The camera isn't running")
        r.paused = true
        jobs.capturing(r.id, paused = true)
        listen(r, outputs)
        val exec = Executor { cmd -> handler()?.post(cmd) }
        val callback = object : CameraExtensionSession.StateCallback() {
            override fun onConfigured(session: CameraExtensionSession) {
                if (!r.active || current !== r) { session.close(); return }
                r.extension = session
                try {
                    val keys = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) ext.getAvailableCaptureRequestKeys(type) else emptySet()
                    val built = device.createCaptureRequest(CameraDevice.TEMPLATE_STILL_CAPTURE).apply {
                        addTarget(reader.surface)
                        set(CaptureRequest.JPEG_QUALITY, r.request.quality.toByte())
                        set(CaptureRequest.JPEG_ORIENTATION, r.request.rotation)
                        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R && CaptureRequest.CONTROL_ZOOM_RATIO in keys)
                            controller.stillRequest(device, CameraDevice.TEMPLATE_STILL_CAPTURE)
                                .get(CaptureRequest.CONTROL_ZOOM_RATIO)?.let { set(CaptureRequest.CONTROL_ZOOM_RATIO, it) }
                    }.build()
                    repeat(r.request.shots.size) {
                        session.capture(built, exec, object : CameraExtensionSession.ExtensionCaptureCallback() {
                            override fun onCaptureSequenceCompleted(s: CameraExtensionSession, sequenceId: Int) {
                                if (current === r && r.active) jobs.shotTaken(r.id)
                            }
                            override fun onCaptureFailed(s: CameraExtensionSession, request: CaptureRequest) {
                                if (current === r && r.active) end(r, "failed", "The camera couldn't take the photo")
                            }
                        })
                    }
                } catch (e: Exception) {
                    onError("still.extension", e)
                    end(r, "failed", "The camera couldn't take the photo")
                }
            }
            override fun onConfigureFailed(session: CameraExtensionSession) {
                if (r.active) end(r, "unsupported", "The phone couldn't start its '${r.request.extension}' mode")
            }
            override fun onClosed(session: CameraExtensionSession) {
                if (r.active && r.extension === session) end(r, "interrupted", "The camera was busy with something else")
            }
        }
        try {
            device.createExtensionSession(ExtensionSessionConfiguration(type,
                listOf(OutputConfiguration(reader.surface)), exec, callback))
        } catch (e: Exception) {
            onError("still.extensionSession", e)
            end(r, "unsupported", "The phone couldn't start its '${r.request.extension}' mode")
        }
    }

    // ── The end ────────────────────────────────────────────────────────────

    // code null: every file is written. restore: the stream's session comes back (not when the stream is stopping)
    private fun end(r: Run, code: String?, text: String?, restore: Boolean = true) {
        if (!r.active) return
        r.active = false
        if (current === r) current = null
        r.images.values.flatten().forEach { (output, image) -> close(output, image) }
        r.images.clear()
        // Pictures of requests still in flight are thrown away as they come, so they can't fill a kept reader
        r.outputs?.list?.forEach { output ->
            output.reader.setOnImageAvailableListener({ discardWaiting(output) }, handler())
        }
        // A photo that failed part way stops asking for the rest, whose frames would also hold up the video
        val target = r.target
        if (code != null && target != null) {
            try { target.abortCaptures() } catch (_: Exception) {}
            // On the stream's session that also dropped the video's repeating request
            if (target !== r.session && restore) controller.stillRestartStream(target)
        }
        val own = r.session
        r.session = null
        try { own?.close() } catch (_: Exception) {}
        val ext = r.extension
        r.extension = null
        if (ext != null && Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) try { (ext as CameraExtensionSession).close() } catch (_: Exception) {}
        r.meter?.close()
        // Images still being written hold their reader's buffers, so the readers close after them
        val ownOutputs = r.own
        if (ownOutputs != null) writer.execute { ownOutputs.close() }
        if (r.paused && restore) controller.resumeAfterStill(r.generation)
        if (code == null) jobs.finish(r.id) else {
            android.util.Log.i(TAG, "Photo ${r.id} ended: $code")
            jobs.fail(r.id, code, text ?: code)
        }
        onBusy(false)
    }
}
