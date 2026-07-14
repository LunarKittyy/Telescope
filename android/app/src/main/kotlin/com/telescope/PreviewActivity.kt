package com.telescope

import android.content.ComponentName
import android.content.Context
import android.content.Intent
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
import android.util.Size
import android.view.Gravity
import android.view.Surface
import android.view.TextureView
import android.widget.AdapterView
import android.widget.ArrayAdapter
import android.widget.FrameLayout
import android.widget.ImageButton
import android.widget.LinearLayout
import android.widget.Spinner
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
    private enum class AspectOption(val label: String, val ratio: Float?) {
        FILL("Fill screen", null),
        R16_9("16:9", 16f / 9f),
        R4_3("4:3", 4f / 3f),
        R3_4("3:4", 3f / 4f),
        R1_1("1:1", 1f),
        R9_16("9:16", 9f / 16f),
    }

class PreviewActivity : AppCompatActivity() {

    private lateinit var textureView: TextureView
    private lateinit var btnClose: ImageButton
    private lateinit var lensContainer: LinearLayout
    private lateinit var spinnerAspect: Spinner

    private var service: CameraStreamService? = null
    private var bound = false
    private var boundToRunningStream = false
    private var resolved = false
    private var pendingSurface: Surface? = null

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

    private val serviceConnection = object : ServiceConnection {
        override fun onServiceConnected(name: ComponentName?, binder: IBinder?) {
            service = (binder as CameraStreamService.LocalBinder).getService()
            bound = true
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
        spinnerAspect = findViewById(R.id.spinnerAspect)

        spinnerAspect.adapter = ArrayAdapter(this,
            R.layout.spinner_item, AspectOption.entries.map { it.label }
        ).also { it.setDropDownViewResource(R.layout.spinner_dropdown_item) }
        spinnerAspect.onItemSelectedListener = object : AdapterView.OnItemSelectedListener {
            override fun onItemSelected(p: AdapterView<*>?, v: android.view.View?, pos: Int, id: Long) {
                setAspectRatio(AspectOption.entries[pos])
            }
            override fun onNothingSelected(p: AdapterView<*>?) {}
        }

        btnClose.setOnClickListener { finish() }
        // Back should just leave this screen, same as the X button — it must never
        // touch the stream (started independently by MainActivity via startForegroundService).
        onBackPressedDispatcher.addCallback(this, object : OnBackPressedCallback(true) {
            override fun handleOnBackPressed() { finish() }
        })

        textureView.surfaceTextureListener = object : TextureView.SurfaceTextureListener {
            override fun onSurfaceTextureAvailable(st: SurfaceTexture, w: Int, h: Int) {
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
        // BIND_AUTO_CREATE so onServiceConnected always fires, even if nothing is streaming —
        // that's how we can reliably tell "not streaming" apart from "still connecting" (a
        // plain flags=0 bind can return true and then just never connect when nothing is
        // running, which silently broke the standalone fallback below). If nothing was running,
        // this only calls the service's onCreate() (cheap, no camera/notification touched) and
        // it's immediately unbound in tryResolve() once we see isStreaming == false, so it
        // tears itself back down — it can never linger or interfere with a real stream.
        bindService(Intent(this, CameraStreamService::class.java), serviceConnection, Context.BIND_AUTO_CREATE)
    }

    override fun onStop() {
        tearDownPreview()
        if (bound) { unbindService(serviceConnection); bound = false }
        service = null
        resolved = false
        boundToRunningStream = false
        super.onStop()
    }

    // ── Resolve bound-to-stream vs standalone ────────────────────────────────

    private fun tryResolve() {
        if (resolved) return
        val surface = pendingSurface ?: return

        if (!bound) return // still waiting for onServiceConnected

        val svc = service
        resolved = true
        if (svc?.isStreaming == true) {
            boundToRunningStream = true
            val streamSize = svc.getStreamSize()
            // Must be set before attaching — Camera2 needs the surface's buffer size
            // fixed to a supported size before it's used as a session output target.
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
        pendingSurface = null
        if (boundToRunningStream) {
            service?.detachPreviewSurface()
        } else {
            closeStandaloneCamera()
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

        val manager = getSystemService(CAMERA_SERVICE) as CameraManager
        val openId = cam.logicalId ?: cam.id
        val physId = if (cam.logicalId != null) cam.id else null
        try {
            @Suppress("MissingPermission")
            manager.openCamera(openId, object : CameraDevice.StateCallback() {
                override fun onOpened(camera: CameraDevice) {
                    cameraDevice = camera
                    openStandaloneSession(camera, surface, physId)
                }
                override fun onDisconnected(camera: CameraDevice) { camera.close(); cameraDevice = null }
                override fun onError(camera: CameraDevice, error: Int) { camera.close(); cameraDevice = null }
            }, handler)
        } catch (_: Exception) {}
    }

    private fun openStandaloneSession(camera: CameraDevice, surface: Surface, physId: String?) {
        val callback = object : CameraCaptureSession.StateCallback() {
            override fun onConfigured(s: CameraCaptureSession) {
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
    //
    // TextureView already auto-rotates the buffer to compensate SENSOR_ORIENTATION
    // (typically 90° on phones, since the sensor is mounted sideways) — no manual
    // rotation is needed, and adding one double-rotates the image. What TextureView
    // does NOT do is account for that auto-rotation when it stretches the buffer to
    // fill the view, so a corrective scale (not rotation) is what fixes the aspect
    // ratio. The app is portrait-locked, so display rotation is always 0 and doesn't
    // factor in here.

    private fun applyPreviewTransform(cameraId: String?, bufferSize: Size?) {
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
            val axesSwapped = sensorOrientation == 90 || sensorOrientation == 270

            val bufW = bufferSize.width.toFloat()
            val bufH = bufferSize.height.toFloat()
            val scaleX = if (axesSwapped) viewWidth  / bufH else viewWidth  / bufW
            val scaleY = if (axesSwapped) viewHeight / bufW else viewHeight / bufH
            val finalScale = maxOf(scaleX, scaleY)

            val matrix = Matrix()
            matrix.setScale(finalScale / scaleX, finalScale / scaleY,
                viewWidth / 2f, viewHeight / 2f)
            textureView.setTransform(matrix)
        } catch (_: Exception) {}
    }

    // ── Aspect ratio crop ────────────────────────────────────────────────────
    //
    // Resizes the TextureView itself to the largest box of the chosen ratio that fits
    // within the screen, centered and letterboxed against the black background. The
    // buffer-fill transform in applyPreviewTransform() then "cover"-fills that smaller
    // box instead of the full screen, which is what gives a true crop preview rather
    // than just guide lines. Re-applying the transform happens automatically once the
    // resize lands, via onSurfaceTextureSizeChanged.

    private fun setAspectRatio(option: AspectOption) {
        val root = textureView.parent as? FrameLayout ?: return
        val screenW = root.width
        val screenH = root.height
        if (screenW == 0 || screenH == 0) return

        val (boxW, boxH) = if (option.ratio == null) {
            screenW to screenH
        } else {
            var w = screenW
            var h = (w / option.ratio).toInt()
            if (h > screenH) { h = screenH; w = (h * option.ratio).toInt() }
            w to h
        }

        val lp = textureView.layoutParams as FrameLayout.LayoutParams
        lp.width = boxW
        lp.height = boxH
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
}
