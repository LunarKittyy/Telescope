package com.phonecam

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.ServiceInfo
import android.graphics.ImageFormat
import android.hardware.camera2.*
import android.hardware.camera2.params.OutputConfiguration
import android.hardware.camera2.params.SessionConfiguration
import android.media.ImageReader
import android.os.BatteryManager
import android.os.Binder
import android.os.Build
import android.os.Handler
import android.os.HandlerThread
import android.os.IBinder
import android.os.PowerManager
import android.util.Range
import androidx.core.app.NotificationCompat
import java.util.concurrent.Executor
import kotlin.math.ln
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
    val supportsManualSensor: Boolean = false,
    val supportsManualWB: Boolean = false,
    val hwLevel: String = "UNKNOWN",
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

        // Map a Kelvin value to the nearest Camera2 AWB mode preset.
        // The presets use the device's own factory calibration, which is far more
        // accurate than computing raw gains manually without sensor spectral data.
        fun kelvinToAwbMode(kelvin: Int): Int = when {
            kelvin < 2500 -> CaptureRequest.CONTROL_AWB_MODE_INCANDESCENT    // ~2700 K
            kelvin < 3500 -> CaptureRequest.CONTROL_AWB_MODE_WARM_FLUORESCENT // ~3000 K
            kelvin < 4500 -> CaptureRequest.CONTROL_AWB_MODE_FLUORESCENT      // ~4000 K
            kelvin < 6000 -> CaptureRequest.CONTROL_AWB_MODE_DAYLIGHT         // ~5500 K / D65
            kelvin < 7000 -> CaptureRequest.CONTROL_AWB_MODE_CLOUDY_DAYLIGHT  // ~6500 K
            else          -> CaptureRequest.CONTROL_AWB_MODE_SHADE             // ~7000-8000 K
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

    // Exposure state - null ISO/shutter = auto AE
    @Volatile private var currentIso:       Int?  = null
    @Volatile private var currentShutterNs: Long? = null
    @Volatile private var currentOis:       Boolean = true
    // WB state - null Kelvin = auto AWB
    @Volatile private var currentWbKelvin:  Int?  = null
    // Stream quality
    @Volatile private var currentJpegQuality: Int = 85
    @Volatile private var currentPhoneFps:    Int = 30

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

            val shtRange = chars.get(CameraCharacteristics.SENSOR_INFO_EXPOSURE_TIME_RANGE)
            val shtMinNs = shtRange?.lower ?: 100_000L
            val shtMaxNs = shtRange?.upper ?: 1_000_000_000L

            val caps = chars.get(CameraCharacteristics.REQUEST_AVAILABLE_CAPABILITIES)
            val supportsManualSensor = caps?.contains(
                CameraCharacteristics.REQUEST_AVAILABLE_CAPABILITIES_MANUAL_SENSOR) == true
            // For WB we use AWB presets (not raw COLOR_CORRECTION_GAINS), so check
            // whether at least DAYLIGHT mode is supported rather than MANUAL_POST_PROCESSING.
            val awbModes = chars.get(CameraCharacteristics.CONTROL_AWB_AVAILABLE_MODES)
            val supportsManualWB = awbModes?.contains(CaptureRequest.CONTROL_AWB_MODE_DAYLIGHT) == true

            val hwLevel = when (chars.get(CameraCharacteristics.INFO_SUPPORTED_HARDWARE_LEVEL)) {
                CameraCharacteristics.INFO_SUPPORTED_HARDWARE_LEVEL_LEGACY   -> "LEGACY"
                CameraCharacteristics.INFO_SUPPORTED_HARDWARE_LEVEL_LIMITED  -> "LIMITED"
                CameraCharacteristics.INFO_SUPPORTED_HARDWARE_LEVEL_FULL     -> "FULL"
                CameraCharacteristics.INFO_SUPPORTED_HARDWARE_LEVEL_3        -> "LEVEL_3"
                CameraCharacteristics.INFO_SUPPORTED_HARDWARE_LEVEL_EXTERNAL -> "EXTERNAL"
                else -> "UNKNOWN"
            }

            val fStr = if (focalEq > 0) "~${focalEq}mm" else "?"
            val oStr = if (hasOis) " OIS" else ""
            val pStr = if (logicalParent != null) " [phys]" else ""
            CameraEntry(id, logicalParent, "$facing $fStr$oStr$pStr", hasOis,
                        isoMin, isoMax, shtMinNs, shtMaxNs, supportsManualSensor, supportsManualWB, hwLevel)
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

    private fun getBatteryInfo(): Triple<Int, Boolean, Double> {
        val bm     = getSystemService(BATTERY_SERVICE) as BatteryManager
        val level  = bm.getIntProperty(BatteryManager.BATTERY_PROPERTY_CAPACITY).coerceIn(0, 100)
        val intent = registerReceiver(null, IntentFilter(Intent.ACTION_BATTERY_CHANGED))
        val status = intent?.getIntExtra(BatteryManager.EXTRA_STATUS, -1) ?: -1
        val charging = status == BatteryManager.BATTERY_STATUS_CHARGING
                    || status == BatteryManager.BATTERY_STATUS_FULL
        val tempC  = (intent?.getIntExtra(BatteryManager.EXTRA_TEMPERATURE, 0) ?: 0) / 10.0
        return Triple(level, charging, tempC)
    }

    private fun buildCamerasJson(): String {
        val cur = currentCamera
        val cams = allCameras.joinToString(",") { e ->
            val log  = if (e.logicalId != null) "\"${e.logicalId}\"" else "null"
            val curr = (e.id == cur?.id).toString()
            """{"id":"${e.id}","logicalId":$log,"label":"${e.label}","current":$curr,""" +
            """"isoMin":${e.isoMin},"isoMax":${e.isoMax},""" +
            """"shutterMinNs":${e.shutterMinNs},"shutterMaxNs":${e.shutterMaxNs},""" +
            """"supportsManualSensor":${e.supportsManualSensor},"supportsManualWB":${e.supportsManualWB},""" +
            """"hwLevel":"${e.hwLevel}"}"""
        }
        val auto   = (currentIso == null).toString()
        val isoStr = currentIso?.toString() ?: "null"
        val shtStr = currentShutterNs?.toString() ?: "null"
        val wbStr  = currentWbKelvin?.toString() ?: "null"
        val (battLevel, battCharging, battTempC) = getBatteryInfo()
        return """{"cameras":[$cams],"auto":$auto,"iso":$isoStr,""" +
               """"shutter_ns":$shtStr,"wb_kelvin":$wbStr,"ois":$currentOis,""" +
               """"jpeg_quality":$currentJpegQuality,"phone_fps":$currentPhoneFps,""" +
               """"battery":$battLevel,"charging":$battCharging,"battery_temp_c":$battTempC}"""
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
                "jpeg_quality" -> {
                    val q = params["value"]?.toIntOrNull() ?: return err("bad value")
                    currentJpegQuality = q.coerceIn(1, 100)
                    handler?.post { applyExposure() }
                    ok()
                }
                "fps_target" -> {
                    val fps = params["value"]?.toIntOrNull() ?: return err("bad value")
                    currentPhoneFps = fps.coerceIn(1, 120)
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

            // Exposure and FPS
            if (currentIso != null && currentShutterNs != null && currentCamera?.supportsManualSensor == true) {
                set(CaptureRequest.CONTROL_MODE,    CaptureRequest.CONTROL_MODE_OFF)
                set(CaptureRequest.CONTROL_AE_MODE, CaptureRequest.CONTROL_AE_MODE_OFF)
                set(CaptureRequest.SENSOR_SENSITIVITY,   currentIso!!)
                set(CaptureRequest.SENSOR_EXPOSURE_TIME, currentShutterNs!!)
                // In manual mode, enforce FPS via frame duration
                val targetFrameNs = 1_000_000_000L / currentPhoneFps
                set(CaptureRequest.SENSOR_FRAME_DURATION,
                    targetFrameNs.coerceAtLeast(currentShutterNs!!))
            } else {
                set(CaptureRequest.CONTROL_MODE,    CaptureRequest.CONTROL_MODE_AUTO)
                set(CaptureRequest.CONTROL_AE_MODE, CaptureRequest.CONTROL_AE_MODE_ON)
                set(CaptureRequest.CONTROL_AF_MODE, CaptureRequest.CONTROL_AF_MODE_CONTINUOUS_VIDEO)
                // In auto mode, request FPS range (allow half target in low light)
                val fpsMin = (currentPhoneFps / 2).coerceAtLeast(5)
                set(CaptureRequest.CONTROL_AE_TARGET_FPS_RANGE,
                    Range(fpsMin, currentPhoneFps))
            }

            // JPEG quality
            set(CaptureRequest.JPEG_QUALITY, currentJpegQuality.toByte())

            // White balance — use AWB mode presets (device-calibrated, not raw gains)
            if (currentWbKelvin != null && currentCamera?.supportsManualWB == true) {
                set(CaptureRequest.CONTROL_AWB_MODE, kelvinToAwbMode(currentWbKelvin!!))
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
