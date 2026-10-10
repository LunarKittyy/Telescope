package com.telescope

import android.content.Context
import android.graphics.ImageFormat
import android.graphics.Rect
import android.hardware.camera2.*
import android.hardware.camera2.params.ColorSpaceTransform
import android.hardware.camera2.params.MeteringRectangle
import android.hardware.camera2.params.OutputConfiguration
import android.hardware.camera2.params.RggbChannelVector
import android.hardware.camera2.params.SessionConfiguration
import android.media.ImageReader
import android.os.Build
import android.os.Handler
import android.os.HandlerThread
import android.os.Looper
import android.os.Message
import android.view.Surface
import java.util.concurrent.Executor

// Read-only snapshot of control state for /v1/state JSON response without exposing mutable fields
data class CameraControlSnapshot(
    val currentCamera:   CameraEntry?,
    val iso:              Int?,
    val shutterNs:        Long?,
    val ois:               Boolean,
    val wbGains:          RggbChannelVector?,
    val measuredGains:    RggbChannelVector?,
    val focusMode:        String,
    val focusDistance:    Float,
    val nrMode:            Int,
    val edgeMode:          Int,
    val aeComp:            Int,
    val blackLevelLock:    Boolean,
    val torch:             Boolean,
    val jpegQuality:       Int,
    val phoneFps:          Int,
    val streamWidth:       Int,
    val streamHeight:      Int,
    val codec:             String,
    val bitrate:           Int,
    val codecError:        String?,
    val codecUnsupported:  Boolean,  // codecError is H.264 not doing this size or rate here, not a crash
    val activeLens:        String?,
    val dynamicFps:        Int = 0,  // the rate Dynamic stepped down to for a slow link; 0 = the rate asked for
    val lowLight:          Boolean = false,
)

// Phone-side zoom, as the desktop splits it: a centred CONTROL_ZOOM_RATIO, then a 1/crop SCALER_CROP_REGION
// around (x, y) in 0..1 of that view. Kept normalized, so it carries over a lens switch, clamped to the new lens.
data class ZoomRequest(val ratio: Float = 1f, val crop: Float = 1f, val x: Float = 0.5f, val y: Float = 0.5f)

