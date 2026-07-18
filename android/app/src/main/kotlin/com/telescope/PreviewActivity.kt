package com.telescope

import android.content.BroadcastReceiver
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.ServiceConnection
import android.content.pm.PackageManager
import android.graphics.Matrix
import android.graphics.SurfaceTexture
import android.hardware.camera2.CameraCaptureSession
import android.hardware.camera2.CameraCharacteristics
import android.hardware.camera2.CameraDevice
import android.hardware.camera2.CameraManager
import android.hardware.camera2.CaptureRequest
import android.hardware.camera2.params.OutputConfiguration
import android.hardware.camera2.params.SessionConfiguration
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.HandlerThread
import android.os.IBinder
import android.util.Log
import android.util.Size
import android.view.Gravity
import android.view.Surface
import android.view.TextureView
import android.view.View
import android.widget.FrameLayout
import android.widget.ImageButton
import android.widget.LinearLayout
import android.widget.TextView
import android.widget.Toast
import androidx.activity.OnBackPressedCallback
import androidx.appcompat.app.AppCompatActivity
import androidx.core.content.ContextCompat
import java.util.concurrent.Executor

/**
 * Fullscreen, overlay-free live preview to help aim the phone's cameras without
 * looking at the desktop app. If a stream is already active, this attaches an extra
 * output surface to the running CameraStreamService session (stream keeps running).
 * Otherwise it opens a short-lived standalone Camera2 session of its own.
 */
    // Labels use the familiar landscape names ("16:9", "4:3") but the ratio values are
    // portrait (9:16, 3:4) - this app only shows portrait crops.
    private enum class AspectOption(val label: String, val ratio: Float) {
        R16_9("16:9", 9f / 16f),
        R4_3("4:3", 3f / 4f),
    }

class PreviewActivity : AppCompatActivity() {

    private lateinit var textureView: TextureView
    private lateinit var btnClose: ImageButton
    private lateinit var lensContainer: LinearLayout
    private lateinit var btnAspect: TextView

    private var aspectIndex = 0

    private var service: CameraStreamService? = null
    private var bound = false
    private var boundToRunningStream = false
    private var resolved = false
    private var pendingSurface: Surface? = null
    // Guards tearDownPreview() so a second call (it can run from both onStop() and
    // onSurfaceTextureDestroyed()) is a no-op instead of redundantly detaching/closing.
    private var tornDown = false

    // True once the aspect-ratio crop layout has landed, not just requested.
    // tryResolve() must wait: TextureView.onSizeChanged() resets the SurfaceTexture's
    // default buffer size on every resize, so attaching before the resize lands can lock
    // the session to the pre-crop size while the buffer is silently deformed.
    private var layoutSettled = false
    private var pendingAspectSize: Size? = null

    // Remembered so the transform can be recomputed when the aspect ratio (and
    // therefore the TextureView's on-screen size) changes, without re-deriving them.
    private var lastCameraId: String? = null
    private var lastBufferSize: Size? = null

    // Standalone (service not streaming) camera state
    private var cameras = listOf<CameraInfo>()
    private var currentCameraId: String? = null
    private var cameraDevice: CameraDevice? = null
    private var captureSession: CameraCaptureSession? = null
    private var handlerThread: HandlerThread? = null
    private var handler: Handler? = null
    // Bumped on every standalone camera-open attempt so a stale onOpened/onConfigured
    // from a superseded lens switch can't clobber the camera that's actually current.
    private var standaloneGeneration = 0

    // Without a lock screen covering the activity, turning the display off only
    // triggers onPause(), not onStop() — the camera session and TextureView are
    // left alive but the surface stalls, so the preview comes back frozen until
    // manually exited and reopened. Since there's no reason to keep this preview
    // open with the screen off, just close it outright when that happens.
    private val screenOffReceiver = object : BroadcastReceiver() {
        override fun onReceive(context: Context?, intent: Intent?) {
            if (intent?.action == Intent.ACTION_SCREEN_OFF) finish()
        }
    }

