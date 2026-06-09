package com.phonecam

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Intent
import android.content.pm.ServiceInfo
import android.graphics.ImageFormat
import android.hardware.camera2.*
import android.hardware.camera2.params.OutputConfiguration
import android.hardware.camera2.params.RggbChannelVector
import android.hardware.camera2.params.SessionConfiguration
import android.media.ImageReader
import android.os.Binder
import android.os.Build
import android.os.Handler
import android.os.HandlerThread
import android.os.IBinder
import android.os.PowerManager
import androidx.core.app.NotificationCompat
import java.util.concurrent.Executor
import kotlin.math.ln
import kotlin.math.pow
import kotlin.math.sqrt

// ── Camera catalogue entry ────────────────────────────────────────────────────
data class CameraEntry(
    val id: String,
    val logicalId: String?,
    val label: String,
    val hasOis: Boolean,
    val isoMin: Int,
    val isoMax: Int,
    val shutterMinNs: Long,
    val shutterMaxNs: Long,
)

class CameraStreamService : Service() {

    companion object {
        const val EXTRA_CAMERA_ID  = "camera_id"
        const val EXTRA_LOGICAL_ID = "logical_id"
        const val EXTRA_WIDTH      = "width"
        const val EXTRA_HEIGHT     = "height"
        const val EXTRA_OIS        = "ois"
        const val CHANNEL_ID       = "phonecam_stream"
        const val NOTIF_ID         = 1
        const val DEFAULT_PORT     = 8080

        // Kelvin → RGGB gains (Tanner Helland algorithm, normalised to green=1.0)
        fun kelvinToRggb(kelvin: Int): RggbChannelVector {
            val t = kelvin.coerceIn(1000, 40000).toDouble() / 100.0
            val r = if (t <= 66) 255.0
                    else (329.698727446 * (t - 60.0).pow(-0.1332047592)).coerceIn(0.0, 255.0)
            val b = when {
                t >= 66 -> 255.0
                t <= 19 -> 0.0
                else    -> (138.5177312231 * ln(t - 10.0) - 305.0447927307).coerceIn(0.0, 255.0)
            }
            // Green stays at 255 across visible range; normalise each channel to green.
            val rG = (r / 255.0).toFloat().coerceAtLeast(0.05f)
            val bG = (b / 255.0).toFloat().coerceAtLeast(0.05f)
            return RggbChannelVector(rG, 1.0f, 1.0f, bG)
        }
    }

    inner class LocalBinder : Binder() {
        fun getService(): CameraStreamService = this@CameraStreamService
    }
    private val binder = LocalBinder()

    // Camera hardware
    private var cameraDevice: CameraDevice? = null
    private var captureSession: CameraCaptureSession? = null
    private var imageReader: ImageReader? = null
    private var handlerThread: HandlerThread? = null
    private var handler: Handler? = null
    private var server: MjpegServer? = null
    private var wakeLock: PowerManager.WakeLock? = null

    // Stream config
    private var streamWidth  = 1920
    private var streamHeight = 1080

    // Exposure state — null ISO/shutter = auto AE
    @Volatile private var currentIso:       Int?  = null
    @Volatile private var currentShutterNs: Long? = null
    @Volatile private var currentOis:       Boolean = true
    // WB state — null Kelvin = auto AWB
    @Volatile private var currentWbKelvin:  Int?  = null

    // Camera catalogue
    private var allCameras:    List<CameraEntry> = emptyList()
    private var currentCamera: CameraEntry?      = null

    var isStreaming = false
        private set
    val port: Int get() = DEFAULT_PORT

    // ── Lifecycle ─────────────────────────────────────────────────────────────

    override fun onBind(intent: Intent?): IBinder = binder
    override fun onCreate() { super.onCreate(); createNotificationChannel() }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        val cameraId  = intent?.getStringExtra(EXTRA_CAMERA_ID)  ?: "0"
        val logicalId = intent?.getStringExtra(EXTRA_LOGICAL_ID) ?: ""
        streamWidth   = intent?.getIntExtra(EXTRA_WIDTH,  1920)  ?: 1920
        streamHeight  = intent?.getIntExtra(EXTRA_HEIGHT, 1080)  ?: 1080
        currentOis    = intent?.getBooleanExtra(EXTRA_OIS, true) ?: true

        enumerateAllCameras()
        startForegroundCompat()
        acquireWakeLock()

        val physId = if (logicalId.isNotEmpty()) cameraId else null
        val openId = if (logicalId.isNotEmpty()) logicalId else cameraId
        currentCamera = allCameras.find { it.id == cameraId }
            ?: CameraEntry(cameraId, logicalId.ifEmpty { null }, "ID $cameraId",
                           currentOis, 50, 3200, 100_000L, 1_000_000_000L)

