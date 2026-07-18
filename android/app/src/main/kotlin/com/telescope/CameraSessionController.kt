package com.telescope

import android.content.Context
import android.graphics.ImageFormat
import android.hardware.camera2.*
import android.hardware.camera2.params.ColorSpaceTransform
import android.hardware.camera2.params.OutputConfiguration
import android.hardware.camera2.params.RggbChannelVector
import android.hardware.camera2.params.SessionConfiguration
import android.media.ImageReader
import android.os.Build
import android.os.Handler
import android.os.HandlerThread
import android.view.Surface
import java.util.concurrent.Executor

/** Read-only snapshot of the current camera-session control state, for
 *  building the /v1/state JSON response without exposing the controller's
 *  mutable fields directly. */
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
)

/**
 * Owns the live Camera2 session: the open [CameraDevice]/[CameraCaptureSession]/
 * [ImageReader], the generation-guard counters that protect them from stale
 * async callbacks, and the control-state fields that shape each capture
 * request. [CameraStreamService] owns everything else - foreground-service
 * lifecycle, the HTTP server, the camera catalogue, and translating HTTP
 * actions into calls here.
 *
 * [onFrame] receives each JPEG frame, [onStateChanged] mirrors every
 * [StreamState] transition this controller makes (the service still owns
 * the actual [StreamStateMachine] and diagnostics history), and
 * [onFatalError] is invoked wherever the original in-service code called
 * `stopSelf()` on an unrecoverable open/configure failure. [switchTo]'s own
 * open-failure callback deliberately skips it, matching the original
 * behavior of leaving the service running (just in a Failed state) if a
 * mid-stream camera switch fails to open the new camera - but a session-
 * configuration or repeating-request failure downstream of that open
 * (shared with [openCamera] via [createPhysicalSession]/
 * [createLegacySession]/[startRepeating]) still calls it either way.
 *
 * [onControlError] reports a *non-fatal* failure: a live control update that
 * couldn't be applied even though the existing stream keeps running on the
 * previous repeating request. It carries the failing operation and the
 * sanitized cause so the service can surface it in diagnostics without tearing
 * the session down.
 */
