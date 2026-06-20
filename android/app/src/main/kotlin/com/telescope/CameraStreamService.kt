package com.telescope

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
import android.hardware.camera2.params.ColorSpaceTransform
import android.hardware.camera2.params.OutputConfiguration
import android.hardware.camera2.params.RggbChannelVector
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
import android.util.Rational
import androidx.core.app.NotificationCompat
import androidx.core.content.ContextCompat
import java.util.concurrent.Executor
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
    val supportsManualFocus: Boolean = false,
    val minFocusDistance: Float = 0f,
    val hwLevel: String = "UNKNOWN",
    val aeCompMin: Int = -8,
    val aeCompMax: Int = 8,
    val aeCompStep: Float = 0.167f,
    val supportsFlash: Boolean = false,
)

class CameraStreamService : Service() {

    companion object {
        const val EXTRA_CAMERA_ID  = "camera_id"
        const val EXTRA_LOGICAL_ID = "logical_id"
        const val EXTRA_WIDTH      = "width"
        const val EXTRA_HEIGHT     = "height"
        const val EXTRA_OIS        = "ois"
        const val EXTRA_LOCAL_ONLY = "local_only"
        const val CHANNEL_ID       = "telescope_stream"
        const val NOTIF_ID         = 1
        const val DEFAULT_PORT     = 8080

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
    private var bindAddr     = "0.0.0.0"

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
        currentOis    = intent?.getBooleanExtra(EXTRA_OIS,        true)  ?: true
        val localOnly = intent?.getBooleanExtra(EXTRA_LOCAL_ONLY, false) ?: false
        bindAddr      = if (localOnly) "127.0.0.1" else "0.0.0.0"

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
            val supportsManualWB = caps?.contains(
                CameraCharacteristics.REQUEST_AVAILABLE_CAPABILITIES_MANUAL_POST_PROCESSING) == true
            // Manual focus: needs MANUAL_SENSOR and a non-zero minimum focus distance
            val minFocusDist = chars.get(CameraCharacteristics.LENS_INFO_MINIMUM_FOCUS_DISTANCE) ?: 0f
            val supportsManualFocus = supportsManualSensor && minFocusDist > 0f

            val aeCompRange = chars.get(CameraCharacteristics.CONTROL_AE_COMPENSATION_RANGE)
            val aeCompMin   = aeCompRange?.lower ?: -8
            val aeCompMax   = aeCompRange?.upper ?: 8
            val aeStepR     = chars.get(CameraCharacteristics.CONTROL_AE_COMPENSATION_STEP)
            val aeCompStep  = if (aeStepR != null && aeStepR.denominator != 0)
                                  aeStepR.numerator.toFloat() / aeStepR.denominator.toFloat()
                              else 0.167f
            val supportsFlash = chars.get(CameraCharacteristics.FLASH_INFO_AVAILABLE) == true

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
                        isoMin, isoMax, shtMinNs, shtMaxNs,
                        supportsManualSensor, supportsManualWB, supportsManualFocus, minFocusDist, hwLevel,
                        aeCompMin, aeCompMax, aeCompStep, supportsFlash)
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
            bindAddr       = bindAddr,
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
            """"hasOis":${e.hasOis},"isoMin":${e.isoMin},"isoMax":${e.isoMax},""" +
            """"shutterMinNs":${e.shutterMinNs},"shutterMaxNs":${e.shutterMaxNs},""" +
            """"supportsManualSensor":${e.supportsManualSensor},"supportsManualWB":${e.supportsManualWB},""" +
            """"supportsManualFocus":${e.supportsManualFocus},"minFocusDistance":${e.minFocusDistance},""" +
            """"aeCompMin":${e.aeCompMin},"aeCompMax":${e.aeCompMax},"aeCompStep":${e.aeCompStep},""" +
            """"supportsFlash":${e.supportsFlash},"hwLevel":"${e.hwLevel}"}"""
        }
        val auto   = (currentIso == null).toString()
        val isoStr = currentIso?.toString() ?: "null"
        val shtStr = currentShutterNs?.toString() ?: "null"
        val wbManual = (currentWbGains != null).toString()
        val mg = lastMeasuredGains
        val wbGainsStr = if (mg != null)
            """"wb_r":${mg.red},"wb_ge":${mg.greenEven},"wb_go":${mg.greenOdd},"wb_b":${mg.blue}"""
        else """"wb_r":null,"wb_ge":null,"wb_go":null,"wb_b":null"""
        val (battLevel, battCharging, battTempC) = getBatteryInfo()
        return """{"cameras":[$cams],"auto":$auto,"iso":$isoStr,""" +
               """"shutter_ns":$shtStr,"wb_manual":$wbManual,$wbGainsStr,"ois":$currentOis,""" +
               """"focus_mode":"$currentFocusMode","focus_distance":$currentFocusDistance,""" +
               """"nr_mode":$currentNrMode,"edge_mode":$currentEdgeMode,""" +
               """"ae_comp":$currentAeComp,"black_level_lock":$currentBlackLevelLock,"torch":$currentTorch,""" +
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
                "wb_gains" -> {
                    val r  = params["r"]?.toFloatOrNull()  ?: return err("bad r")
                    val ge = params["ge"]?.toFloatOrNull() ?: return err("bad ge")
                    val go = params["go"]?.toFloatOrNull() ?: return err("bad go")
                    val b  = params["b"]?.toFloatOrNull()  ?: return err("bad b")
                    currentWbGains = RggbChannelVector(r, ge, go, b)
                    handler?.post { applyExposure() }
                    ok()
                }
                "wb_auto" -> {
                    currentWbGains = null
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
                "focus_mode" -> {
                    val mode = params["value"] ?: return err("no value")
                    if (mode != "continuous" && mode != "manual") return err("bad mode")
                    currentFocusMode = mode
                    handler?.post { applyExposure() }
                    ok()
                }
                "focus_distance" -> {
                    val d = params["value"]?.toFloatOrNull() ?: return err("bad distance")
                    currentFocusDistance = d.coerceAtLeast(0f)
                    handler?.post { applyExposure() }
                    ok()
                }
                "nr_mode" -> {
                    val m = params["value"]?.toIntOrNull() ?: return err("bad value")
                    currentNrMode = m.coerceIn(0, 4)
                    handler?.post { applyExposure() }
                    ok()
                }
                "edge_mode" -> {
                    val m = params["value"]?.toIntOrNull() ?: return err("bad value")
                    currentEdgeMode = m.coerceIn(0, 3)
                    handler?.post { applyExposure() }
                    ok()
                }
                "ae_comp" -> {
                    val v = params["value"]?.toIntOrNull() ?: return err("bad value")
                    currentAeComp = v
                    handler?.post { applyExposure() }
                    ok()
                }
                "black_level_lock" -> {
                    currentBlackLevelLock = params["value"] == "1"
                    handler?.post { applyExposure() }
                    ok()
                }
                "torch" -> {
                    currentTorch = params["value"] == "1"
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
        try { session.setRepeatingRequest(buildRequest(camera), ccmCaptureCallback, handler) }
        catch (e: CameraAccessException) { stopSelf() }
    }

    private fun buildRequest(camera: CameraDevice = cameraDevice!!): CaptureRequest {
        return camera.createCaptureRequest(CameraDevice.TEMPLATE_PREVIEW).apply {
            addTarget(imageReader!!.surface)

            // Exposure and FPS
            // Use CONTROL_MODE_AUTO even in manual AE so that AF keeps running independently.
            set(CaptureRequest.CONTROL_MODE, CaptureRequest.CONTROL_MODE_AUTO)
            if (currentIso != null && currentShutterNs != null && currentCamera?.supportsManualSensor == true) {
                set(CaptureRequest.CONTROL_AE_MODE, CaptureRequest.CONTROL_AE_MODE_OFF)
                set(CaptureRequest.SENSOR_SENSITIVITY,   currentIso!!)
                set(CaptureRequest.SENSOR_EXPOSURE_TIME, currentShutterNs!!)
                // Enforce FPS via frame duration
                val targetFrameNs = 1_000_000_000L / currentPhoneFps
                set(CaptureRequest.SENSOR_FRAME_DURATION,
                    targetFrameNs.coerceAtLeast(currentShutterNs!!))
            } else {
                set(CaptureRequest.CONTROL_AE_MODE, CaptureRequest.CONTROL_AE_MODE_ON)
                // In auto AE, request FPS range (allow half target in low light)
                val fpsMin = (currentPhoneFps / 2).coerceAtLeast(5)
                set(CaptureRequest.CONTROL_AE_TARGET_FPS_RANGE,
                    Range(fpsMin, currentPhoneFps))
            }

            // Focus
            if (currentFocusMode == "manual" && currentCamera?.supportsManualFocus == true) {
                set(CaptureRequest.CONTROL_AF_MODE, CaptureRequest.CONTROL_AF_MODE_OFF)
                set(CaptureRequest.LENS_FOCUS_DISTANCE, currentFocusDistance)
            } else {
                set(CaptureRequest.CONTROL_AF_MODE, CaptureRequest.CONTROL_AF_MODE_CONTINUOUS_VIDEO)
            }

            // JPEG quality
            set(CaptureRequest.JPEG_QUALITY, currentJpegQuality.toByte())

            // White balance — desktop sends pre-computed RGGB gains
            val gains = currentWbGains
            if (gains != null && currentCamera?.supportsManualWB == true) {
                set(CaptureRequest.CONTROL_AWB_MODE, CaptureRequest.CONTROL_AWB_MODE_OFF)
                set(CaptureRequest.COLOR_CORRECTION_MODE,
                    CameraMetadata.COLOR_CORRECTION_MODE_TRANSFORM_MATRIX)
                set(CaptureRequest.COLOR_CORRECTION_GAINS, gains)
                lastCCM?.let { set(CaptureRequest.COLOR_CORRECTION_TRANSFORM, it) }
            } else {
                set(CaptureRequest.CONTROL_AWB_MODE, CaptureRequest.CONTROL_AWB_MODE_AUTO)
                set(CaptureRequest.COLOR_CORRECTION_MODE, CameraMetadata.COLOR_CORRECTION_MODE_FAST)
            }

            // OIS
            if (currentOis) set(CaptureRequest.LENS_OPTICAL_STABILIZATION_MODE,
                CaptureRequest.LENS_OPTICAL_STABILIZATION_MODE_ON)

            // Noise reduction and edge enhancement
            set(CaptureRequest.NOISE_REDUCTION_MODE, currentNrMode)
            set(CaptureRequest.EDGE_MODE, currentEdgeMode)

            // AE exposure compensation (only meaningful in auto AE)
            if (currentIso == null) set(CaptureRequest.CONTROL_AE_EXPOSURE_COMPENSATION, currentAeComp)

            // Black level lock
            set(CaptureRequest.BLACK_LEVEL_LOCK, currentBlackLevelLock)

            // Torch
            set(CaptureRequest.FLASH_MODE,
                if (currentTorch) CaptureRequest.FLASH_MODE_TORCH else CaptureRequest.FLASH_MODE_OFF)

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
        try {
            val s = captureSession ?: return
            val c = cameraDevice  ?: return
            s.setRepeatingRequest(buildRequest(c), ccmCaptureCallback, handler)
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
            .apply { description = "Telescope MJPEG stream" }
        (getSystemService(NOTIFICATION_SERVICE) as NotificationManager).createNotificationChannel(ch)
    }

    private fun startForegroundCompat() {
        val pi = PendingIntent.getActivity(this, 0,
            Intent(this, MainActivity::class.java), PendingIntent.FLAG_IMMUTABLE)
        val n = NotificationCompat.Builder(this, CHANNEL_ID)
            .setContentTitle("Telescope").setContentText("Streaming :$DEFAULT_PORT")
            .setSmallIcon(R.drawable.ic_notification)
            .setColor(ContextCompat.getColor(this, R.color.colorPrimary))
            .setColorized(false)
            .setContentIntent(pi).setOngoing(true).build()
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q)
            startForeground(NOTIF_ID, n, ServiceInfo.FOREGROUND_SERVICE_TYPE_CAMERA)
        else
            startForeground(NOTIF_ID, n)
    }

    private fun acquireWakeLock() {
        val pm = getSystemService(POWER_SERVICE) as PowerManager
        wakeLock = pm.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "telescope::stream")
        wakeLock?.acquire(12 * 60 * 60 * 1000L)
    }
}
