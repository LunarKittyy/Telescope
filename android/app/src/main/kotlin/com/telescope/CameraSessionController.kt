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
    val codecFailure:      CodecFailure?,
    val activeLens:        String?,
    val dynamicFps:        Int = 0,  // the rate Dynamic stepped down to for a slow link; 0 = the rate asked for
    val lowLight:          Boolean = false,
    val h264Profile:       String? = null,  // "high", "baseline" or "auto" while H.264 runs
    val intraRefresh:      Boolean = false,  // the encoder refreshes the picture gradually instead of with keyframes
    val heatFps:           Int = 0,  // the rate the phone's heat capped the camera at; 0 = not capped
    val heatWidth:         Int = 0,  // the size the phone's heat dropped the stream to; 0 = the size asked for
    val heatHeight:        Int = 0,
)

// Phone-side zoom, as the desktop splits it: a centred CONTROL_ZOOM_RATIO, then a 1/crop SCALER_CROP_REGION
// around (x, y) in 0..1 of that view. Kept normalized, so it carries over a lens switch, clamped to the new lens.
data class ZoomRequest(val ratio: Float = 1f, val crop: Float = 1f, val x: Float = 0.5f, val y: Float = 0.5f)

// A photo output next to the stream's (StillCapture), never in the repeating request; maxRes: a full resolution mode size
class StillOutput(val surface: Surface, val maxRes: Boolean)

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
        private const val ENCODER_CAP_CHECK_MS = 3_000L
        private const val COOL_DOWN_MS = 30_000L
        // False once this phone's encoder was caught ignoring KEY_MAX_FPS_TO_ENCODER; every step is checked until then
        @Volatile var encoderCapWorks: Boolean? = null
        // True once a FrameGate failed here: steps restart the camera from then on, until the app restarts
        @Volatile var gateBroken = false
    }

    private var cameraDevice: CameraDevice? = null
    private var captureSession: CameraCaptureSession? = null
    private var imageReader: ImageReader? = null
    private var encoder: H264Encoder? = null
    private var gate: FrameGate? = null  // drops frames for an encoder that ignores the cap; between camera and encoder
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
    // Set by meter_point: like focusPoint, but only metering goes there and focus stays as it is
    @Volatile private var meterPoint: FloatArray? = null
    @Volatile private var zoom = ZoomRequest()
    @Volatile private var currentNrMode:         Int     = CaptureRequest.NOISE_REDUCTION_MODE_FAST
    @Volatile private var currentEdgeMode:       Int     = CaptureRequest.EDGE_MODE_FAST
    @Volatile private var currentAeComp:         Int     = 0
    @Volatile private var currentBlackLevelLock: Boolean = false
    // Low light: auto exposure may slow the camera down (to LOW_LIGHT_MIN_FPS where the phone has that) and noise
    // reduction runs at its best, for a dark room
    @Volatile private var currentLowLight: Boolean = false
    @Volatile private var currentTorch:          Boolean = false
    @Volatile private var currentJpegQuality: Int = 85
    @Volatile private var currentPhoneFps:    Int = initialPhoneFps
    // Mutable so switchResolution() can change live; sized by openCamera(). What the camera and encoder run at, which
    // is the asked size unless the phone's heat dropped it (heatSize)
    @Volatile private var streamWidth:  Int = initialStreamWidth
    @Volatile private var streamHeight: Int = initialStreamHeight
    // The size the computer asked for, which the state keeps reporting so a hot spell doesn't change its pick
    @Volatile private var askedWidth:   Int = initialStreamWidth
    @Volatile private var askedHeight:  Int = initialStreamHeight
    @Volatile private var codec: String = H264Stream.CODEC_MJPEG
    @Volatile private var requestedBitrate: Int = 0  // 0 = sized from resolution and fps, H264Stream.DYNAMIC = dynamic
    @Volatile private var dynamic: DynamicBitrate? = null
    // Dynamic's frame rate step, alongside its bitrate: only while Dynamic runs the H.264 stream
    @Volatile private var frameRate: DynamicFrameRate? = null
    @Volatile private var codecFailure: CodecFailure? = null
    // Photo outputs the session carries besides the stream's (StillCapture); dropped when they make it fail
    @Volatile private var stillOutputs: List<StillOutput> = emptyList()
    // StillCapture closed the session to run its own; rebuilds wait for resumeAfterStill, which takes in every change
    @Volatile private var stillPaused = false
    // StillCapture's: the lens changed or the camera reopened or stopped, which ends a photo in flight (camera thread)
    var onStillInterrupted: ((StillRebuild.Cause) -> Unit)? = null
    // StillCapture's: the stream's session is up again after a rebuild, with the photo outputs it kept (camera thread)
    var onStillSessionReady: (() -> Unit)? = null
    // useStillOutputs' answer, given once a session is up or the photo can't go on
    private var stillSetup: ((Boolean) -> Unit)? = null
    // Automatic changes waiting for the photo under way to end (camera thread)
    private val photoHold = PhotoHold<() -> Unit>()
    // The newest result of the repeating request: what auto exposure and focus settled on, for photos
    @Volatile private var lastResult: TotalCaptureResult? = null

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
    fun getCurrentCamera(): CameraEntry? = currentCamera
    fun isCameraOff(): Boolean = cameraOff
    fun isCameraTaken(): Boolean = taken
    fun getStreamSize(): android.util.Size = android.util.Size(streamWidth, streamHeight)
    // What the computer picked, which the heat may have dropped the stream below
    fun getAskedSize(): android.util.Size = android.util.Size(askedWidth, askedHeight)

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
        streamWidth     = askedWidth,
        streamHeight    = askedHeight,
        codec           = codec,
        bitrate         = currentBitrate(),
        codecFailure    = codecFailure,
        activeLens      = activeLens,
        dynamicFps      = frameRate?.cap ?: 0,
        lowLight        = currentLowLight,
        h264Profile     = encoder?.profile?.wire,
        intraRefresh    = encoder?.intraRefresh == true,
        heatFps         = heatCap()?.takeIf { it < (frameRate?.cap ?: currentPhoneFps) } ?: 0,
        heatWidth       = if (heated()) streamWidth else 0,
        heatHeight      = if (heated()) streamHeight else 0,
    )

    private fun heated() = streamWidth != askedWidth || streamHeight != askedHeight

    // The smaller size the heat calls for, or null to stream at the asked size
    private fun heatSize(): Pair<Int, Int>? {
        val sizes = currentCamera?.supportedSizes?.map { it.width to it.height } ?: return null
        return HeatStep.size(thermalStatus, askedWidth to askedHeight, sizes) { w, h ->
            codec != H264Stream.CODEC_H264 || H264Encoder.supportsSize(w, h)
        }
    }

    // The rate the camera and encoder run at: the one asked for, unless Dynamic stepped it down for a slow link
    // or the phone is running hot.
    private fun streamFps(): Int {
        val rate = frameRate?.cap ?: currentPhoneFps
        return heatCap()?.let { minOf(it, rate) } ?: rate
    }

    // The rate the camera runs at: the stream's, unless the encoder is the one dropping frames for Dynamic's step
    private fun cameraFps(): Int {
        val rate = if (capInEncoder) currentPhoneFps else frameRate?.cap ?: currentPhoneFps
        return heatCap()?.let { minOf(it, rate) } ?: rate
    }

    // Dynamic's step is the encoder dropping frames rather than the camera's session being rebuilt for it
    @Volatile private var capInEncoder = false
    @Volatile private var sessionFps = 0  // the rate the camera's last repeating request was built for
    private var capCheckGeneration = 0

    private fun canCapInEncoder(): Boolean = encoder != null && heatCap() == null &&
        (gate != null || (encoderCapWorks != false && Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q))

    // On the camera thread, after Dynamic stepped: the encoder takes the new rate, or the session is rebuilt for it
    private fun applyFrameRate() {
        if (photoHold.hold("frameRate") { applyFrameRate() }) { encoder?.setBitrate(currentBitrate()); return }
        val cap = frameRate?.cap
        val enc = encoder
        // Only from a camera already at the rate asked for: one an earlier step slowed down needs the session rebuilt anyway
        val g = gate
        if (enc != null && g != null && (capInEncoder || (cap != null && sessionFps == currentPhoneFps)) && canCapInEncoder()) {
            g.maxFps = cap  // ours: no need to check it took
            capInEncoder = cap != null
        } else if (enc != null && (capInEncoder || (cap != null && sessionFps == currentPhoneFps)) && canCapInEncoder() &&
            enc.setMaxFps(cap, currentPhoneFps)) {
            capInEncoder = cap != null
            if (cap != null) checkEncoderCap(enc, cap)
        } else {
            uncapEncoder()
            reconfigureSession()
        }
        encoder?.setBitrate(currentBitrate())
    }

    // Some encoders take KEY_MAX_FPS_TO_ENCODER and ignore it: a few seconds on, what came out says whether it worked
    private fun checkEncoderCap(enc: H264Encoder, cap: Int) {
        val generation = ++capCheckGeneration
        handler?.postDelayed({ runCapCheck(enc, cap, generation) }, ENCODER_CAP_CHECK_MS)
    }

    private fun runCapCheck(enc: H264Encoder, cap: Int, generation: Int) {
        if (generation != capCheckGeneration || encoder !== enc || !capInEncoder || frameRate?.cap != cap) return
        // A photo's hitch skews what came out, and the fix restarts the camera: checked afresh once it's over
        if (photoHold.hold("capCheck") { checkEncoderCap(enc, cap) }) return
        val out = enc.outputRate.fps()
        // Working, it puts out the cap give or take a frame; ignoring it, the camera's rate (a rung or more above)
        if (out > cap + maxOf(2.0, cap * 0.1)) {
            android.util.Log.i(TAG, "The encoder didn't drop frames for $cap fps ($out came out): restarting the camera instead")
            encoderCapWorks = false
            uncapEncoder()
            // The camera holds the encoder's surface, so a FrameGate comes with a fresh encoder: one reopen, then no more restarts
            if (!gateBroken) { reopen("addFrameGate"); return }
            reconfigureSession()
            encoder?.setBitrate(currentBitrate())
        }
    }

    private fun uncapEncoder() {
        if (!capInEncoder) return
        capInEncoder = false
        capCheckGeneration++
        gate?.maxFps = null
        if (gate == null) encoder?.setMaxFps(null, currentPhoneFps)
    }

    // A FrameGate in front of the encoder; the session has to be rebuilt to send the camera into it
    private fun addGate(): Boolean {
        val enc = encoder ?: return false
        if (gate != null) return true
        if (gateBroken) return false
        return try {
            gate = FrameGate(streamWidth, streamHeight, enc.inputSurface) { e -> post { gateFailed(e) } }
            true
        } catch (e: Exception) {
            gateBroken = true
            onControlError("frameGate", e)
            false
        }
    }

    private fun gateFailed(e: Throwable) {
        if (gate == null) return
        gateBroken = true
        onControlError("frameGate", e)
        reopen("frameGate")  // without it: steps restart the camera again
    }

    // Back to no Dynamic step; the encoder lets every frame through again
    private fun forgetFrameRate() {
        frameRate = null
        if (capInEncoder) post { uncapEncoder() }
    }

    @Volatile private var thermalStatus = 0

    private fun heatCap(): Int? {
        val ranges = currentCamera?.aeFpsRanges?.map { it.lower to it.upper }
        return HeatStep.cap(thermalStatus, currentPhoneFps) { ranges == null || CameraRequestSelection.capsAt(ranges, it) }
    }

    /** Android's thermal status (PowerManager.THERMAL_STATUS_*): severe and up lower the frame rate until it cools. */
    fun setThermalStatus(status: Int) {
        // Hotter applies at once; cooler only once it has held a while, so a status flickering at the edge doesn't
        // rebuild the session every time
        if (status < thermalStatus) {
            val seen = ++thermalSeen
            handler?.postDelayed({ if (seen == thermalSeen) applyThermalStatus(status) }, COOL_DOWN_MS)
        } else {
            thermalSeen++
            post { applyThermalStatus(status) }
        }
    }

    @Volatile private var thermalSeen = 0  // bumped by every status, so a cool-down that waited sees a newer one

    private fun applyThermalStatus(status: Int) {
        if (photoHold.hold("thermal") { applyThermalStatus(status) }) return
        val before = cameraFps() to streamFps()
        thermalStatus = status
        // A new size needs a new reader or encoder, so the camera reopens, which picks up the new rate too
        val (w, h) = heatSize() ?: (askedWidth to askedHeight)
        if (w != streamWidth || h != streamHeight) {
            android.util.Log.i(TAG, "Thermal status $status: ${w}x$h at ${streamFps()} fps")
            streamWidth = w
            streamHeight = h
            reopen("heatSize")
            return
        }
        val after = cameraFps() to streamFps()
        if (after == before) return
        android.util.Log.i(TAG, "Thermal status $status: $after fps")
        reconfigureSession()
        encoder?.setBitrate(currentBitrate())
    }

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
            codecFailure = null
            forgetFrameRate()  // the reopen below picks the rate up again
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
        val capped = frameRate?.cap != null && !capInEncoder
        forgetFrameRate()
        return capped
    }

    /** The bitrate Dynamic set, or 0 when it's off. */
    val dynamicBitrate: Int get() = if (requestedBitrate == H264Stream.DYNAMIC) dynamic?.bitrate ?: 0 else 0

    /** Whether Dynamic found where the link fills up lately (DynamicBitrate.full). */
    val dynamicFull: Boolean get() = requestedBitrate == H264Stream.DYNAMIC && dynamic?.full == true

    /** How the link did lately (MjpegServer, on the encoder's thread); Dynamic moves the bitrate to match, and the
     *  frame rate too once the bitrate alone can't keep it sharp. */
    fun onLinkSample(sample: DynamicBitrate.Sample) {
        if (requestedBitrate != H264Stream.DYNAMIC || codec != H264Stream.CODEC_H264) return
        val d = dynamic ?: return
        d.update(sample)?.let {
            android.util.Log.i(TAG, "Dynamic: ${it / 1000} kbps (queue ${sample.queueMs} ms, on the way ${sample.routerMs} ms, " +
                "lost ${"%.1f".format(sample.loss * 100)}%, through ${sample.sentBytes * 8 / 250} kbps)")
            encoder?.setBitrate(it)
        }
        // A sample racing setFpsTarget or a lens switch could put back rungs for the rate or lens before; those are
        // rebuilt here
        val camera = currentCamera
        val old = frameRate
        val rate = old?.takeIf { it.askedFps == currentPhoneFps && it.lens === camera } ?: newFrameRate(camera).also {
            frameRate = it
            if (old?.cap != null) post { applyFrameRate() }  // uncapped
        }
        val stepped = rate.update(sample.nowMs, d.bitrate, streamWidth, streamHeight, d.lastStallCutMs) ?: return
        android.util.Log.i(TAG, "Dynamic: ${stepped} fps for a ${d.bitrate / 1000} kbps link")
        // The encoder drops frames for it where it can; otherwise the session is rebuilt, as the rate is part of
        // its setup (see createSession)
        post { applyFrameRate() }
    }

    private fun newFrameRate(camera: CameraEntry?): DynamicFrameRate {
        val ranges = camera?.aeFpsRanges?.map { it.lower to it.upper }
        return DynamicFrameRate(currentPhoneFps, camera) { ranges == null || CameraRequestSelection.capsAt(ranges, it) }
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
        forgetFrameRate()  // a new rate asked for: Dynamic starts from it
        // A new rate goes in the session's own setup too (see createSession), so the session is rebuilt for it
        post { if (changed) reconfigureSession() else applyExposure(); encoder?.setBitrate(currentBitrate()) }
    }
    fun setFocusMode(mode: String)          { currentFocusMode = mode; focusPoint = null; meterPoint = null; post { applyExposure() } }
    fun setZoom(z: ZoomRequest)             { zoom = z;                         post { applyExposure() } }

    // Focus (and meter, when exposure is automatic) on a point of the stream frame. False if this lens
    // can't: no AF regions, no single-shot AF, or no known active array.
    fun setFocusPoint(x: Float, y: Float, size: Float): Boolean {
        val cam = currentCamera ?: return false
        if (cam.activeArray == null) return false
        if (cam.maxAfRegions <= 0 || CaptureRequest.CONTROL_AF_MODE_AUTO !in cam.afModes) return false
        focusPoint = floatArrayOf(x, y, size)
        meterPoint = null
        currentFocusMode = "point"
        post { triggerFocus() }
        return true
    }

    // Meters on a point while exposure is automatic and leaves focus alone; false without AE regions or an active array
    fun setMeterPoint(x: Float, y: Float, size: Float): Boolean {
        val cam = currentCamera ?: return false
        if (cam.activeArray == null || cam.maxAeRegions <= 0) return false
        meterPoint = floatArrayOf(x, y, size)
        post { applyExposure() }
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
        val rect = meteringRect(point, cam) ?: return false
        builder.set(CaptureRequest.CONTROL_MODE, CaptureRequest.CONTROL_MODE_AUTO)
        builder.set(CaptureRequest.CONTROL_AF_MODE, CaptureRequest.CONTROL_AF_MODE_AUTO)
        builder.set(CaptureRequest.CONTROL_AF_REGIONS, arrayOf(rect))
        if (cam.maxAeRegions > 0 && currentIso == null) builder.set(CaptureRequest.CONTROL_AE_REGIONS, arrayOf(rect))
        return true
    }

    private fun applyMeterRegion(builder: CaptureRequest.Builder) {
        val point = meterPoint ?: return
        val cam = currentCamera ?: return
        if (cam.maxAeRegions <= 0 || currentIso != null) return
        builder.set(CaptureRequest.CONTROL_AE_REGIONS, arrayOf(meteringRect(point, cam) ?: return))
    }

    // point: x, y and size in the stream frame, mapped onto the sensor through the zoom
    private fun meteringRect(point: FloatArray, cam: CameraEntry): MeteringRectangle? {
        val area = zoomCrop(cam) ?: cam.activeArray ?: return null
        val region = CameraRequestSelection.meteringRect(point[0], point[1], point[2], area, streamWidth, streamHeight)
        return MeteringRectangle(region[0], region[1], region[2], region[3], MeteringRectangle.METERING_WEIGHT_MAX)
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
        stillRebuilding(StillRebuild.Cause.OFF)
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
                    onPacket = onH264, onError = { e -> post { encoderFailed(e) } },
                    onReorder = { post { if (codec == H264Stream.CODEC_H264) reopen("h264Reorders") } })
                // An encoder known to ignore the cap gets a FrameGate straight away; Dynamic's step is then its job
                if (encoderCapWorks == false && addGate() && frameRate?.cap != null) {
                    gate?.maxFps = frameRate?.cap
                    capInEncoder = true
                }
                return
            } catch (e: Exception) {
                codec = H264Stream.CODEC_MJPEG
                forgetFrameRate()  // Dynamic's step was for H.264; JPEG goes back to the rate asked for
                codecFailure = CodecFailure(CodecFailure.UNAVAILABLE, streamWidth, streamHeight)
                onControlError("h264Encoder", e)
                onCodecFailed(codecFailure!!.text)
            }
        }
        imageReader = buildImageReader()
    }

    // code: a CodecFailure code; all but ENCODER_STOPPED mean it can't do this size or rate here (so trying again
    // won't help), rather than a crash.
    // reopenTries: after the camera itself failed, it can take a moment before it can be opened again.
    private fun encoderFailed(e: Throwable, code: String = CodecFailure.ENCODER_STOPPED, reopenTries: Int = 0) {
        if (codec != H264Stream.CODEC_H264) return
        codec = H264Stream.CODEC_MJPEG
        forgetFrameRate()  // Dynamic's step was for H.264, and it can't step back up on JPEG
        codecFailure = CodecFailure(code, streamWidth, streamHeight)
        onControlError("h264Encoder", e)
        onCodecFailed(codecFailure!!.text)
        reopen("encoderFailed", reopenTries)
    }

    private fun releaseOutputs() {
        try { imageReader?.close() } catch (_: Exception) {}
        gate?.release()
        encoder?.release()
        imageReader = null; encoder = null; gate = null
        stillOutputs = emptyList()  // StillCapture makes its readers again for the new session
        stillPaused = false
        capInEncoder = false  // a new encoder doesn't have the cap: the session takes Dynamic's rate again
        capCheckGeneration++
    }

    private fun outputSurface(): Surface? = gate?.inputSurface ?: encoder?.inputSurface ?: imageReader?.surface

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
        if (stillOutputs.isNotEmpty()) {
            // Photos don't get to break the stream: it goes back without them, and StillCapture shoots on its own
            stillOutputs = emptyList()
            onControlError("$op.stillOutputsDropped", e ?: IllegalStateException("configure failed"))
            reconfigureSession(onComplete)
            return true
        }
        if (usedStreamUseCase && !streamUseCaseRefused) {
            // The labels are only a hint: a camera that won't take them gets the session without, from now on
            streamUseCaseRefused = true
            onControlError("$op.streamUseCaseDropped", e ?: IllegalStateException("configure failed"))
            reconfigureSession(onComplete)
            return true
        }
        if (retryWithoutPreview(op, e, onComplete)) return true
        if (codec == H264Stream.CODEC_H264) {
            // The camera can't feed the encoder at this size: MJPEG instead, and codecFailure stops the desktop retrying
            encoderFailed(e ?: IllegalStateException("$op: configure failed"), CodecFailure.CAMERA_CANT_FEED)
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
            val outCfgs = sessionOutputs(physId, stillOutputs)
            camera.createCaptureSession(SessionConfiguration(
                SessionConfiguration.SESSION_REGULAR, outCfgs, exec,
                object : CameraCaptureSession.StateCallback() {
                    override fun onConfigured(s: CameraCaptureSession) {
                        if (generation != cameraGeneration) { s.close(); onComplete?.invoke(); return }
                        if (mySession == sessionGeneration) { captureSession = s; startRepeating(camera, s); stillSessionReady() }
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

    @androidx.annotation.RequiresApi(Build.VERSION_CODES.P)
    private fun sessionOutputs(physId: String?, stills: List<StillOutput>): List<OutputConfiguration> =
        currentTargetSurfaces().map { surface ->
            OutputConfiguration(surface).also { cfg ->
                physId?.let { cfg.setPhysicalCameraId(it) }
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU && !streamUseCaseRefused)
                    applyStreamUseCase(cfg, surface)
            }
        } + stills.map { still -> StillCapture.outputConfig(still, physId) }

    @Suppress("DEPRECATION")
    private fun createLegacySession(
        camera: CameraDevice,
        generation: Int = cameraGeneration,
        mySession: Int = sessionGeneration,
        onComplete: (() -> Unit)? = null,
    ) {
        if (generation != cameraGeneration) { onComplete?.invoke(); return }
        if (noOutput("createLegacySession", onComplete)) return
        val targets = currentTargetSurfaces() + stillOutputs.map { it.surface }
        try {
            camera.createCaptureSession(targets,
                object : CameraCaptureSession.StateCallback() {
                    override fun onConfigured(s: CameraCaptureSession) {
                        if (generation != cameraGeneration) { s.close(); onComplete?.invoke(); return }
                        if (mySession == sessionGeneration) { captureSession = s; startRepeating(camera, s); stillSessionReady() }
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

    // Rebuilds capture session without reopening device; onComplete fires when terminal state reached.
    // The photo outputs stay in it, so a photo under way carries on (StillCapture asks again for what it dropped)
    private fun reconfigureSession(onComplete: (() -> Unit)? = null, resumingStill: Boolean = false) {
        val camera = cameraDevice ?: run { onComplete?.invoke(); return }
        // The photo's own session has the camera: the stream's comes back with every change once the photo is done
        if (stillPaused && !resumingStill) { onComplete?.invoke(); return }
        stillPaused = false
        stillRebuilding(StillRebuild.Cause.SESSION)
        try { captureSession?.stopRepeating() } catch (_: Exception) {}
        try { captureSession?.close() } catch (_: Exception) {}
        captureSession = null

        val mySession = ++sessionGeneration
        val cam    = currentCamera
        val physId = if (cam?.logicalId != null) cam.id else null
        createSession(camera, physId, cameraGeneration, mySession, onComplete)
    }

    // Tells StillCapture: a photo in flight ends unless only the session is rebuilt
    private fun stillRebuilding(cause: StillRebuild.Cause) {
        if (!StillRebuild.interrupts(cause)) return
        onStillInterrupted?.invoke(cause)
        // After the photo ended, so its outputs aren't taken as refused by this lens
        stillSetup?.let { stillSetup = null; it(false) }
    }

    private fun stillSessionReady() {
        stillSetup?.let { stillSetup = null; it(true) }
        onStillSessionReady?.invoke()
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
            encoder != null && (surface === encoder?.inputSurface || surface === gate?.inputSurface) ->
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
        val asked = CameraRequestSelection.pickAeFpsRange(cam?.aeFpsRanges ?: emptyList(), cameraFps(), currentLowLight)
        val template = if (requestTemplate() == CameraDevice.TEMPLATE_RECORD) "record" else "preview"
        val labels = when {
            usedStreamUseCase -> ", outputs labelled"
            streamUseCaseRefused -> ", output labels refused"
            else -> ""
        }
        val stepped = (frameRate?.cap?.let { ", Dynamic stepped down to $it for the link" } ?: "") +
            (heatCap()?.let { ", $it for the heat (thermal status $thermalStatus)" } ?: "")
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

    // probe: only to ask the camera about a session, so sessionFps stays the running request's
    private fun buildRequest(camera: CameraDevice = cameraDevice!!, probe: Boolean = false): CaptureRequest {
        return camera.createCaptureRequest(requestTemplate()).apply {
            addTarget(outputSurface()!!)
            previewSurface?.let { addTarget(it) }
            val fps = cameraFps()
            if (!probe) sessionFps = fps
            applyControls(this, fps)
        }.build()
    }

    // Every setting the computer made: the stream's repeating request, and the photos taken from it
    private fun applyControls(builder: CaptureRequest.Builder, fps: Int) {
        builder.apply {
            val cam = currentCamera
            if (cam != null) applyZoom(this, cam)

            // Use CONTROL_MODE_AUTO even in manual AE so AF keeps running independently
            set(CaptureRequest.CONTROL_MODE, CaptureRequest.CONTROL_MODE_AUTO)
            // A recording template may turn stabilisation on, which crops: framing and zoom here assume the full view
            set(CaptureRequest.CONTROL_VIDEO_STABILIZATION_MODE, CaptureRequest.CONTROL_VIDEO_STABILIZATION_MODE_OFF)
            // Set in manual exposure too, where AE ignores it: it's also what the session is set up for.
            // Unsupported ranges can fail on some devices; use advertised range
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
            applyMeterRegion(this)

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
        }
    }

    // ── For StillCapture, on the camera thread ──────────────────────────────

    fun stillHandler(): Handler? = handler
    fun stillDevice(): CameraDevice? = cameraDevice
    fun stillCamera(): CameraEntry? = currentCamera
    fun stillSession(): CameraCaptureSession? = captureSession
    // The stream's outputs, which a photo's request can feed too so the video doesn't stop for it
    fun stillStreamTargets(): List<Surface> = currentTargetSurfaces()
    // Changes with every reopen: a photo started on an older camera stops there
    fun stillGeneration(): Int = cameraGeneration
    fun stillOutputsInSession(outputs: List<StillOutput>): Boolean =
        outputs.isNotEmpty() && stillOutputs === outputs && captureSession != null
    // The lens a multi-lens camera streams from, as the session's outputs name it; null for the camera itself
    fun stillPhysicalId(): String? = currentCamera?.let { if (it.logicalId != null) it.id else null }
    // What decides whether photo outputs fit next to the stream: the lens, the stream's size and its codec
    fun stillStreamKey(): String = "${currentCamera?.id}|${streamWidth}x$streamHeight|$codec"

    // What the stream exposes at: the exposure set by hand, or what auto exposure settled on lately
    fun streamExposure(): StillExposure? {
        val iso = currentIso
        val shutter = currentShutterNs
        if (iso != null && shutter != null) return StillExposure(shutter, iso)
        val result = lastResult ?: return null
        val e = result.get(CaptureResult.SENSOR_EXPOSURE_TIME) ?: return null
        val s = result.get(CaptureResult.SENSOR_SENSITIVITY) ?: return null
        return StillExposure(e, s)
    }

    // The shutter the stream's requests set by hand, which a photo's shot without its own exposure keeps; 0 = auto
    fun manualShutterNs(): Long {
        val cam = currentCamera ?: return 0L
        val shutter = currentShutterNs ?: return 0L
        if (currentIso == null || !cam.supportsManualSensor) return 0L
        return CameraRequestSelection.clamp(shutter, cam.shutterMinNs, cam.shutterMaxNs)
    }

    // A request with the stream's settings (lens, zoom, exposure, white balance, focus, torch)
    fun stillRequest(device: CameraDevice, template: Int): CaptureRequest.Builder =
        device.createCaptureRequest(template).also { applyControls(it, cameraFps()) }

    // Whether the session takes these outputs next to the stream's; null when the phone can't say, so it's worth trying
    fun stillsFit(outputs: List<StillOutput>): Boolean? {
        val camera = cameraDevice ?: return false
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.Q) return null
        return try {
            val cfg = SessionConfiguration(SessionConfiguration.SESSION_REGULAR, sessionOutputs(stillPhysicalId(), outputs),
                Executor { it.run() }, object : CameraCaptureSession.StateCallback() {
                    override fun onConfigured(s: CameraCaptureSession) {}
                    override fun onConfigureFailed(s: CameraCaptureSession) {}
                })
            cfg.sessionParameters = buildRequest(camera, probe = true)
            camera.isSessionConfigurationSupported(cfg)
        } catch (_: Exception) { null }
    }

    // Rebuilds the stream's session with these photo outputs as well (empty: none); done(true) once they're in it
    fun useStillOutputs(outputs: List<StillOutput>, done: (Boolean) -> Unit) {
        if (cameraDevice == null || cameraOff) { done(false); return }
        if (stillOutputs === outputs && captureSession != null && !stillPaused) { done(true); return }
        stillOutputs = outputs
        // Answered by the session that comes up with them, or without them when they made it fail
        stillSetup?.invoke(false)
        stillSetup = { ready -> done(ready && (stillOutputsInSession(outputs) || (outputs.isEmpty() && captureSession != null))) }
        reconfigureSession()
    }

    // StillCapture's photo is being set up or taken: automatic session changes wait until it ends
    fun stillBusy(busy: Boolean) {
        if (busy) { photoHold.start(); return }
        val held = photoHold.end()
        // A photo that began before this runs holds each change again, rather than dropping it
        if (held.isNotEmpty()) post { held.forEach { it() } }
    }

    // Closes the stream's session so StillCapture can run its own; false when there's no camera to do it on
    fun pauseForStill(): Boolean {
        if (cameraDevice == null || cameraOff || stopped) return false
        try { captureSession?.stopRepeating() } catch (_: Exception) {}
        try { captureSession?.close() } catch (_: Exception) {}
        captureSession = null
        stillOutputs = emptyList()
        sessionGeneration++  // a session of the stream's still configuring is closed when it gets there
        stillPaused = true
        return true
    }

    // The stream's repeating request again after a photo aborted its captures on the stream's own session
    fun stillRestartStream(session: CameraCaptureSession) {
        if (captureSession !== session || stillPaused) return
        val c = cameraDevice ?: return
        try {
            session.setRepeatingRequest(buildRequest(c), ccmCaptureCallback, handler)
        } catch (e: Exception) {
            onControlError("stillRestartStream", e)
        }
    }

    // The stream's session again, unless something else rebuilt it or reopened the camera meanwhile
    fun resumeAfterStill(generation: Int) {
        if (!stillPaused || generation != cameraGeneration || stopped) return
        reconfigureSession(resumingStill = true)
    }

    private val ccmCaptureCallback = object : CameraCaptureSession.CaptureCallback() {
        override fun onCaptureCompleted(
            session: CameraCaptureSession,
            request: CaptureRequest,
            result: TotalCaptureResult
        ) {
            lastResult = result
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
        meterPoint = null
        dropFrameRate()  // its rungs came from the old lens's ranges; the next link sample builds the new lens's
        currentCamera = entry
        if (cameraOff) return  // the lens it opens with when the camera comes back on
        onStateChanged(StreamState.Recovering, "switchCameraTo", null)
        stillRebuilding(StillRebuild.Cause.LENS)

        val myGeneration = ++cameraGeneration
        try { captureSession?.close() } catch (_: Exception) {}
        try { cameraDevice?.close()   } catch (_: Exception) {}
        captureSession = null; cameraDevice = null
        // The old lens's photo outputs wouldn't fit the new one: StillCapture makes them again for it
        stillOutputs = emptyList()
        stillPaused = false

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
        askedWidth = width
        askedHeight = height
        val (w, h) = heatSize() ?: (width to height)
        if (w == streamWidth && h == streamHeight) return
        streamWidth = w
        streamHeight = h
        reopen("switchResolution")
    }

    // Same lens, new output (size or codec): the reader or encoder is rebuilt, which needs a reopen.
    // tries: how many more times to try, REOPEN_RETRY_MS apart, while the camera can't be opened yet.
    private fun reopen(op: String, tries: Int = 0) {
        if (cameraOff) return  // a new size or codec is simply what it opens with later
        onStateChanged(StreamState.Recovering, op, null)
        stillRebuilding(StillRebuild.Cause.REOPEN)

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
                            encoderFailed(e, CodecFailure.CAMERA_STOPPED, reopenTries = REOPEN_TRIES)
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