class CameraSessionController(
    private val context: Context,
    private val streamWidth: Int,
    private val streamHeight: Int,
    private val onFrame: (ByteArray) -> Unit,
    private val onStateChanged: (StreamState, String, Throwable?) -> Unit,
    private val onFatalError: () -> Unit,
    private val onControlError: (String, Throwable) -> Unit = { _, _ -> },
) {
    companion object {
        private const val TAG = "CameraSessionController"
    }

    private var cameraDevice: CameraDevice? = null
    private var captureSession: CameraCaptureSession? = null
    private var imageReader: ImageReader? = null
    private var handlerThread: HandlerThread? = null
    private var handler: Handler? = null
    @Volatile private var previewSurface: Surface? = null

    // Exposure state - null ISO/shutter = auto AE
    @Volatile private var currentIso:       Int?  = null
    @Volatile private var currentShutterNs: Long? = null
    @Volatile private var currentOis:       Boolean = true
    // WB state - null gains = auto AWB
    @Volatile private var currentWbGains: RggbChannelVector? = null
    @Volatile private var lastCCM:        ColorSpaceTransform? = null
    @Volatile private var lastMeasuredGains: RggbChannelVector? = null
    // Focus state
    @Volatile private var currentFocusMode:     String = "continuous"  // "continuous" | "manual"
    @Volatile private var currentFocusDistance: Float  = 0f            // diopters; 0 = infinity
    // Image quality controls
    @Volatile private var currentNrMode:         Int     = CaptureRequest.NOISE_REDUCTION_MODE_FAST
    @Volatile private var currentEdgeMode:       Int     = CaptureRequest.EDGE_MODE_FAST
    @Volatile private var currentAeComp:         Int     = 0
    @Volatile private var currentBlackLevelLock: Boolean = false
    @Volatile private var currentTorch:          Boolean = false
    // Stream quality
    @Volatile private var currentJpegQuality: Int = 85
    @Volatile private var currentPhoneFps:    Int = 30

    @Volatile private var currentCamera: CameraEntry? = null

    // Bumped on every camera-open attempt. Async onOpened/onConfigured callbacks
    // compare their captured generation against the current value and discard
    // themselves if a newer open has superseded them, so a stale callback can't
    // clobber the camera that's actually active.
    @Volatile private var cameraGeneration = 0

    // Bumped on every reconfigureSession() call (attach/detach preview surface),
    // independent of cameraGeneration (which only changes on a full camera open/
    // switch). Distinguishes "this is still the latest reconfigure attempt for the
    // currently-open camera" from a rapid attach immediately followed by detach (or
    // vice versa), where two createCaptureSession calls can be in flight at once and
    // an older one's onConfigured could otherwise land after the newer one and
    // clobber captureSession with a stale session.
    @Volatile private var sessionGeneration = 0

    fun getCurrentCameraId(): String? = currentCamera?.id

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
    )

    // ── Control setters (values already validated/clamped by the caller) ────

    fun setIso(iso: Int)                    { currentIso = iso;                 handler?.post { applyExposure() } }
    fun setShutter(ns: Long)                { currentShutterNs = ns;            handler?.post { applyExposure() } }
    fun setAuto()                           { currentIso = null; currentShutterNs = null; handler?.post { applyExposure() } }
    fun setOis(on: Boolean)                 { currentOis = on;                  handler?.post { applyExposure() } }
    fun setWbGains(gains: RggbChannelVector) { currentWbGains = gains;          handler?.post { applyExposure() } }
    fun setWbAuto()                         { currentWbGains = null;            handler?.post { applyExposure() } }
    fun setJpegQuality(q: Int)              { currentJpegQuality = q;           handler?.post { applyExposure() } }
    fun setFpsTarget(fps: Int)              { currentPhoneFps = fps;            handler?.post { applyExposure() } }
    fun setFocusMode(mode: String)          { currentFocusMode = mode;          handler?.post { applyExposure() } }
    fun setFocusDistance(d: Float)          { currentFocusDistance = d;         handler?.post { applyExposure() } }
    fun setNrMode(m: Int)                   { currentNrMode = m;                handler?.post { applyExposure() } }
    fun setEdgeMode(m: Int)                 { currentEdgeMode = m;              handler?.post { applyExposure() } }
    fun setAeComp(v: Int)                   { currentAeComp = v;                handler?.post { applyExposure() } }
    fun setBlackLevelLock(on: Boolean)      { currentBlackLevelLock = on;       handler?.post { applyExposure() } }
    fun setTorch(on: Boolean)               { currentTorch = on;                handler?.post { applyExposure() } }

    // ── Open / switch / preview ──────────────────────────────────────────

    /** Opens [cameraId] (or its [physicalCameraId] sub-camera) for the first time
     *  this session. [initialEntry]/[initialOis] seed [currentCamera]/[currentOis]
     *  before the async open begins. */
    fun open(cameraId: String, physicalCameraId: String?, initialEntry: CameraEntry, initialOis: Boolean) {
        currentCamera = initialEntry
        currentOis = initialOis
        openCamera(cameraId, physicalCameraId)
    }

    fun switchTo(entry: CameraEntry) {
        handler?.post { switchCameraTo(entry) }
    }

    /** Adds an extra output surface to the running capture session so a live preview
     *  can be shown without interrupting the MJPEG stream. */
    fun attachPreviewSurface(surface: Surface) {
        handler?.post { previewSurface = surface; reconfigureSession() }
    }

    /** [onDetached] runs after the surface has been dropped from the capture session and
     *  the session rebuilt without it - the caller can safely release the Surface then. */
    fun detachPreviewSurface(onDetached: (() -> Unit)? = null) {
        handler?.post {
            previewSurface = null
            reconfigureSession(onDetached)
        }
    }

    /** Tears down the open camera/session/reader and stops the camera handler thread.
     *  Does not touch the HTTP server or any service-level state - the caller
     *  (CameraStreamService.stopStreaming) handles that. */
    fun stop() {
        // Invalidates any open/session-configure callback already in flight (e.g. a
        // start immediately followed by a stop, before onOpened fired), so it can't
        // resurrect cameraDevice/captureSession using surfaces this call is about to
        // null out and hand Camera2 an empty or torn-down surface set.
        cameraGeneration++
        try { captureSession?.stopRepeating() } catch (_: Exception) {}
        try { captureSession?.close()         } catch (_: Exception) {}
        try { cameraDevice?.close()           } catch (_: Exception) {}
        try { imageReader?.close()            } catch (_: Exception) {}
        handlerThread?.quitSafely()
        captureSession = null; cameraDevice = null; imageReader = null
    }

    private fun openCamera(openCameraId: String, physicalCameraId: String?) {
        handlerThread = HandlerThread("CamThread").also { it.start() }
        handler       = Handler(handlerThread!!.looper)

        imageReader = ImageReader.newInstance(streamWidth, streamHeight, ImageFormat.JPEG, 3)
        imageReader!!.setOnImageAvailableListener({ reader ->
            val image = reader.acquireLatestImage() ?: return@setOnImageAvailableListener
            try {
                val buf   = image.planes[0].buffer
                val bytes = ByteArray(buf.remaining())
                buf.get(bytes)
                onFrame(bytes)
            } finally { image.close() }
        }, handler)

        val myGeneration = ++cameraGeneration
        val manager = context.getSystemService(Context.CAMERA_SERVICE) as CameraManager
        try {
            @Suppress("MissingPermission")
            manager.openCamera(openCameraId, object : CameraDevice.StateCallback() {
                override fun onOpened(camera: CameraDevice) {
                    if (myGeneration != cameraGeneration) { camera.close(); return }
                    cameraDevice = camera
                    onStateChanged(StreamState.ConfiguringSession, "openCamera.onOpened", null)
                    if (physicalCameraId != null && Build.VERSION.SDK_INT >= Build.VERSION_CODES.P)
                        createPhysicalSession(camera, physicalCameraId, myGeneration)
                    else
                        createLegacySession(camera, myGeneration)
                }
                override fun onDisconnected(camera: CameraDevice) {
                    camera.close()
                    if (myGeneration == cameraGeneration) {
                        cameraDevice = null
                        onStateChanged(StreamState.Failed, "openCamera.onDisconnected", null)
                    }
                }
                override fun onError(camera: CameraDevice, error: Int) {
                    camera.close()
                    if (myGeneration == cameraGeneration) {
                        cameraDevice = null
                        onStateChanged(StreamState.Failed, "openCamera.onError", RuntimeException("Camera2 error code $error"))
                        onFatalError()
                    }
                }
            }, handler)
        } catch (e: Exception) {
            onStateChanged(StreamState.Failed, "openCamera", e)
            onFatalError()
        }
    }

    private fun currentTargetSurfaces(): List<Surface> = listOfNotNull(imageReader?.surface, previewSurface)

    private fun createPhysicalSession(
        camera: CameraDevice, physId: String,
        generation: Int = cameraGeneration,
        mySession: Int = sessionGeneration,
        onComplete: (() -> Unit)? = null,
    ) {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.P) { createLegacySession(camera, generation, mySession, onComplete); return }
        // A stop/teardown racing in between the generation check in the caller and
        // this call can null out imageReader (and previewSurface) concurrently -
        // re-check staleness and bail rather than asking Camera2 to configure a
        // session with zero surfaces, which throws.
        if (generation != cameraGeneration) { onComplete?.invoke(); return }
        val outCfgs = currentTargetSurfaces().map { surface ->
            OutputConfiguration(surface).also { it.setPhysicalCameraId(physId) }
        }
        if (outCfgs.isEmpty()) { onComplete?.invoke(); return }
        val exec   = Executor { cmd -> handler?.post(cmd) }
        try {
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
                            onStateChanged(StreamState.Failed, "createPhysicalSession.onConfigureFailed", null)
                            onFatalError()
                        }
                        onComplete?.invoke()
                    }
                }
            ))
        } catch (e: Exception) {
            if (generation == cameraGeneration) {
                onStateChanged(StreamState.Failed, "createPhysicalSession", e)
                onFatalError()
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
        val targets = currentTargetSurfaces()
        if (targets.isEmpty()) { onComplete?.invoke(); return }
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
                            onStateChanged(StreamState.Failed, "createLegacySession.onConfigureFailed", null)
                            onFatalError()
                        }
                        onComplete?.invoke()
                    }
                }, handler)
        } catch (e: Exception) {
            if (generation == cameraGeneration) {
                onStateChanged(StreamState.Failed, "createLegacySession", e)
                onFatalError()
            }
            onComplete?.invoke()
        }
    }

    /** Tears down and rebuilds the capture session on the already-open camera device,
     *  e.g. when a preview surface is attached/detached. Does not reopen the device.
     *  [onComplete], if given, fires once this specific reconfigure attempt reaches a
     *  terminal state (configured, failed, or superseded) - not immediately after this
     *  call returns, since the rebuild itself is asynchronous. */
    private fun reconfigureSession(onComplete: (() -> Unit)? = null) {
        val camera = cameraDevice ?: run { onComplete?.invoke(); return }
        try { captureSession?.stopRepeating() } catch (_: Exception) {}
        try { captureSession?.close() } catch (_: Exception) {}
        captureSession = null

        val mySession = ++sessionGeneration
        val cam    = currentCamera
        val physId = if (cam?.logicalId != null) cam.id else null
        if (physId != null && Build.VERSION.SDK_INT >= Build.VERSION_CODES.P)
            createPhysicalSession(camera, physId, cameraGeneration, mySession, onComplete)
        else
            createLegacySession(camera, cameraGeneration, mySession, onComplete)
    }

    private fun startRepeating(camera: CameraDevice, session: CameraCaptureSession) {
        try {
            session.setRepeatingRequest(buildRequest(camera), ccmCaptureCallback, handler)
            // Only now - after Camera2 has actually accepted a repeating capture
            // request, not merely after the session finished configuring - is a
            // frame actually guaranteed to be on its way to sendFrame().
            onStateChanged(StreamState.Streaming, "startRepeating", null)
        } catch (e: CameraAccessException) {
            onStateChanged(StreamState.Failed, "startRepeating", e)
            onFatalError()
        }
    }

    private fun buildRequest(camera: CameraDevice = cameraDevice!!): CaptureRequest {
        return camera.createCaptureRequest(CameraDevice.TEMPLATE_PREVIEW).apply {
            addTarget(imageReader!!.surface)
            previewSurface?.let { addTarget(it) }

            val cam = currentCamera

            // Exposure and FPS
            // Use CONTROL_MODE_AUTO even in manual AE so that AF keeps running independently.
            set(CaptureRequest.CONTROL_MODE, CaptureRequest.CONTROL_MODE_AUTO)
            if (currentIso != null && currentShutterNs != null && cam != null && cam.supportsManualSensor) {
                val iso = CameraRequestSelection.clamp(currentIso!!, cam.isoMin, cam.isoMax)
                val sht = CameraRequestSelection.clamp(currentShutterNs!!, cam.shutterMinNs, cam.shutterMaxNs)
                set(CaptureRequest.CONTROL_AE_MODE, CaptureRequest.CONTROL_AE_MODE_OFF)
                set(CaptureRequest.SENSOR_SENSITIVITY,   iso)
                set(CaptureRequest.SENSOR_EXPOSURE_TIME, sht)
                // Enforce FPS via frame duration
                val targetFrameNs = 1_000_000_000L / currentPhoneFps
                set(CaptureRequest.SENSOR_FRAME_DURATION, targetFrameNs.coerceAtLeast(sht))
            } else {
                set(CaptureRequest.CONTROL_AE_MODE, CaptureRequest.CONTROL_AE_MODE_ON)
                // Use the closest range Camera2 actually advertises; an unsupported
                // range can make the capture request fail outright on some devices.
                val range = CameraRequestSelection.pickAeFpsRange(cam?.aeFpsRanges ?: emptyList(), currentPhoneFps)
                if (range != null) {
                    android.util.Log.d(TAG, "AE FPS range for ${cam?.id}: $range (target=$currentPhoneFps)")
                    set(CaptureRequest.CONTROL_AE_TARGET_FPS_RANGE, range)
                }
            }

            // Focus
            if (currentFocusMode == "manual" && cam != null && cam.supportsManualFocus) {
                set(CaptureRequest.CONTROL_AF_MODE, CaptureRequest.CONTROL_AF_MODE_OFF)
                set(CaptureRequest.LENS_FOCUS_DISTANCE,
                    CameraRequestSelection.clamp(currentFocusDistance, 0f, cam.minFocusDistance))
            } else {
                set(CaptureRequest.CONTROL_AF_MODE,
                    CameraRequestSelection.pickAfMode(cam?.afModes ?: emptySet(), wantContinuousVideo = true))
            }

            // JPEG quality
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

            // Noise reduction and edge enhancement - set only when the camera advertises
            // a usable mode; otherwise omit the key.
            CameraRequestSelection.pickNrMode(cam?.nrModes ?: emptySet(), currentNrMode)?.let {
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

            // Black level lock
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
        }
    }

    private fun applyExposure() {
        val s = captureSession ?: return
        val c = cameraDevice  ?: return
        try {
            s.setRepeatingRequest(buildRequest(c), ccmCaptureCallback, handler)
        } catch (e: Exception) {
            // The stream keeps running on the previous repeating request, so this
            // is non-fatal - but the requested control change silently didn't take
            // effect. Surface it in diagnostics instead of swallowing it whole.
            android.util.Log.w(TAG, "applyExposure failed to update live controls", e)
            onControlError("applyExposure", e)
        }
    }

    private fun switchCameraTo(entry: CameraEntry) {
        // Drop manual/flash/focus state the new lens can't honor, so buildRequest()
        // and snapshot() don't disagree on what's actually active.
        //
        // currentOis is deliberately NOT reset: buildRequest() only requests OIS-on
        // when cam.hasOis is true, so leaving the toggle alone lets OIS resume
        // automatically on switching back to an OIS-capable lens.
        if (!entry.supportsFlash) currentTorch = false
        if (!entry.supportsManualSensor) { currentIso = null; currentShutterNs = null }
        if (!entry.supportsManualFocus && currentFocusMode == "manual") currentFocusMode = "continuous"
        currentCamera = entry
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
                    if (physId != null && Build.VERSION.SDK_INT >= Build.VERSION_CODES.P)
                        createPhysicalSession(camera, physId, myGeneration)
                    else
                        createLegacySession(camera, myGeneration)
                }
                override fun onDisconnected(camera: CameraDevice) {
                    camera.close()
                    if (myGeneration == cameraGeneration) {
                        cameraDevice = null
                        onStateChanged(StreamState.Failed, "switchCameraTo.onDisconnected", null)
                    }
                }
                override fun onError(camera: CameraDevice, error: Int) {
                    camera.close()
                    if (myGeneration == cameraGeneration) {
                        cameraDevice = null
                        onStateChanged(StreamState.Failed, "switchCameraTo.onError", RuntimeException("Camera2 error code $error"))
                    }
                }
            }, handler)
        } catch (e: Exception) {
            onStateChanged(StreamState.Failed, "switchCameraTo", e)
        }
    }
}