    private val serviceConnection = object : ServiceConnection {
        override fun onServiceConnected(name: ComponentName?, binder: IBinder?) {
            service = (binder as CameraStreamService.LocalBinder).getService()
            bound = true
            if (service?.isStreaming == true) {
                // A running stream's buffer is already fixed at streaming resolution, so
                // the aspect picker (unlike standalone mode, which opens a new session per
                // ratio) can't actually change what's captured - skip it and leave the
                // TextureView full-screen, which also avoids a resize racing session creation.
                btnAspect.visibility = View.GONE
                layoutSettled = true
            } else {
                btnAspect.visibility = View.VISIBLE
                beginStandaloneAspectLayout()
            }
            tryResolve()
        }
        override fun onServiceDisconnected(name: ComponentName?) {
            service = null
            bound = false
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_preview)

        textureView   = findViewById(R.id.textureView)
        btnClose      = findViewById(R.id.btnClosePreview)
        lensContainer = findViewById(R.id.layoutLensPills)
        btnAspect     = findViewById(R.id.btnAspect)

        // Hidden until onServiceConnected confirms we're in the standalone (not currently
        // streaming) case, where picking a ratio actually changes what's captured.
        btnAspect.visibility = View.GONE
        btnAspect.text = AspectOption.entries[aspectIndex].label
        btnAspect.setOnClickListener {
            aspectIndex = (aspectIndex + 1) % AspectOption.entries.size
            applyAspectOption()
        }

        btnClose.setOnClickListener { finish() }
        // Back should just leave this screen, same as the X button - it must never
        // touch the stream (started independently by MainActivity via startForegroundService).
        onBackPressedDispatcher.addCallback(this, object : OnBackPressedCallback(true) {
            override fun handleOnBackPressed() { finish() }
        })

        textureView.surfaceTextureListener = object : TextureView.SurfaceTextureListener {
            override fun onSurfaceTextureAvailable(st: SurfaceTexture, w: Int, h: Int) {
                tornDown = false
                pendingSurface = Surface(st)
                tryResolve()
            }
            override fun onSurfaceTextureSizeChanged(st: SurfaceTexture, w: Int, h: Int) {
                applyPreviewTransform(lastCameraId, lastBufferSize)
            }
            override fun onSurfaceTextureDestroyed(st: SurfaceTexture): Boolean {
                tearDownPreview()
                return true
            }
            override fun onSurfaceTextureUpdated(st: SurfaceTexture) {}
        }
    }

    override fun onStart() {
        super.onStart()
        if (ContextCompat.checkSelfPermission(this, android.Manifest.permission.CAMERA)
                != PackageManager.PERMISSION_GRANTED) {
            Toast.makeText(this, "Camera permission required.", Toast.LENGTH_SHORT).show()
            finish()
            return
        }
        // BIND_AUTO_CREATE so onServiceConnected always fires even when nothing is streaming;
        // a plain flags=0 bind can return true and never connect, making "not streaming"
        // indistinguishable from "still connecting". If nothing was running this only
        // triggers the service's cheap onCreate(), and tryResolve() unbinds it immediately.
        bindService(Intent(this, CameraStreamService::class.java), serviceConnection, Context.BIND_AUTO_CREATE)
        registerReceiver(screenOffReceiver, IntentFilter(Intent.ACTION_SCREEN_OFF))
    }

    override fun onStop() {
        unregisterReceiver(screenOffReceiver)
        tearDownPreview()
        if (bound) { unbindService(serviceConnection); bound = false }
        service = null
        resolved = false
        boundToRunningStream = false
        // Back out to the main screen rather than leave a dead preview on screen-off or
        // backgrounding. Only this activity's own camera/session was torn down above; a
        // live stream is owned independently by CameraStreamService and keeps running.
        finish()
        super.onStop()
    }

    // ── Resolve bound-to-stream vs standalone ────────────────────────────────