// Owns the Camera2 session lifecycle (device/session/reader) and control state; CameraStreamService owns the HTTP server, notification, and everything outside the camera itself. onFatalError tears the whole session down (camera/session lost); onControlError reports a single failed control change (e.g. exposure) while the stream keeps running on its previous request.
class CameraSessionController(
    private val context: Context,
    initialStreamWidth: Int,
    initialStreamHeight: Int,
    initialPhoneFps: Int = 30,
    private val onFrame: (ByteArray) -> Unit,
    private val onStateChanged: (StreamState, String, Throwable?) -> Unit,
    private val onFatalError: () -> Unit,
    private val onControlError: (String, Throwable) -> Unit = { _, _ -> },
    private val onH264: (bytes: ByteArray, key: Boolean, config: Boolean) -> Unit = { _, _, _ -> },
    // The encoder failed and the stream went back to MJPEG; the message says why.
    private val onCodecFailed: (String) -> Unit = {},
) {
    companion object {
        private const val TAG = "CameraSessionController"
        private const val REOPEN_TRIES = 8        // after the camera failed: up to about 4 s for it to come back
        private const val REOPEN_RETRY_MS = 500L
    }

    private var cameraDevice: CameraDevice? = null
    private var captureSession: CameraCaptureSession? = null
    private var imageReader: ImageReader? = null
    private var encoder: H264Encoder? = null
    private var handlerThread: HandlerThread? = null
    private var handler: Handler? = null
    @Volatile private var previewSurface: Surface? = null

    @Volatile private var currentIso:       Int?  = null  // null = auto AE
    @Volatile private var currentShutterNs: Long? = null  // null = auto AE
    @Volatile private var currentOis:       Boolean = true
    @Volatile private var currentWbGains: RggbChannelVector? = null  // null = auto AWB
    @Volatile private var lastCCM:        ColorSpaceTransform? = null
    @Volatile private var lastMeasuredGains: RggbChannelVector? = null
    // From capture results, for diagnostics: what the camera really did, as opposed to what it was asked for.
    private val frameDurations = ArrayDeque<Long>()  // SENSOR_FRAME_DURATION of the last frames, guarded by itself
    @Volatile private var appliedFpsRange: android.util.Range<Int>? = null
    @Volatile private var exposureNs: Long? = null
    @Volatile private var activeLens: String? = null  // which lens a multi-lens camera is using (capture results)
    @Volatile private var currentFocusMode:     String = "continuous"
    @Volatile private var currentFocusDistance: Float  = 0f  // diopters; 0 = infinity
    // Set in "point" mode: x, y and size in the stream frame; mapped onto the sensor per request, through the zoom.
    @Volatile private var focusPoint: FloatArray? = null
    @Volatile private var zoom = ZoomRequest()
    @Volatile private var currentNrMode:         Int     = CaptureRequest.NOISE_REDUCTION_MODE_FAST
    @Volatile private var currentEdgeMode:       Int     = CaptureRequest.EDGE_MODE_FAST
    @Volatile private var currentAeComp:         Int     = 0
    @Volatile private var currentBlackLevelLock: Boolean = false
    // Low light: auto exposure may slow the camera down (to CameraRequestSelection.LOW_LIGHT_MIN_FPS) and noise
    // reduction runs at its best, for a dark room
    @Volatile private var currentLowLight: Boolean = false
    @Volatile private var currentTorch:          Boolean = false
    @Volatile private var currentJpegQuality: Int = 85
    @Volatile private var currentPhoneFps:    Int = initialPhoneFps
    // Mutable so switchResolution() can change live; sized by openCamera().
    @Volatile private var streamWidth:  Int = initialStreamWidth
    @Volatile private var streamHeight: Int = initialStreamHeight
    @Volatile private var codec: String = H264Stream.CODEC_MJPEG
    @Volatile private var requestedBitrate: Int = 0  // 0 = sized from resolution and fps, H264Stream.DYNAMIC = dynamic
    @Volatile private var dynamic: DynamicBitrate? = null
    // Dynamic's frame rate step, alongside its bitrate: only while Dynamic runs the H.264 stream
    @Volatile private var frameRate: DynamicFrameRate? = null
    @Volatile private var codecError: String? = null
    @Volatile private var codecUnsupported = false

    @Volatile private var currentCamera: CameraEntry? = null

    // Set by stop(): work already queued on the camera thread (a switch, a reopen) must not reopen the camera.
    @Volatile private var stopped = false

    // Camera off: device and outputs closed, every setting kept, so turning it back on carries on where it was.
    @Volatile private var cameraOff = false
    // Turning back on: a failure now leaves the camera off rather than ending the stream (and the mic with it).
    @Volatile private var turningOn = false
    // Another app has the camera: waiting for it to be free again (cameraTaken).
    @Volatile private var taken = false
    // The camera has made frames for this stream at least once; a camera taken before then fails the start instead.
    @Volatile private var streamedOnce = false
    // Why the camera didn't come back on; cleared on the next try.
    @Volatile var cameraError: String? = null
        private set

    // Guards against stale onOpened/onConfigured callbacks after a new open.
    @Volatile private var cameraGeneration = 0

    // Guards against stale onConfigured callbacks from rapid reconfigureSession() calls.
    @Volatile private var sessionGeneration = 0

    fun getCurrentCameraId(): String? = currentCamera?.id
    fun isCameraOff(): Boolean = cameraOff
    fun isCameraTaken(): Boolean = taken
    fun getStreamSize(): android.util.Size = android.util.Size(streamWidth, streamHeight)

    // True while PreviewActivity has a surface attached - the idle watchdog must not stop this.
    fun hasPreviewSurface(): Boolean = previewSurface != null

    /** For diagnostics/logging only - the service logs this alongside every
     *  state transition it's told about via [onStateChanged]. */
    fun currentGeneration(): Int = cameraGeneration

    fun snapshot(): CameraControlSnapshot = CameraControlSnapshot(
        currentCamera   = currentCamera,
        iso             = currentIso,
        shutterNs       = currentShutterNs,
        ois             = currentOis,
        wbGains         = currentWbGains,
        measuredGains   = lastMeasuredGains,
        focusMode       = currentFocusMode,
        focusDistance   = currentFocusDistance,
        nrMode          = currentNrMode,
        edgeMode        = currentEdgeMode,
        aeComp          = currentAeComp,
        blackLevelLock  = currentBlackLevelLock,
        torch           = currentTorch,
        jpegQuality     = currentJpegQuality,
        phoneFps        = currentPhoneFps,
        streamWidth     = streamWidth,
        streamHeight    = streamHeight,
        codec           = codec,
        bitrate         = currentBitrate(),
        codecError      = codecError,
        codecUnsupported = codecUnsupported,
        activeLens      = activeLens,
        dynamicFps      = frameRate?.cap ?: 0,
        lowLight        = currentLowLight,
    )

    // The rate the camera and encoder run at: the one asked for, unless Dynamic stepped it down for a slow link.
    private fun streamFps(): Int = frameRate?.cap ?: currentPhoneFps

    private fun currentBitrate(): Int {
        if (requestedBitrate != H264Stream.DYNAMIC) {
            return H264Stream.bitrateFor(requestedBitrate, streamWidth, streamHeight, currentPhoneFps)
        }
        val ceiling = H264Stream.dynamicCeiling(streamWidth, streamHeight, streamFps(), H264Encoder.maxBitrate)
        // Kept across a new size or rate: the link is the same one.
        val d = dynamic?.also { it.rebound(ceiling) }
            ?: DynamicBitrate(H264Stream.defaultBitrate(streamWidth, streamHeight, currentPhoneFps), ceiling)
                .also { dynamic = it }
        return d.bitrate
    }

    // Which route the viewer connected to decides the codec; switching rebuilds the output (a reopen).
    fun setCodec(value: String) {
        post {
            if (value == codec) return@post
            codec = value
            codecError = null
            codecUnsupported = false
            frameRate = null  // the reopen below picks the rate up again
            reopen("setCodec")
        }
    }

    fun setBitrate(bps: Int) {
        if (bps == requestedBitrate) return
        if (bps == H264Stream.DYNAMIC) dynamic = null  // starts again from the default, like a new stream
        requestedBitrate = bps
        val capped = dropFrameRate()
        post { if (capped) reconfigureSession(); encoder?.setBitrate(currentBitrate()) }
    }

    // Back to the rate asked for; true if Dynamic had stepped it down, so the session needs the rate again.
    private fun dropFrameRate(): Boolean {
        val capped = frameRate?.cap != null
        frameRate = null
        return capped
    }

    /** How the link did lately (MjpegServer, on the encoder's thread); Dynamic moves the bitrate to match, and the
     *  frame rate too once the bitrate alone can't keep it sharp. */
    fun onLinkSample(sample: DynamicBitrate.Sample) {
        if (requestedBitrate != H264Stream.DYNAMIC || codec != H264Stream.CODEC_H264) return
        val d = dynamic ?: return
        d.update(sample)?.let { encoder?.setBitrate(it) }
        // A sample racing setFpsTarget could put back rungs for the rate asked before; those are rebuilt here
        val rate = frameRate?.takeIf { it.askedFps == currentPhoneFps } ?: newFrameRate().also { frameRate = it }
        val stepped = rate.update(sample.nowMs, d.bitrate, streamWidth, streamHeight, d.lastStallMs) ?: return
        android.util.Log.i(TAG, "Dynamic: ${stepped} fps for a ${d.bitrate / 1000} kbps link")
        // The rate is part of the session's setup (see createSession), so the session is rebuilt for it
        post { reconfigureSession(); encoder?.setBitrate(currentBitrate()) }
    }

    private fun newFrameRate(): DynamicFrameRate {
        val ranges = currentCamera?.aeFpsRanges?.map { it.lower to it.upper }
        return DynamicFrameRate(currentPhoneFps) { ranges == null || CameraRequestSelection.capsAt(ranges, it) }
    }

    fun requestKeyFrame() { encoder?.requestKeyFrame() }

    fun setIso(iso: Int)                    { currentIso = iso;                 post { applyExposure() } }
    fun setShutter(ns: Long)                { currentShutterNs = ns;            post { applyExposure() } }
    fun setAuto()                           { currentIso = null; currentShutterNs = null; post { applyExposure() } }
    fun setOis(on: Boolean)                 { currentOis = on;                  post { applyExposure() } }
    fun setWbGains(gains: RggbChannelVector) { currentWbGains = gains;          post { applyExposure() } }
    fun setWbAuto()                         { currentWbGains = null;            post { applyExposure() } }
    fun setJpegQuality(q: Int)              { currentJpegQuality = q;           post { applyExposure() } }
    fun setFpsTarget(fps: Int) {
        val changed = fps != currentPhoneFps || frameRate?.cap != null
        currentPhoneFps = fps
        frameRate = null  // a new rate asked for: Dynamic starts from it
        // A new rate goes in the session's own setup too (see createSession), so the session is rebuilt for it
        post { if (changed) reconfigureSession() else applyExposure(); encoder?.setBitrate(currentBitrate()) }
    }
    fun setFocusMode(mode: String)          { currentFocusMode = mode; focusPoint = null; post { applyExposure() } }
    fun setZoom(z: ZoomRequest)             { zoom = z;                         post { applyExposure() } }

    // Focus (and meter, when exposure is automatic) on a point of the stream frame. False if this lens
    // can't: no AF regions, no single-shot AF, or no known active array.
    fun setFocusPoint(x: Float, y: Float, size: Float): Boolean {
        val cam = currentCamera ?: return false
        if (cam.activeArray == null) return false
        if (cam.maxAfRegions <= 0 || CaptureRequest.CONTROL_AF_MODE_AUTO !in cam.afModes) return false
        focusPoint = floatArrayOf(x, y, size)
        currentFocusMode = "point"
        post { triggerFocus() }
        return true
    }

    // AUTO mode holds focus once it locks, so the repeating request keeps it; the trigger starts the sweep.
    private fun triggerFocus() {
        val s = captureSession ?: return
        val c = cameraDevice ?: return
        try {
            s.setRepeatingRequest(buildRequest(c), ccmCaptureCallback, handler)
            val trigger = c.createCaptureRequest(requestTemplate()).apply {
                addTarget(outputSurface()!!)
                previewSurface?.let { addTarget(it) }
                currentCamera?.let { applyZoom(this, it) }  // this frame reaches the stream too
                applyFocusRegion(this)
                set(CaptureRequest.CONTROL_AF_TRIGGER, CaptureRequest.CONTROL_AF_TRIGGER_START)
            }.build()
            s.capture(trigger, null, handler)
        } catch (e: Exception) {
            onControlError("triggerFocus", e)
        }
    }

    private fun applyFocusRegion(builder: CaptureRequest.Builder): Boolean {
        val point = focusPoint ?: return false
        val cam = currentCamera ?: return false
        val area = zoomCrop(cam) ?: cam.activeArray ?: return false
        val region = CameraRequestSelection.meteringRect(point[0], point[1], point[2], area, streamWidth, streamHeight)
        val rect = MeteringRectangle(region[0], region[1], region[2], region[3], MeteringRectangle.METERING_WEIGHT_MAX)
        builder.set(CaptureRequest.CONTROL_MODE, CaptureRequest.CONTROL_MODE_AUTO)
        builder.set(CaptureRequest.CONTROL_AF_MODE, CaptureRequest.CONTROL_AF_MODE_AUTO)
        builder.set(CaptureRequest.CONTROL_AF_REGIONS, arrayOf(rect))
        if (cam.maxAeRegions > 0 && currentIso == null) builder.set(CaptureRequest.CONTROL_AE_REGIONS, arrayOf(rect))
        return true
    }
    fun setFocusDistance(d: Float)          { currentFocusDistance = d;         post { applyExposure() } }
    fun setNrMode(m: Int)                   { currentNrMode = m;                post { applyExposure() } }
    fun setLowLight(on: Boolean) {
        if (on == currentLowLight) return
        currentLowLight = on
        // The frame rate range is part of the session's setup (see createSession), so the session is rebuilt for it
        post { reconfigureSession() }
    }
    fun setEdgeMode(m: Int)                 { currentEdgeMode = m;              post { applyExposure() } }
    fun setAeComp(v: Int)                   { currentAeComp = v;                post { applyExposure() } }
    fun setBlackLevelLock(on: Boolean)      { currentBlackLevelLock = on;       post { applyExposure() } }
    fun setTorch(on: Boolean)               { currentTorch = on;                post { applyExposure() } }

    fun open(cameraId: String, physicalCameraId: String?, initialEntry: CameraEntry, initialOis: Boolean) {
        currentCamera = initialEntry
        currentOis = initialOis
        openCamera(cameraId, physicalCameraId)
    }

    // A stream that starts with the camera off: nothing opens until setCameraOn(true).
    fun openOff(initialEntry: CameraEntry, initialOis: Boolean) {
        currentCamera = initialEntry
        currentOis = initialOis
        cameraOff = true
        startThread()
    }

    fun setCameraOn(on: Boolean) {
        post { if (on) turnCameraOn() else turnCameraOff() }
    }

    private fun turnCameraOff() {
        if (cameraOff) return
        cameraOff = true
        turningOn = false
        taken = false  // off on purpose: no longer waiting for it
        unwatchAvailability()
        cameraError = null
        teardown()  // also drops a lens switch or reopen still opening
        onStateChanged(StreamState.Streaming, "cameraOff", null)
    }

    private fun turnCameraOn() {
        if (!cameraOff) return
        cameraOff = false
        turningOn = true
        cameraError = null
        reopen("cameraOn")
    }

    // A camera that won't come back on stays off with the stream carrying on; any other failure ends the stream.
    private fun fail(op: String, e: Throwable?) {
        if (turningOn) {
            turningOn = false
            cameraOff = true
            teardown()
            cameraError = "The camera didn't turn back on. Another app may be using it."
            onControlError(op, e ?: IllegalStateException(op))
            onStateChanged(StreamState.Streaming, "$op.cameraStaysOff", null)
            return
        }
        onStateChanged(StreamState.Failed, op, e)
        onFatalError()
    }

    // Android hands the camera to the app on screen, so another app opening it takes it from this one.
    private fun takenBy(error: Int): Boolean =
        error == CameraDevice.StateCallback.ERROR_CAMERA_IN_USE || error == CameraDevice.StateCallback.ERROR_MAX_CAMERAS_IN_USE

    private fun takenBy(e: Exception): Boolean = e is CameraAccessException &&
        (e.reason == CameraAccessException.CAMERA_IN_USE || e.reason == CameraAccessException.MAX_CAMERAS_IN_USE)

    // Another app took the camera mid-stream: the stream (and the mic) carries on without it, and the camera opens
    // again once it's free. Before the first frame, or while turning back on, it fails as before.
    private fun cameraTaken(op: String, e: Throwable?) {
        if (turningOn || cameraOff || !streamedOnce) { fail(op, e); return }
        teardown()
        if (!taken) onControlError(op, e ?: IllegalStateException("another app took the camera"))
        taken = true
        // Streaming, not Recovering, also after a reopen that found it still taken: the busy watchdog would end it
        onStateChanged(StreamState.Streaming, "$op.cameraTaken", null)
        watchAvailability()
    }

    @Volatile private var availability: CameraManager.AvailabilityCallback? = null

    private fun watchAvailability() {
        if (availability != null || stopped) return
        val callback = object : CameraManager.AvailabilityCallback() {
            override fun onCameraAvailable(cameraId: String) {
                val cam = currentCamera ?: return
                if (stopped || !taken || cameraOff || cameraId != (cam.logicalId ?: cam.id)) return
                unwatchAvailability()
                // A moment's wait, so a camera listed free that still won't open doesn't make a tight loop of tries
                handler?.postDelayed({
                    if (!stopped && taken && !cameraOff) reopen("cameraFree")  // taken again if it isn't free after all
                }, REOPEN_RETRY_MS)
            }
        }
        availability = callback
        try {
            (context.getSystemService(Context.CAMERA_SERVICE) as CameraManager).registerAvailabilityCallback(callback, handler)
        } catch (e: Exception) {
            availability = null
            onControlError("watchAvailability", e)
        }
    }

    private fun unwatchAvailability() {
        val callback = availability ?: return
        availability = null
        try {
            (context.getSystemService(Context.CAMERA_SERVICE) as CameraManager).unregisterAvailabilityCallback(callback)
        } catch (_: Exception) {}
    }

    // Only the newest of a burst of lens or size changes runs: each one is a full close and reopen of the camera.
    @Volatile private var wantedLens: CameraEntry? = null
    @Volatile private var wantedSize: Pair<Int, Int>? = null

    fun switchTo(entry: CameraEntry) {
        // Asked again for the lens it's on or going to (the computer resends when a bad link lost the reply): no reopen
        if ((wantedLens ?: currentCamera) == entry) return
        wantedLens = entry
        post { if (wantedLens === entry) switchCameraTo(entry) }
    }

    // Adds extra output surface for live preview without interrupting MJPEG stream
    fun attachPreviewSurface(surface: Surface) {
        post { previewSurface = surface; reconfigureSession() }
    }

    // onDetached fires after surface is dropped and session rebuilt
    fun detachPreviewSurface(onDetached: (() -> Unit)? = null) {
        val h = handler
        val posted = !stopped && h != null && h.post {
            previewSurface = null
            if (stopped) onDetached?.invoke() else reconfigureSession(onDetached)
        }
        if (!posted) { previewSurface = null; onDetached?.invoke() }
    }

    // Everything on the camera thread runs through here: an uncaught throw ends the stream, recorded, not the whole app.
    private inner class SafeHandler(looper: Looper) : Handler(looper) {
        override fun dispatchMessage(msg: Message) {
            try {
                super.dispatchMessage(msg)
            } catch (e: Exception) {
                if (stopped) return
                fail("cameraThread", e)
            }
        }
    }

    // A dead preview surface (Preview closing as the session is rebuilt) mustn't end the stream: rebuild once without it.
    private fun retryWithoutPreview(op: String, e: Throwable?, onComplete: (() -> Unit)?): Boolean {
        if (previewSurface == null) return false
        previewSurface = null
        onControlError("$op.previewDropped", e ?: IllegalStateException("configure failed"))
        reconfigureSession(onComplete)
        return true
    }

    // Runs block on the camera thread unless stop() came first.
    private fun post(block: () -> Unit) {
        handler?.post { if (!stopped) block() }
    }

    // Tears down camera/session/reader on the camera thread, after any frame it is copying; caller handles service-level cleanup
    fun stop() {
        stopped = true
        unwatchAvailability()
        val h = handler
        if (h == null || !h.post { teardown(); handlerThread?.quitSafely() }) teardown()
    }

    private fun teardown() {
        // Invalidate in-flight callbacks so they can't resurrect stale camera/session with dead surfaces
        cameraGeneration++
        try { captureSession?.stopRepeating() } catch (_: Exception) {}
        try { captureSession?.close()         } catch (_: Exception) {}
        try { cameraDevice?.close()           } catch (_: Exception) {}
        releaseOutputs()
        captureSession = null; cameraDevice = null
    }

    // The camera's stream output at the current size: the JPEG reader, or the H.264 encoder's
    // Surface. An encoder that won't start falls back to JPEG and reports it.
    private fun buildOutputs() {
        if (codec == H264Stream.CODEC_H264) {
            try {
                encoder = H264Encoder(streamWidth, streamHeight, streamFps(), currentBitrate(),
                    onPacket = onH264, onError = { e -> post { encoderFailed(e) } })
                return
            } catch (e: Exception) {
                codec = H264Stream.CODEC_MJPEG
                frameRate = null  // Dynamic's step was for H.264; JPEG goes back to the rate asked for
                codecError = "H.264 isn't available at ${streamWidth}x$streamHeight on this phone"
                codecUnsupported = true
                onControlError("h264Encoder", e)
                onCodecFailed(codecError!!)
            }
        }
        imageReader = buildImageReader()
    }

    // unsupported: it can't do this size or rate here (so trying again won't help), rather than a crash.
    // reopenTries: after the camera itself failed, it can take a moment before it can be opened again.
    private fun encoderFailed(e: Throwable, reason: String = "The phone's H.264 encoder stopped",
                              unsupported: Boolean = false, reopenTries: Int = 0) {
        if (codec != H264Stream.CODEC_H264) return
        codec = H264Stream.CODEC_MJPEG
        frameRate = null  // Dynamic's step was for H.264, and it can't step back up on JPEG
        codecError = reason
        codecUnsupported = unsupported
        onControlError("h264Encoder", e)
        onCodecFailed(codecError!!)
        reopen("encoderFailed", reopenTries)
    }

    private fun releaseOutputs() {
        try { imageReader?.close() } catch (_: Exception) {}
        encoder?.release()
        imageReader = null; encoder = null
    }

    private fun outputSurface(): Surface? = encoder?.inputSurface ?: imageReader?.surface

    private fun buildImageReader(): ImageReader {
        val reader = ImageReader.newInstance(streamWidth, streamHeight, ImageFormat.JPEG, 3)
        reader.setOnImageAvailableListener({ r ->
            val image = r.acquireLatestImage() ?: return@setOnImageAvailableListener
            try {
                val buf   = image.planes[0].buffer
                val bytes = ByteArray(buf.remaining())
                buf.get(bytes)
                onFrame(bytes)
            } catch (e: Exception) {
                if (!stopped) onControlError("frame", e)  // one frame lost; the reader is being rebuilt or closed
            } finally { image.close() }
        }, handler)
        return reader
    }

    private fun startThread() {
        handlerThread = HandlerThread("CamThread").also { it.start() }
        handler       = SafeHandler(handlerThread!!.looper)
    }

    private fun openCamera(openCameraId: String, physicalCameraId: String?) {
        startThread()
        buildOutputs()

        val myGeneration = ++cameraGeneration
        val manager = context.getSystemService(Context.CAMERA_SERVICE) as CameraManager
        try {
            @Suppress("MissingPermission")
            manager.openCamera(openCameraId, object : CameraDevice.StateCallback() {
                override fun onOpened(camera: CameraDevice) {
                    if (myGeneration != cameraGeneration) { camera.close(); return }
                    cameraDevice = camera
                    onStateChanged(StreamState.ConfiguringSession, "openCamera.onOpened", null)
                    createSession(camera, physicalCameraId, myGeneration)
                }
                override fun onDisconnected(camera: CameraDevice) {
                    camera.close()
                    if (myGeneration == cameraGeneration) {
                        cameraDevice = null
                        cameraTaken("openCamera.onDisconnected", null)
                    }
                }
                override fun onError(camera: CameraDevice, error: Int) {
                    camera.close()
                    if (myGeneration == cameraGeneration) {
                        cameraDevice = null
                        val e = RuntimeException("Camera2 error code $error")
                        if (takenBy(error)) cameraTaken("openCamera.onError", e) else fail("openCamera.onError", e)
                    }
                }
            }, handler)
        } catch (e: Exception) {
            if (takenBy(e)) cameraTaken("openCamera", e) else fail("openCamera", e)
        }
    }

    // Returns true when it has taken over (onComplete is then someone else's to call)
    private fun sessionFailed(op: String, e: Throwable?, onComplete: (() -> Unit)?): Boolean {
        if (usedStreamUseCase && !streamUseCaseRefused) {
            // The labels are only a hint: a camera that won't take them gets the session without, from now on
            streamUseCaseRefused = true
            onControlError("$op.streamUseCaseDropped", e ?: IllegalStateException("configure failed"))
            reconfigureSession(onComplete)
            return true
        }
        if (retryWithoutPreview(op, e, onComplete)) return true
        if (codec == H264Stream.CODEC_H264) {
            // The camera can't feed the encoder at this size: MJPEG instead, and codecError stops the desktop retrying
            encoderFailed(e ?: IllegalStateException("$op: configure failed"),
                "The camera can't feed H.264 at ${streamWidth}x$streamHeight on this phone", unsupported = true)
            return false
        }
        fail(op, e)
        return false
    }

    // A session without the stream's output would sit in Recovering until the watchdog; fail it now instead
    private fun noOutput(op: String, onComplete: (() -> Unit)?): Boolean {
        if (outputSurface() != null) return false
        fail(op, IllegalStateException("no stream output"))
        onComplete?.invoke()
        return true
    }

    private fun currentTargetSurfaces(): List<Surface> = listOfNotNull(outputSurface(), previewSurface)

    // physId: a lens of a logical camera, streamed on its own; null for the camera as opened.
    private fun createSession(
        camera: CameraDevice, physId: String?,
        generation: Int = cameraGeneration,
        mySession: Int = sessionGeneration,
        onComplete: (() -> Unit)? = null,
    ) {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.P) { createLegacySession(camera, generation, mySession, onComplete); return }
        // Re-check staleness; surfaces may have been cleared concurrently.
        if (generation != cameraGeneration) { onComplete?.invoke(); return }
        val exec   = Executor { cmd -> handler?.post(cmd) }
        try {
            if (noOutput("createSession", onComplete)) return
            usedStreamUseCase = false
            val outCfgs = currentTargetSurfaces().map { surface ->
                OutputConfiguration(surface).also { cfg ->
                    physId?.let { cfg.setPhysicalCameraId(it) }
                    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU && !streamUseCaseRefused)
                        applyStreamUseCase(cfg, surface)
                }
            }
            camera.createCaptureSession(SessionConfiguration(
                SessionConfiguration.SESSION_REGULAR, outCfgs, exec,
                object : CameraCaptureSession.StateCallback() {
                    override fun onConfigured(s: CameraCaptureSession) {
                        if (generation != cameraGeneration) { s.close(); onComplete?.invoke(); return }
                        if (mySession == sessionGeneration) { captureSession = s; startRepeating(camera, s) }
                        else s.close()
                        onComplete?.invoke()
                    }
                    override fun onConfigureFailed(s: CameraCaptureSession) {
                        if (generation == cameraGeneration) {
                            if (sessionFailed("createSession.onConfigureFailed", null, onComplete)) return
                        }
                        onComplete?.invoke()
                    }
                }
            ).apply {
                // The frame rate is picked when the session is set up: without it here, a phone can choose a
                // sensor mode that never reaches the rate asked for later (a vivo stuck near 42 at 60 fps).
                setSessionParameters(buildRequest(camera))
            })
        } catch (e: Exception) {
            if (generation == cameraGeneration) {
                if (sessionFailed("createSession", e, onComplete)) return
            }
            onComplete?.invoke()
        }
    }

    @Suppress("DEPRECATION")
    private fun createLegacySession(
        camera: CameraDevice,
        generation: Int = cameraGeneration,
        mySession: Int = sessionGeneration,
        onComplete: (() -> Unit)? = null,
    ) {
        if (generation != cameraGeneration) { onComplete?.invoke(); return }
        if (noOutput("createLegacySession", onComplete)) return
        val targets = currentTargetSurfaces()
        try {
            camera.createCaptureSession(targets,
                object : CameraCaptureSession.StateCallback() {
                    override fun onConfigured(s: CameraCaptureSession) {
                        if (generation != cameraGeneration) { s.close(); onComplete?.invoke(); return }
                        if (mySession == sessionGeneration) { captureSession = s; startRepeating(camera, s) }
                        else s.close()
                        onComplete?.invoke()
                    }
                    override fun onConfigureFailed(s: CameraCaptureSession) {
                        if (generation == cameraGeneration) {
                            if (sessionFailed("createLegacySession.onConfigureFailed", null, onComplete)) return
                        }
                        onComplete?.invoke()
                    }
                }, handler)
        } catch (e: Exception) {
            if (generation == cameraGeneration) {
                if (sessionFailed("createLegacySession", e, onComplete)) return
            }
            onComplete?.invoke()
        }
    }

    // Rebuilds capture session without reopening device; onComplete fires when terminal state reached
    private fun reconfigureSession(onComplete: (() -> Unit)? = null) {
        val camera = cameraDevice ?: run { onComplete?.invoke(); return }
        try { captureSession?.stopRepeating() } catch (_: Exception) {}
        try { captureSession?.close() } catch (_: Exception) {}
        captureSession = null

        val mySession = ++sessionGeneration
        val cam    = currentCamera
        val physId = if (cam?.logicalId != null) cam.id else null
        createSession(camera, physId, cameraGeneration, mySession, onComplete)
    }

    private fun startRepeating(camera: CameraDevice, session: CameraCaptureSession) {
        try {
            session.setRepeatingRequest(buildRequest(camera), ccmCaptureCallback, handler)
            turningOn = false
            streamedOnce = true
            if (taken) { taken = false; unwatchAvailability() }
            // Only after Camera2 accepts the repeating request are frames guaranteed en route
            onStateChanged(StreamState.Streaming, "startRepeating", null)
        } catch (e: Exception) {  // also IllegalStateException for a session closed meanwhile
            if (stopped) return
            fail("startRepeating", e)
        }
    }

    // What the stream shows under the current zoom, in request coordinates; null = the whole frame. With a
    // zoom ratio set, Android measures crop and metering regions in the zoomed view, so both use the array.
    private fun zoomCrop(cam: CameraEntry): SensorBox? {
        val array = cam.activeArray ?: return null
        val z = zoom
        val crop = CameraRequestSelection.clamp(z.crop, 1f, cam.cropZoomMax)
        if (crop <= 1f) return null
        val (x, y) = if (cam.freeformCrop) z.x to z.y else 0.5f to 0.5f
        return CameraRequestSelection.cropRect(array, crop, x, y, streamWidth, streamHeight)
    }

    private fun applyZoom(builder: CaptureRequest.Builder, cam: CameraEntry) {
        val ratio = CameraRequestSelection.clamp(zoom.ratio, 1f, cam.zoomRatioMax)
        if (ratio > 1f && Build.VERSION.SDK_INT >= Build.VERSION_CODES.R)
            builder.set(CaptureRequest.CONTROL_ZOOM_RATIO, ratio)
        val box = zoomCrop(cam) ?: return
        val rect = Rect(box.left, box.top, box.left + box.width, box.top + box.height)
        if (cam.logicalId != null && Build.VERSION.SDK_INT >= Build.VERSION_CODES.P)
            builder.setPhysicalCameraKey(CaptureRequest.SCALER_CROP_REGION, rect, cam.id)  // a lens of a logical camera
        else
            builder.set(CaptureRequest.SCALER_CROP_REGION, rect)
    }

    // Labels each output with what it's for, on cameras that take that: some (a vivo) only run 60 fps for a
    // stream marked as video recording, and otherwise stay at 30 whatever the request asks.
    @Volatile private var usedStreamUseCase = false
    @Volatile private var streamUseCaseRefused = false  // the session failed with the labels: sessions go without

    @androidx.annotation.RequiresApi(Build.VERSION_CODES.TIRAMISU)
    private fun applyStreamUseCase(cfg: OutputConfiguration, surface: Surface) {
        val useCase = when {
            encoder != null && surface === encoder?.inputSurface ->
                CameraMetadata.SCALER_AVAILABLE_STREAM_USE_CASES_VIDEO_RECORD.toLong()
            surface === previewSurface -> CameraMetadata.SCALER_AVAILABLE_STREAM_USE_CASES_PREVIEW.toLong()
            else -> return
        }
        val cam = currentCamera ?: return
        val offered = try {
            (context.getSystemService(Context.CAMERA_SERVICE) as CameraManager)
                .getCameraCharacteristics(cam.logicalId ?: cam.id)
                .get(CameraCharacteristics.SCALER_AVAILABLE_STREAM_USE_CASES)
        } catch (_: Exception) { null } ?: return
        if (useCase !in offered) return
        cfg.streamUseCase = useCase
        usedStreamUseCase = true
    }

    // For Copy diagnostics: the rate asked for against what the camera did, and the fastest each output allows here.
    fun frameReport(): String = buildString {
        val cam = currentCamera
        val asked = CameraRequestSelection.pickAeFpsRange(cam?.aeFpsRanges ?: emptyList(), streamFps(), currentLowLight)
        val template = if (requestTemplate() == CameraDevice.TEMPLATE_RECORD) "record" else "preview"
        val labels = when {
            usedStreamUseCase -> ", outputs labelled"
            streamUseCaseRefused -> ", output labels refused"
            else -> ""
        }
        val stepped = frameRate?.cap?.let { ", Dynamic stepped down to $it for the link" } ?: ""
        val dark = if (currentLowLight) ", low light" else ""
        appendLine("FPS asked for: $currentPhoneFps$stepped (range $asked$dark, $template template, $codec$labels)")
        val durations = synchronized(frameDurations) { frameDurations.toList() }
        fun ms(ns: Long) = "%.1f".format(java.util.Locale.ROOT, ns / 1e6)
        if (durations.isNotEmpty()) {
            append("Camera did: frame time ${ms(durations.average().toLong())} ms (${ms(durations.min())}-${ms(durations.max())})")
            append(", range ${appliedFpsRange ?: "?"}")
            exposureNs?.let { append(", exposure ${ms(it)} ms") }
            appendLine()
        }
        val map = try {
            (context.getSystemService(Context.CAMERA_SERVICE) as CameraManager)
                .getCameraCharacteristics(cam?.logicalId ?: cam?.id ?: return@buildString)
                .get(CameraCharacteristics.SCALER_STREAM_CONFIGURATION_MAP)
        } catch (_: Exception) { null } ?: return@buildString
        val size = android.util.Size(streamWidth, streamHeight)
        val fastest = try {
            if (encoder != null) map.getOutputMinFrameDuration(android.media.MediaCodec::class.java, size)
            else map.getOutputMinFrameDuration(ImageFormat.JPEG, size)
        } catch (_: Exception) { 0L }
        append("Outputs: stream ${streamWidth}x$streamHeight fastest ${if (fastest > 0) ms(fastest) + " ms" else "?"}")
        appendLine(if (previewSurface != null) ", plus the phone's preview" else "")
    }

    // H.264 feeds an encoder, so it asks as a video recording: phones like vivo's only run their 60 fps sensor modes
    // for that, and stay at 30 for a preview. MJPEG's JPEG output stays a preview, where JPEG is a normal target.
    private fun requestTemplate(): Int =
        if (codec == H264Stream.CODEC_H264) CameraDevice.TEMPLATE_RECORD else CameraDevice.TEMPLATE_PREVIEW

    private fun buildRequest(camera: CameraDevice = cameraDevice!!): CaptureRequest {
        return camera.createCaptureRequest(requestTemplate()).apply {
            addTarget(outputSurface()!!)
            previewSurface?.let { addTarget(it) }

            val cam = currentCamera
            if (cam != null) applyZoom(this, cam)

            // Use CONTROL_MODE_AUTO even in manual AE so AF keeps running independently
            set(CaptureRequest.CONTROL_MODE, CaptureRequest.CONTROL_MODE_AUTO)
            // A recording template may turn stabilisation on, which crops: framing and zoom here assume the full view
            set(CaptureRequest.CONTROL_VIDEO_STABILIZATION_MODE, CaptureRequest.CONTROL_VIDEO_STABILIZATION_MODE_OFF)
            // Set in manual exposure too, where AE ignores it: it's also what the session is set up for.
            // Unsupported ranges can fail on some devices; use advertised range
            val fps = streamFps()
            CameraRequestSelection.pickAeFpsRange(cam?.aeFpsRanges ?: emptyList(), fps, currentLowLight)?.let { range ->
                android.util.Log.d(TAG, "AE FPS range for ${cam?.id}: $range (target=$fps)")
                set(CaptureRequest.CONTROL_AE_TARGET_FPS_RANGE, range)
            }
            if (currentIso != null && currentShutterNs != null && cam != null && cam.supportsManualSensor) {
                val iso = CameraRequestSelection.clamp(currentIso!!, cam.isoMin, cam.isoMax)
                val sht = CameraRequestSelection.clamp(currentShutterNs!!, cam.shutterMinNs, cam.shutterMaxNs)
                set(CaptureRequest.CONTROL_AE_MODE, CaptureRequest.CONTROL_AE_MODE_OFF)
                set(CaptureRequest.SENSOR_SENSITIVITY,   iso)
                set(CaptureRequest.SENSOR_EXPOSURE_TIME, sht)
                val targetFrameNs = 1_000_000_000L / fps
                set(CaptureRequest.SENSOR_FRAME_DURATION, targetFrameNs.coerceAtLeast(sht))
            } else {
                set(CaptureRequest.CONTROL_AE_MODE, CaptureRequest.CONTROL_AE_MODE_ON)
            }

            if (currentFocusMode == "point" && applyFocusRegion(this)) {
                // AF_MODE_AUTO with the picked region; set by applyFocusRegion
            } else if (currentFocusMode == "manual" && cam != null && cam.supportsManualFocus) {
                set(CaptureRequest.CONTROL_AF_MODE, CaptureRequest.CONTROL_AF_MODE_OFF)
                set(CaptureRequest.LENS_FOCUS_DISTANCE,
                    CameraRequestSelection.clamp(currentFocusDistance, 0f, cam.minFocusDistance))
            } else {
                set(CaptureRequest.CONTROL_AF_MODE,
                    CameraRequestSelection.pickAfMode(cam?.afModes ?: emptySet(), wantContinuousVideo = true))
            }

            set(CaptureRequest.JPEG_QUALITY, currentJpegQuality.toByte())

            // White balance - desktop sends pre-computed RGGB gains
            val gains = currentWbGains
            if (gains != null && cam?.supportsManualWB == true) {
                set(CaptureRequest.CONTROL_AWB_MODE, CaptureRequest.CONTROL_AWB_MODE_OFF)
                set(CaptureRequest.COLOR_CORRECTION_MODE,
                    CameraMetadata.COLOR_CORRECTION_MODE_TRANSFORM_MATRIX)
                set(CaptureRequest.COLOR_CORRECTION_GAINS, gains)
                lastCCM?.let { set(CaptureRequest.COLOR_CORRECTION_TRANSFORM, it) }
            } else {
                set(CaptureRequest.CONTROL_AWB_MODE, CaptureRequest.CONTROL_AWB_MODE_AUTO)
                set(CaptureRequest.COLOR_CORRECTION_MODE, CameraMetadata.COLOR_CORRECTION_MODE_FAST)
            }

            // OIS - only ever requested ON if this camera actually advertises it.
            set(CaptureRequest.LENS_OPTICAL_STABILIZATION_MODE,
                if (currentOis && cam?.hasOis == true) CaptureRequest.LENS_OPTICAL_STABILIZATION_MODE_ON
                else CaptureRequest.LENS_OPTICAL_STABILIZATION_MODE_OFF)

            // NR/edge modes set only if camera advertises them; omit otherwise.
            val nr = if (currentLowLight) CaptureRequest.NOISE_REDUCTION_MODE_HIGH_QUALITY else currentNrMode
            CameraRequestSelection.pickNrMode(cam?.nrModes ?: emptySet(), nr)?.let {
                set(CaptureRequest.NOISE_REDUCTION_MODE, it)
            }
            CameraRequestSelection.pickEdgeMode(cam?.edgeModes ?: emptySet(), currentEdgeMode)?.let {
                set(CaptureRequest.EDGE_MODE, it)
            }

            // AE exposure compensation (only meaningful in auto AE)
            if (currentIso == null) {
                val comp = if (cam != null) CameraRequestSelection.clamp(currentAeComp, cam.aeCompMin, cam.aeCompMax)
                           else currentAeComp
                set(CaptureRequest.CONTROL_AE_EXPOSURE_COMPENSATION, comp)
            }

            set(CaptureRequest.BLACK_LEVEL_LOCK, currentBlackLevelLock)

            // Torch - only if this camera actually has a flash unit.
            set(CaptureRequest.FLASH_MODE,
                if (currentTorch && cam?.supportsFlash == true) CaptureRequest.FLASH_MODE_TORCH
                else CaptureRequest.FLASH_MODE_OFF)

        }.build()
    }

    private val ccmCaptureCallback = object : CameraCaptureSession.CaptureCallback() {
        override fun onCaptureCompleted(
            session: CameraCaptureSession,
            request: CaptureRequest,
            result: TotalCaptureResult
        ) {
            result.get(CaptureResult.COLOR_CORRECTION_TRANSFORM)?.let { lastCCM = it }
            result.get(CaptureResult.COLOR_CORRECTION_GAINS)?.let { lastMeasuredGains = it }
            result.get(CaptureResult.SENSOR_FRAME_DURATION)?.let { ns ->
                synchronized(frameDurations) {
                    frameDurations.addLast(ns)
                    if (frameDurations.size > 60) frameDurations.removeFirst()
                }
            }
            appliedFpsRange = result.get(CaptureResult.CONTROL_AE_TARGET_FPS_RANGE)
            exposureNs = result.get(CaptureResult.SENSOR_EXPOSURE_TIME)
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q)
                activeLens = result.get(CaptureResult.LOGICAL_MULTI_CAMERA_ACTIVE_PHYSICAL_ID)
        }
    }

    private fun applyExposure() {
        val s = captureSession ?: return
        val c = cameraDevice  ?: return
        try {
            s.setRepeatingRequest(buildRequest(c), ccmCaptureCallback, handler)
        } catch (e: Exception) {
            // Non-fatal; stream continues on previous request but control change failed.
            onControlError("applyExposure", e)
        }
    }

    private fun switchCameraTo(entry: CameraEntry) {
        // Drop unsupported manual/flash/focus; preserve OIS toggle for auto-resume.
        if (!entry.supportsFlash) currentTorch = false
        if (!entry.supportsManualSensor) { currentIso = null; currentShutterNs = null }
        if (!entry.supportsManualFocus && currentFocusMode == "manual") currentFocusMode = "continuous"
        if (currentFocusMode == "point") { currentFocusMode = "continuous"; focusPoint = null }  // another sensor
        dropFrameRate()  // its rungs came from the old lens's ranges; the next link sample builds the new lens's
        currentCamera = entry
        if (cameraOff) return  // the lens it opens with when the camera comes back on
        onStateChanged(StreamState.Recovering, "switchCameraTo", null)

        val myGeneration = ++cameraGeneration
        try { captureSession?.close() } catch (_: Exception) {}
        try { cameraDevice?.close()   } catch (_: Exception) {}
        captureSession = null; cameraDevice = null

        val openId = entry.logicalId ?: entry.id
        val physId = if (entry.logicalId != null) entry.id else null
        val manager = context.getSystemService(Context.CAMERA_SERVICE) as CameraManager
        try {
            @Suppress("MissingPermission")
            manager.openCamera(openId, object : CameraDevice.StateCallback() {
                override fun onOpened(camera: CameraDevice) {
                    if (myGeneration != cameraGeneration) { camera.close(); return }
                    cameraDevice = camera
                    if (outputSurface() == null) buildOutputs()  // a reopen this switch overtook had released them
                    createSession(camera, physId, myGeneration)
                }
                override fun onDisconnected(camera: CameraDevice) {
                    camera.close()
                    if (myGeneration == cameraGeneration) {
                        cameraDevice = null
                        cameraTaken("switchCameraTo.onDisconnected", null)
                    }
                }
                override fun onError(camera: CameraDevice, error: Int) {
                    camera.close()
                    if (myGeneration == cameraGeneration) {
                        cameraDevice = null
                        val e = RuntimeException("Camera2 error code $error")
                        if (takenBy(error)) cameraTaken("switchCameraTo.onError", e) else fail("switchCameraTo.onError", e)
                    }
                }
            }, handler)
        } catch (e: Exception) {
            if (takenBy(e)) cameraTaken("switchCameraTo", e) else fail("switchCameraTo", e)
        }
    }

    // Live size change: lens stays same but ImageReader must be rebuilt (requires device reopen)
    fun switchResolution(width: Int, height: Int) {
        val size = width to height
        wantedSize = size
        post { if (wantedSize === size) switchResolutionInternal(width, height) }
    }

    private fun switchResolutionInternal(width: Int, height: Int) {
        if (width == streamWidth && height == streamHeight) return
        streamWidth = width
        streamHeight = height
        reopen("switchResolution")
    }

    // Same lens, new output (size or codec): the reader or encoder is rebuilt, which needs a reopen.
    // tries: how many more times to try, REOPEN_RETRY_MS apart, while the camera can't be opened yet.
    private fun reopen(op: String, tries: Int = 0) {
        if (cameraOff) return  // a new size or codec is simply what it opens with later
        onStateChanged(StreamState.Recovering, op, null)

        val myGeneration = ++cameraGeneration
        try { captureSession?.stopRepeating() } catch (_: Exception) {}
        try { captureSession?.close() } catch (_: Exception) {}
        try { cameraDevice?.close()   } catch (_: Exception) {}
        releaseOutputs()
        captureSession = null; cameraDevice = null

        val cam = currentCamera ?: run {
            fail(op, IllegalStateException("no current camera"))
            return
        }
        val openId = cam.logicalId ?: cam.id
        val physId = if (cam.logicalId != null) cam.id else null
        val manager = context.getSystemService(Context.CAMERA_SERVICE) as CameraManager
        try {
            @Suppress("MissingPermission")
            manager.openCamera(openId, object : CameraDevice.StateCallback() {
                override fun onOpened(camera: CameraDevice) {
                    if (myGeneration != cameraGeneration) { camera.close(); return }
                    cameraDevice = camera
                    buildOutputs()
                    createSession(camera, physId, myGeneration)
                }
                override fun onDisconnected(camera: CameraDevice) {
                    camera.close()
                    if (myGeneration == cameraGeneration) {
                        cameraDevice = null
                        cameraTaken("$op.onDisconnected", null)
                    }
                }
                override fun onError(camera: CameraDevice, error: Int) {
                    camera.close()
                    if (myGeneration == cameraGeneration) {
                        cameraDevice = null
                        val e = RuntimeException("Camera2 error code $error")
                        if (takenBy(error)) { cameraTaken("$op.onError", e); return }
                        if (codec == H264Stream.CODEC_H264 && error == CameraDevice.StateCallback.ERROR_CAMERA_DEVICE) {
                            // The camera gave up feeding the encoder (a size or rate it can't): MJPEG, not the end.
                            // The camera service restarts it first, so opening it again waits a little.
                            encoderFailed(e, "H.264 at ${streamWidth}x$streamHeight stopped the camera on this phone",
                                unsupported = true, reopenTries = REOPEN_TRIES)
                            return
                        }
                        if (tries > 0 && retryReopen(op, tries, myGeneration)) return
                        fail("$op.onError", e)
                    }
                }
            }, handler)
        } catch (e: Exception) {
            if (takenBy(e)) { cameraTaken(op, e); return }
            // Right after the camera failed it can be briefly unknown ("Unable to retrieve camera characteristics")
            if (tries > 0 && retryReopen(op, tries, myGeneration)) return
            fail(op, e)
        }
    }

    private fun retryReopen(op: String, tries: Int, generation: Int): Boolean {
        val h = handler ?: return false
        h.postDelayed({ if (!stopped && generation == cameraGeneration) reopen(op, tries - 1) }, REOPEN_RETRY_MS)
        return true
    }
}