        openCamera(openId, physId)
        isStreaming = true
        return START_NOT_STICKY
    }

    override fun onDestroy() { stopStreaming(); super.onDestroy() }

    // ── Camera enumeration ────────────────────────────────────────────────────

    private fun enumerateAllCameras() {
        val manager = getSystemService(CAMERA_SERVICE) as CameraManager
        val result  = mutableListOf<CameraEntry>()

        fun buildEntry(id: String, logicalParent: String?): CameraEntry? = runCatching {
            val chars  = manager.getCameraCharacteristics(id)
            val facing = when (chars.get(CameraCharacteristics.LENS_FACING)) {
                CameraCharacteristics.LENS_FACING_BACK  -> "Back"
                CameraCharacteristics.LENS_FACING_FRONT -> "Front"
                else -> "Ext"
            }
            val focalRaw = chars.get(CameraCharacteristics.LENS_INFO_AVAILABLE_FOCAL_LENGTHS)?.firstOrNull() ?: 0f
            val sensor   = chars.get(CameraCharacteristics.SENSOR_INFO_PHYSICAL_SIZE)
            val focalEq  = if (sensor != null && focalRaw > 0f) {
                val diag = sqrt((sensor.width * sensor.width + sensor.height * sensor.height).toDouble()).toFloat()
                (focalRaw * 43.27f / diag).toInt()
            } else 0

            val oisModes = chars.get(CameraCharacteristics.LENS_INFO_AVAILABLE_OPTICAL_STABILIZATION)
            val hasOis   = oisModes?.contains(1) == true

            val isoRange = chars.get(CameraCharacteristics.SENSOR_INFO_SENSITIVITY_RANGE)
            val isoMin   = isoRange?.lower ?: 50
            val isoMax   = isoRange?.upper ?: 3200

            val shtRange   = chars.get(CameraCharacteristics.SENSOR_INFO_EXPOSURE_TIME_RANGE)
            val shtMinNs   = shtRange?.lower ?: 100_000L
            val shtMaxNs   = shtRange?.upper ?: 1_000_000_000L

            val fStr = if (focalEq > 0) "~${focalEq}mm" else "?"
            val oStr = if (hasOis) " OIS" else ""
            val pStr = if (logicalParent != null) " [phys]" else ""
            CameraEntry(id, logicalParent, "$facing $fStr$oStr$pStr", hasOis,
                        isoMin, isoMax, shtMinNs, shtMaxNs)
        }.getOrNull()

        manager.cameraIdList.forEach { id ->
            buildEntry(id, null)?.let { result += it }
        }
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
            manager.cameraIdList.forEach { logId ->
                runCatching {
                    val chars = manager.getCameraCharacteristics(logId)
                    val caps  = chars.get(CameraCharacteristics.REQUEST_AVAILABLE_CAPABILITIES)
                    if (caps?.contains(CameraCharacteristics.REQUEST_AVAILABLE_CAPABILITIES_LOGICAL_MULTI_CAMERA) == true) {
                        chars.physicalCameraIds.forEach { physId ->
                            if (result.none { it.id == physId })
                                buildEntry(physId, logId)?.let { result += it }
                        }
                    }
                }
            }
        }
        allCameras = result
        startServer()
    }

    // ── HTTP server ───────────────────────────────────────────────────────────

    private fun startServer() {
        server = MjpegServer(
            port           = DEFAULT_PORT,
            getCamerasJson = ::buildCamerasJson,
            handleControl  = ::handleControlCommand,
        ).also { it.start() }
    }

    private fun buildCamerasJson(): String {
        val cur = currentCamera
        val cams = allCameras.joinToString(",") { e ->
            val log  = if (e.logicalId != null) "\"${e.logicalId}\"" else "null"
            val curr = (e.id == cur?.id).toString()
            """{"id":"${e.id}","logicalId":$log,"label":"${e.label}","current":$curr,""" +
            """"isoMin":${e.isoMin},"isoMax":${e.isoMax},""" +
            """"shutterMinNs":${e.shutterMinNs},"shutterMaxNs":${e.shutterMaxNs}}"""
        }
        val auto   = (currentIso == null).toString()
        val isoStr = currentIso?.toString() ?: "null"
        val shtStr = currentShutterNs?.toString() ?: "null"
        val wbStr  = currentWbKelvin?.toString() ?: "null"
        return """{"cameras":[$cams],"auto":$auto,"iso":$isoStr,""" +
               """"shutter_ns":$shtStr,"wb_kelvin":$wbStr,"ois":$currentOis}"""
    }

    private fun handleControlCommand(params: Map<String, String>): String {
        return try {
            when (params["action"]) {
                "camera" -> {
                    val id    = params["id"] ?: return err("no id")
                    val entry = allCameras.find { it.id == id } ?: return err("unknown id $id")
                    handler?.post { switchCameraTo(entry) }
                    ok()
                }
                "iso" -> {
                    val iso = params["value"]?.toIntOrNull() ?: return err("bad iso")
                    currentIso = iso
                    handler?.post { applyExposure() }
                    ok()
                }
                "shutter" -> {
                    val ns = params["value"]?.toLongOrNull() ?: return err("bad shutter")
                    currentShutterNs = ns
                    handler?.post { applyExposure() }
                    ok()
                }
                "auto" -> {
                    currentIso = null; currentShutterNs = null
                    handler?.post { applyExposure() }
                    ok()
                }
                "ois" -> {
                    currentOis = params["value"] == "1"
                    handler?.post { applyExposure() }
                    ok()
                }
                "wb_kelvin" -> {
                    val k = params["value"]?.toIntOrNull() ?: return err("bad kelvin")
                    currentWbKelvin = k.coerceIn(1000, 40000)
                    handler?.post { applyExposure() }
                    ok()
                }
                "wb_auto" -> {
                    currentWbKelvin = null
                    handler?.post { applyExposure() }
                    ok()
                }
                else -> err("unknown action '${params["action"]}'")
            }
        } catch (e: Exception) { err(e.message ?: "exception") }
    }

    private fun ok()             = """{"ok":true}"""
    private fun err(msg: String) = """{"ok":false,"error":"$msg"}"""

    // ── Camera open / session ─────────────────────────────────────────────────

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
                server?.sendFrame(bytes)
            } finally { image.close() }
        }, handler)

        val manager = getSystemService(CAMERA_SERVICE) as CameraManager
        try {
            @Suppress("MissingPermission")
            manager.openCamera(openCameraId, object : CameraDevice.StateCallback() {
                override fun onOpened(camera: CameraDevice) {
                    cameraDevice = camera
                    if (physicalCameraId != null && Build.VERSION.SDK_INT >= Build.VERSION_CODES.P)
                        createPhysicalSession(camera, physicalCameraId)
                    else
                        createLegacySession(camera)
                }
                override fun onDisconnected(camera: CameraDevice) { camera.close(); cameraDevice = null }
                override fun onError(camera: CameraDevice, error: Int) { camera.close(); cameraDevice = null; stopSelf() }
            }, handler)
        } catch (e: Exception) { stopSelf() }
    }

    private fun createPhysicalSession(camera: CameraDevice, physId: String) {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.P) { createLegacySession(camera); return }
        val outCfg = OutputConfiguration(imageReader!!.surface).also { it.setPhysicalCameraId(physId) }
        val exec   = Executor { cmd -> handler?.post(cmd) }
        camera.createCaptureSession(SessionConfiguration(
            SessionConfiguration.SESSION_REGULAR, listOf(outCfg), exec,
            object : CameraCaptureSession.StateCallback() {
                override fun onConfigured(s: CameraCaptureSession) { captureSession = s; startRepeating(camera, s) }
                override fun onConfigureFailed(s: CameraCaptureSession) { stopSelf() }
            }
        ))
    }

    @Suppress("DEPRECATION")
    private fun createLegacySession(camera: CameraDevice) {
        camera.createCaptureSession(listOf(imageReader!!.surface),
            object : CameraCaptureSession.StateCallback() {
                override fun onConfigured(s: CameraCaptureSession) { captureSession = s; startRepeating(camera, s) }
                override fun onConfigureFailed(s: CameraCaptureSession) { stopSelf() }
            }, handler)
    }

    private fun startRepeating(camera: CameraDevice, session: CameraCaptureSession) {
        try { session.setRepeatingRequest(buildRequest(camera), null, handler) }
        catch (e: CameraAccessException) { stopSelf() }
    }

    private fun buildRequest(camera: CameraDevice = cameraDevice!!): CaptureRequest {
        return camera.createCaptureRequest(CameraDevice.TEMPLATE_PREVIEW).apply {
            addTarget(imageReader!!.surface)

            // Exposure
            if (currentIso != null && currentShutterNs != null) {
                set(CaptureRequest.CONTROL_MODE,    CaptureRequest.CONTROL_MODE_OFF)
                set(CaptureRequest.CONTROL_AE_MODE, CaptureRequest.CONTROL_AE_MODE_OFF)
                set(CaptureRequest.SENSOR_SENSITIVITY,   currentIso!!)
                set(CaptureRequest.SENSOR_EXPOSURE_TIME, currentShutterNs!!)
            } else {
                set(CaptureRequest.CONTROL_MODE,    CaptureRequest.CONTROL_MODE_AUTO)
                set(CaptureRequest.CONTROL_AE_MODE, CaptureRequest.CONTROL_AE_MODE_ON)
                set(CaptureRequest.CONTROL_AF_MODE, CaptureRequest.CONTROL_AF_MODE_CONTINUOUS_VIDEO)
            }

            // White balance
            if (currentWbKelvin != null) {
                set(CaptureRequest.CONTROL_AWB_MODE,         CaptureRequest.CONTROL_AWB_MODE_OFF)
                set(CaptureRequest.COLOR_CORRECTION_MODE,    CaptureRequest.COLOR_CORRECTION_MODE_FAST)
                set(CaptureRequest.COLOR_CORRECTION_GAINS,   kelvinToRggb(currentWbKelvin!!))
            } else {
                set(CaptureRequest.CONTROL_AWB_MODE, CaptureRequest.CONTROL_AWB_MODE_AUTO)
            }

            // OIS
            if (currentOis) set(CaptureRequest.LENS_OPTICAL_STABILIZATION_MODE,
                CaptureRequest.LENS_OPTICAL_STABILIZATION_MODE_ON)

        }.build()
    }

    private fun applyExposure() {
        try {
            val s = captureSession ?: return
            val c = cameraDevice  ?: return
            s.setRepeatingRequest(buildRequest(c), null, handler)
        } catch (_: Exception) {}
    }

    private fun switchCameraTo(entry: CameraEntry) {
        currentCamera = entry
        try { captureSession?.close() } catch (_: Exception) {}
        try { cameraDevice?.close()   } catch (_: Exception) {}
        captureSession = null; cameraDevice = null

        val openId = entry.logicalId ?: entry.id
        val physId = if (entry.logicalId != null) entry.id else null
        val manager = getSystemService(CAMERA_SERVICE) as CameraManager
        try {
            @Suppress("MissingPermission")
            manager.openCamera(openId, object : CameraDevice.StateCallback() {
                override fun onOpened(camera: CameraDevice) {
                    cameraDevice = camera
                    if (physId != null && Build.VERSION.SDK_INT >= Build.VERSION_CODES.P)
                        createPhysicalSession(camera, physId)
                    else
                        createLegacySession(camera)
                }
                override fun onDisconnected(camera: CameraDevice) { camera.close() }
                override fun onError(camera: CameraDevice, error: Int) { camera.close() }
            }, handler)
        } catch (_: Exception) {}
    }

    fun stopStreaming() {
        isStreaming = false
        try { captureSession?.stopRepeating() } catch (_: Exception) {}
        try { captureSession?.close()         } catch (_: Exception) {}
        try { cameraDevice?.close()           } catch (_: Exception) {}
        try { imageReader?.close()            } catch (_: Exception) {}
        server?.stop()
        handlerThread?.quitSafely()
        wakeLock?.let { if (it.isHeld) it.release() }
        captureSession = null; cameraDevice = null; imageReader = null; server = null
        stopForeground(STOP_FOREGROUND_REMOVE); stopSelf()
    }

    // ── Notification ──────────────────────────────────────────────────────────

    private fun createNotificationChannel() {
        val ch = NotificationChannel(CHANNEL_ID, "Camera Stream", NotificationManager.IMPORTANCE_LOW)
            .apply { description = "PhoneCam MJPEG stream" }
        (getSystemService(NOTIFICATION_SERVICE) as NotificationManager).createNotificationChannel(ch)
    }

    private fun startForegroundCompat() {
        val pi = PendingIntent.getActivity(this, 0,
            Intent(this, MainActivity::class.java), PendingIntent.FLAG_IMMUTABLE)
        val n = NotificationCompat.Builder(this, CHANNEL_ID)
            .setContentTitle("PhoneCam").setContentText("Streaming :$DEFAULT_PORT")
            .setSmallIcon(android.R.drawable.ic_menu_camera)
            .setContentIntent(pi).setOngoing(true).build()
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q)
            startForeground(NOTIF_ID, n, ServiceInfo.FOREGROUND_SERVICE_TYPE_CAMERA)
        else
            startForeground(NOTIF_ID, n)
    }

    private fun acquireWakeLock() {
        val pm = getSystemService(POWER_SERVICE) as PowerManager
        wakeLock = pm.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "phonecam::stream")
        wakeLock?.acquire(12 * 60 * 60 * 1000L)
    }
}