    private fun tryResolve() {
        if (resolved) return
        // Bound-to-stream case sets this immediately (no crop applied there); standalone
        // case waits for its aspect-ratio crop layout to actually land - see onServiceConnected.
        if (!layoutSettled) return
        val surface = pendingSurface ?: return

        if (!bound) return // still waiting for onServiceConnected

        val svc = service
        resolved = true
        if (svc?.isStreaming == true) {
            boundToRunningStream = true
            val streamSize = svc.getStreamSize()
            // Must be set before attaching: Camera2 needs the buffer size fixed to a
            // supported size before the surface is used as a session output target.
            // No crop is applied in this path (see onServiceConnected), so there's no
            // pending resize that could overwrite it first.
            textureView.surfaceTexture?.setDefaultBufferSize(streamSize.width, streamSize.height)
            svc.attachPreviewSurface(surface)
            setupLensPillsFromService(streamSize)
        } else {
            unbindService(serviceConnection)
            bound = false
            startStandalone(surface)
        }
    }

    private fun tearDownPreview() {
        if (tornDown) return
        tornDown = true
        val surface = pendingSurface
        pendingSurface = null
        if (boundToRunningStream) {
            // The service owns detaching the surface from its live session; only release
            // the Surface itself once that's done (or immediately if it's already gone).
            val svc = service
            if (svc != null) svc.detachPreviewSurface { surface?.release() }
            else surface?.release()
        } else {
            closeStandaloneCamera()
            surface?.release()
        }
    }

    // ── Bound-to-running-stream lens switching ───────────────────────────────

    private fun setupLensPillsFromService(bufferSize: Size) {
        val svc = service ?: return
        currentCameraId = svc.getCurrentCameraId()
        applyPreviewTransform(currentCameraId, bufferSize)
        buildLensPills(svc.getCameras().map { it.id to it.label }) { id ->
            currentCameraId = id
            svc.switchCamera(id)
            applyPreviewTransform(id, bufferSize)
            refreshPillSelection()
        }
    }

    // ── Standalone camera (no stream running) ────────────────────────────────

    private fun startStandalone(surface: Surface) {
        val manager = getSystemService(CAMERA_SERVICE) as CameraManager
        cameras = CameraCatalog.enumerate(manager)
        if (cameras.isEmpty()) {
            Toast.makeText(this, "No cameras found.", Toast.LENGTH_SHORT).show()
            return
        }

        handlerThread = HandlerThread("PreviewCamThread").also { it.start() }
        handler = Handler(handlerThread!!.looper)

        buildLensPills(cameras.map { it.id to it.label }) { id -> switchStandaloneCamera(id, surface) }
        switchStandaloneCamera(cameras.first().id, surface)
    }

    private fun switchStandaloneCamera(id: String, surface: Surface) {
        val cam = cameras.find { it.id == id } ?: return
        currentCameraId = id
        refreshPillSelection()
        closeStandaloneCamera(keepThread = true)
        openStandaloneCamera(cam, surface)
    }

    private fun openStandaloneCamera(cam: CameraInfo, surface: Surface) {
        val size = cam.supportedSizes.firstOrNull { it.width <= 1920 } ?: cam.supportedSizes.first()
        textureView.surfaceTexture?.setDefaultBufferSize(size.width, size.height)
        applyPreviewTransform(cam.id, size)

        val myGeneration = ++standaloneGeneration
        val manager = getSystemService(CAMERA_SERVICE) as CameraManager
        val openId = cam.logicalId ?: cam.id
        val physId = if (cam.logicalId != null) cam.id else null
        try {
            @Suppress("MissingPermission")
            manager.openCamera(openId, object : CameraDevice.StateCallback() {
                override fun onOpened(camera: CameraDevice) {
                    if (myGeneration != standaloneGeneration) { camera.close(); return }
                    cameraDevice = camera
                    openStandaloneSession(camera, surface, physId, myGeneration)
                }
                override fun onDisconnected(camera: CameraDevice) {
                    camera.close()
                    if (myGeneration == standaloneGeneration) cameraDevice = null
                }
                override fun onError(camera: CameraDevice, error: Int) {
                    camera.close()
                    if (myGeneration == standaloneGeneration) cameraDevice = null
                }
            }, handler)
        } catch (_: Exception) {}
    }

    private fun openStandaloneSession(camera: CameraDevice, surface: Surface, physId: String?, generation: Int) {
        val callback = object : CameraCaptureSession.StateCallback() {
            override fun onConfigured(s: CameraCaptureSession) {
                if (generation != standaloneGeneration) { s.close(); return }
                captureSession = s
                val request = camera.createCaptureRequest(CameraDevice.TEMPLATE_PREVIEW).apply {
                    addTarget(surface)
                    set(CaptureRequest.CONTROL_AF_MODE, CaptureRequest.CONTROL_AF_MODE_CONTINUOUS_PICTURE)
                    set(CaptureRequest.CONTROL_AE_MODE, CaptureRequest.CONTROL_AE_MODE_ON)
                }.build()
                try { s.setRepeatingRequest(request, null, handler) } catch (_: Exception) {}
            }
            override fun onConfigureFailed(s: CameraCaptureSession) {}
        }
        if (physId != null && Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
            val outCfg = OutputConfiguration(surface).also { it.setPhysicalCameraId(physId) }
            val exec = Executor { cmd -> handler?.post(cmd) }
            camera.createCaptureSession(SessionConfiguration(
                SessionConfiguration.SESSION_REGULAR, listOf(outCfg), exec, callback))
        } else {
            @Suppress("DEPRECATION")
            camera.createCaptureSession(listOf(surface), callback, handler)
        }
    }

    private fun closeStandaloneCamera(keepThread: Boolean = false) {
        // Invalidate any in-flight open/session-configure from a camera we're now
        // abandoning so a late callback can't resurrect it after this returns.
        standaloneGeneration++
        try { captureSession?.stopRepeating() } catch (_: Exception) {}
        try { captureSession?.close() } catch (_: Exception) {}
        try { cameraDevice?.close() } catch (_: Exception) {}
        captureSession = null
        cameraDevice = null
        if (!keepThread) {
            handlerThread?.quitSafely()
            handlerThread = null
            handler = null
        }
    }

    // ── Preview scale correction ────────────────────────────────────────────

    private fun applyPreviewTransform(cameraId: String?, bufferSize: Size?, retryCount: Int = 0) {
        if (cameraId == null || bufferSize == null) return
        lastCameraId = cameraId
        lastBufferSize = bufferSize
        val viewWidth  = textureView.width.toFloat()
        val viewHeight = textureView.height.toFloat()
        if (viewWidth == 0f || viewHeight == 0f) return
        try {
            val manager = getSystemService(CAMERA_SERVICE) as CameraManager
            val sensorOrientation = manager.getCameraCharacteristics(cameraId)
                .get(CameraCharacteristics.SENSOR_ORIENTATION) ?: 0
            // TextureView already auto-rotates the buffer for sensor orientation, so no
            // manual rotation is applied here (display rotation is always 0, app is
            // portrait-locked) - only the resulting stretch needs a corrective scale.
            val axesSwapped = sensorOrientation == 90 || sensorOrientation == 270

            val bufW = bufferSize.width.toFloat()
            val bufH = bufferSize.height.toFloat()
            val scaleX = if (axesSwapped) viewWidth  / bufH else viewWidth  / bufW
            val scaleY = if (axesSwapped) viewHeight / bufW else viewHeight / bufH
            // Standalone mode already crops the view to the target aspect box, so "cover"
            // (max) fills it with a near-zero mismatch. Bound mode has no crop box - full
            // screen, arbitrary aspect - so "cover" would crop the stream; use "contain" (min).
            val finalScale = if (boundToRunningStream) minOf(scaleX, scaleY) else maxOf(scaleX, scaleY)

            val matrix = Matrix()
            matrix.setScale(finalScale / scaleX, finalScale / scaleY,
                viewWidth / 2f, viewHeight / 2f)
            textureView.setTransform(matrix)
        } catch (e: Exception) {
            Log.w(TAG, "Preview transform failed for camera $cameraId (attempt ${retryCount + 1})", e)
            if (retryCount < MAX_TRANSFORM_RETRIES) {
                // Camera characteristics lookups can transiently fail during a lens switch
                // or session reconfiguration; retry rather than leave the view stretched.
                textureView.postDelayed({
                    // Bail if a newer call has already superseded this one (e.g. another lens switch).
                    if (cameraId == lastCameraId && bufferSize == lastBufferSize) {
                        applyPreviewTransform(cameraId, bufferSize, retryCount + 1)
                    }
                }, TRANSFORM_RETRY_DELAY_MS)
            } else {
                Toast.makeText(this,
                    "Preview may look stretched - couldn't read camera info.",
                    Toast.LENGTH_SHORT).show()
            }
        }
    }

    // ── Aspect ratio crop ────────────────────────────────────────────────────
    //
    // Resizes the TextureView to the largest box of the chosen ratio that fits the
    // screen; applyPreviewTransform() then "cover"-fills that box, giving a true crop
    // instead of guide lines. onSurfaceTextureSizeChanged reapplies the transform once
    // the resize lands.

    // Only called for the standalone (not currently streaming) case - see onServiceConnected.
    private fun beginStandaloneAspectLayout() {
        // Deferred until after layout so the TextureView's parent has a real size;
        // calling this synchronously here would no-op against a still-zero-sized view.
        textureView.post { applyAspectOption() }
        // post{} alone isn't a sufficient barrier: requestLayout() only schedules a
        // traversal for the next Choreographer frame. Wait for the TextureView's bounds
        // to actually match the target size before letting tryResolve() open the camera,
        // or its pending onSizeChanged could still overwrite the buffer size mid-session.
        textureView.addOnLayoutChangeListener(object : View.OnLayoutChangeListener {
            override fun onLayoutChange(
                v: View, left: Int, top: Int, right: Int, bottom: Int,
                oldLeft: Int, oldTop: Int, oldRight: Int, oldBottom: Int
            ) {
                val target = pendingAspectSize ?: return
                if (right - left == target.width && bottom - top == target.height) {
                    layoutSettled = true
                    v.removeOnLayoutChangeListener(this)
                    tryResolve()
                }
            }
        })
    }

    private fun applyAspectOption() {
        val option = AspectOption.entries[aspectIndex]
        btnAspect.text = option.label
        setAspectRatio(option)
    }

    private fun setAspectRatio(option: AspectOption) {
        val root = textureView.parent as? FrameLayout ?: return
        val screenW = root.width
        val screenH = root.height
        if (screenW == 0 || screenH == 0) return

        var w = screenW
        var h = (w / option.ratio).toInt()
        if (h > screenH) { h = screenH; w = (h * option.ratio).toInt() }

        pendingAspectSize = Size(w, h)
        val lp = textureView.layoutParams as FrameLayout.LayoutParams
        lp.width = w
        lp.height = h
        lp.gravity = Gravity.CENTER
        textureView.layoutParams = lp
    }

    // ── Lens pill UI ──────────────────────────────────────────────────────────

    private fun buildLensPills(entries: List<Pair<String, String>>, onSelect: (String) -> Unit) {
        lensContainer.removeAllViews()
        entries.forEach { (id, label) ->
            val pill = TextView(this).apply {
                text = shortLabel(label)
                tag = id
                textSize = 13f
                setPadding(28, 16, 28, 16)
                setOnClickListener { onSelect(id) }
                layoutParams = LinearLayout.LayoutParams(
                    LinearLayout.LayoutParams.WRAP_CONTENT, LinearLayout.LayoutParams.WRAP_CONTENT
                ).apply { marginEnd = 10 }
            }
            lensContainer.addView(pill)
        }
        refreshPillSelection()
    }

    private fun refreshPillSelection() {
        for (i in 0 until lensContainer.childCount) {
            val v = lensContainer.getChildAt(i) as? TextView ?: continue
            val selected = v.tag == currentCameraId
            v.setTextColor(resources.getColor(
                if (selected) R.color.colorOnPrimary else R.color.colorOnSurface, theme))
            v.background = resources.getDrawable(
                if (selected) R.drawable.pill_lens_selected else R.drawable.pill_lens, theme)
        }
    }

    private fun shortLabel(raw: String): String =
        raw.replace(Regex("^ID \\d+\\s+"), "")
           .replace(Regex("\\[phys[^]]*\\]"), "")
           .replace(Regex("\\s{2,}"), " ")
           .trim()

    companion object {
        private const val TAG = "PreviewActivity"
        private const val MAX_TRANSFORM_RETRIES = 3
        private const val TRANSFORM_RETRY_DELAY_MS = 150L
    }
}
