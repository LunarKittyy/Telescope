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
import android.view.Surface
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
    val aeFpsRanges: List<Range<Int>> = emptyList(),
    val afModes: Set<Int> = emptySet(),
    val nrModes: Set<Int> = emptySet(),
    val edgeModes: Set<Int> = emptySet(),
)

/**
 * Pure Camera2 request-parameter selection logic, kept free of any CameraDevice/Service
 * state so it can be unit tested on a plain JVM without camera hardware.
 */
object CameraRequestSelection {
    /**
     * Picks the advertised AE target FPS range closest to [target].
     * Preference order: (1) a range that contains target, highest lower-bound among those
     * (reduces low-light FPS drop); (2) otherwise the range whose upper bound is nearest
     * target. Returns null (omit the request key) if [available] is empty.
     */
    fun pickAeFpsRange(available: List<Range<Int>>, target: Int): Range<Int>? {
        if (available.isEmpty()) return null
        val containing = available.filter { target in it.lower..it.upper }
        if (containing.isNotEmpty()) return containing.maxByOrNull { it.lower }
        return available.minByOrNull { kotlin.math.abs(it.upper - target) }
    }

    /**
     * Chooses an AF mode from the camera's advertised modes. When [wantContinuousVideo] is
     * true (i.e. not doing manual focus), prefers CONTINUOUS_VIDEO, then falls back through
     * CONTINUOUS_PICTURE, AUTO, and finally OFF (always legal per the Camera2 contract) if
     * none of the preferred modes are advertised.
     */
    fun pickAfMode(available: Set<Int>, wantContinuousVideo: Boolean): Int {
        if (wantContinuousVideo && CaptureRequest.CONTROL_AF_MODE_CONTINUOUS_VIDEO in available)
            return CaptureRequest.CONTROL_AF_MODE_CONTINUOUS_VIDEO
        return when {
            CaptureRequest.CONTROL_AF_MODE_CONTINUOUS_PICTURE in available ->
                CaptureRequest.CONTROL_AF_MODE_CONTINUOUS_PICTURE
            CaptureRequest.CONTROL_AF_MODE_AUTO in available -> CaptureRequest.CONTROL_AF_MODE_AUTO
            else -> CaptureRequest.CONTROL_AF_MODE_OFF
        }
    }

    /** Returns [requested] if advertised, else a safe fallback, else null (omit the key). */
    fun pickNrMode(available: Set<Int>, requested: Int): Int? = pickMode(
        available, requested,
        listOf(CaptureRequest.NOISE_REDUCTION_MODE_FAST, CaptureRequest.NOISE_REDUCTION_MODE_OFF)
    )

    /** Returns [requested] if advertised, else a safe fallback, else null (omit the key). */
    fun pickEdgeMode(available: Set<Int>, requested: Int): Int? = pickMode(
        available, requested,
        listOf(CaptureRequest.EDGE_MODE_FAST, CaptureRequest.EDGE_MODE_OFF)
    )

    private fun pickMode(available: Set<Int>, requested: Int, fallbacks: List<Int>): Int? {
        if (available.isEmpty()) return null
        if (requested in available) return requested
        return fallbacks.firstOrNull { it in available }
    }

    fun clamp(value: Int, min: Int, max: Int): Int =
        if (min > max) value else value.coerceIn(min, max)

    fun clamp(value: Long, min: Long, max: Long): Long =
        if (min > max) value else value.coerceIn(min, max)

    fun clamp(value: Float, min: Float, max: Float): Float =
        if (min > max) value else value.coerceIn(min, max)
}

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
        private const val TAG      = "CameraStreamService"

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
    @Volatile private var previewSurface: Surface? = null

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

    // ── Live preview (for PreviewActivity) ───────────────────────────────────

    fun getCameras(): List<CameraEntry> = allCameras
    fun getCurrentCameraId(): String? = currentCamera?.id
    fun getStreamSize(): android.util.Size = android.util.Size(streamWidth, streamHeight)

    fun switchCamera(id: String) {
        val entry = allCameras.find { it.id == id } ?: return
        handler?.post { switchCameraTo(entry) }
    }

    /** Adds an extra output surface to the running capture session so a live preview
     *  can be shown without interrupting the MJPEG stream. */
    fun attachPreviewSurface(surface: Surface) {
        handler?.post { previewSurface = surface; reconfigureSession() }
    }

    /** [onDetached] runs after the surface has been dropped from the capture session and
     *  the session rebuilt without it — the caller can safely release the Surface then. */
    fun detachPreviewSurface(onDetached: (() -> Unit)? = null) {
        handler?.post {
            previewSurface = null
            reconfigureSession()
            onDetached?.invoke()
        }
    }

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

        // startForeground() must be called unconditionally and early, before any
        // return below: this service is launched via startForegroundService(), and
        // Android kills the app if that promotion doesn't happen soon after, no
        // matter what else goes wrong in the rest of this method.
        startForegroundCompat()

        try {
            enumerateAllCameras()
        } catch (e: Exception) {
            // e.g. EADDRINUSE if a just-stopped instance's MJPEG server port hasn't
            // been released yet — this used to be uncaught and crashed the app.
            android.util.Log.e(TAG, "Failed to start MJPEG server", e)
            stopForeground(STOP_FOREGROUND_REMOVE)
            stopSelf()
            return START_NOT_STICKY
        }
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

            val aeFpsRanges = chars.get(CameraCharacteristics.CONTROL_AE_AVAILABLE_TARGET_FPS_RANGES)
                ?.toList() ?: emptyList()
            val afModes   = chars.get(CameraCharacteristics.CONTROL_AF_AVAILABLE_MODES)?.toSet() ?: emptySet()
            val nrModes   = chars.get(CameraCharacteristics.NOISE_REDUCTION_AVAILABLE_NOISE_REDUCTION_MODES)
                ?.toSet() ?: emptySet()
            val edgeModes = chars.get(CameraCharacteristics.EDGE_AVAILABLE_EDGE_MODES)?.toSet() ?: emptySet()

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
                        aeCompMin, aeCompMax, aeCompStep, supportsFlash,
                        aeFpsRanges, afModes, nrModes, edgeModes)
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

    // Bumped on every camera-open attempt (initial open or switchCameraTo). Async
    // onOpened/onConfigured callbacks compare their captured generation against the
    // current value and discard themselves if a newer open has since superseded them —
    // otherwise a stale callback from an old open could clobber the camera that's
    // actually meant to be active (see PR3 review notes on async camera switching).
    @Volatile private var cameraGeneration = 0

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

        val myGeneration = ++cameraGeneration
        val manager = getSystemService(CAMERA_SERVICE) as CameraManager
        try {
            @Suppress("MissingPermission")
            manager.openCamera(openCameraId, object : CameraDevice.StateCallback() {
                override fun onOpened(camera: CameraDevice) {
                    if (myGeneration != cameraGeneration) { camera.close(); return }
                    cameraDevice = camera
                    if (physicalCameraId != null && Build.VERSION.SDK_INT >= Build.VERSION_CODES.P)
                        createPhysicalSession(camera, physicalCameraId, myGeneration)
                    else
                        createLegacySession(camera, myGeneration)
                }
                override fun onDisconnected(camera: CameraDevice) {
                    camera.close()
                    if (myGeneration == cameraGeneration) cameraDevice = null
                }
                override fun onError(camera: CameraDevice, error: Int) {
                    camera.close()
                    if (myGeneration == cameraGeneration) { cameraDevice = null; stopSelf() }
                }
            }, handler)
        } catch (e: Exception) { stopSelf() }
    }

    private fun currentTargetSurfaces(): List<Surface> = listOfNotNull(imageReader?.surface, previewSurface)

    private fun createPhysicalSession(camera: CameraDevice, physId: String, generation: Int = cameraGeneration) {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.P) { createLegacySession(camera, generation); return }
        // A stop/teardown racing in between the generation check in the caller and
        // this call can null out imageReader (and previewSurface) concurrently -
        // re-check staleness and bail rather than asking Camera2 to configure a
        // session with zero surfaces, which throws.
        if (generation != cameraGeneration) return
        val outCfgs = currentTargetSurfaces().map { surface ->
            OutputConfiguration(surface).also { it.setPhysicalCameraId(physId) }
        }
        if (outCfgs.isEmpty()) return
        val exec   = Executor { cmd -> handler?.post(cmd) }
        try {
            camera.createCaptureSession(SessionConfiguration(
                SessionConfiguration.SESSION_REGULAR, outCfgs, exec,
                object : CameraCaptureSession.StateCallback() {
                    override fun onConfigured(s: CameraCaptureSession) {
                        if (generation != cameraGeneration) { s.close(); return }
                        captureSession = s; startRepeating(camera, s)
                    }
                    override fun onConfigureFailed(s: CameraCaptureSession) {
                        if (generation == cameraGeneration) stopSelf()
                    }
                }
            ))
        } catch (_: Exception) {
            if (generation == cameraGeneration) stopSelf()
        }
    }

    @Suppress("DEPRECATION")
    private fun createLegacySession(camera: CameraDevice, generation: Int = cameraGeneration) {
        if (generation != cameraGeneration) return
        val targets = currentTargetSurfaces()
        if (targets.isEmpty()) return
        try {
            camera.createCaptureSession(targets,
                object : CameraCaptureSession.StateCallback() {
                    override fun onConfigured(s: CameraCaptureSession) {
                        if (generation != cameraGeneration) { s.close(); return }
                        captureSession = s; startRepeating(camera, s)
                    }
                    override fun onConfigureFailed(s: CameraCaptureSession) {
                        if (generation == cameraGeneration) stopSelf()
                    }
                }, handler)
        } catch (_: Exception) {
            if (generation == cameraGeneration) stopSelf()
        }
    }

    /** Tears down and rebuilds the capture session on the already-open camera device,
     *  e.g. when a preview surface is attached/detached. Does not reopen the device. */
    private fun reconfigureSession() {
        val camera = cameraDevice ?: return
        try { captureSession?.stopRepeating() } catch (_: Exception) {}
        try { captureSession?.close() } catch (_: Exception) {}
        captureSession = null

        val cam    = currentCamera
        val physId = if (cam?.logicalId != null) cam.id else null
        if (physId != null && Build.VERSION.SDK_INT >= Build.VERSION_CODES.P)
            createPhysicalSession(camera, physId)
        else
            createLegacySession(camera)
    }

    private fun startRepeating(camera: CameraDevice, session: CameraCaptureSession) {
        try { session.setRepeatingRequest(buildRequest(camera), ccmCaptureCallback, handler) }
        catch (e: CameraAccessException) { stopSelf() }
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
                // Pick the closest AE FPS range Camera2 actually advertises for this
                // camera instead of inventing one — an unsupported range can make the
                // capture request fail outright on some devices.
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

            // White balance — desktop sends pre-computed RGGB gains
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

            // OIS — only ever requested ON if this camera actually advertises it.
            set(CaptureRequest.LENS_OPTICAL_STABILIZATION_MODE,
                if (currentOis && cam?.hasOis == true) CaptureRequest.LENS_OPTICAL_STABILIZATION_MODE_ON
                else CaptureRequest.LENS_OPTICAL_STABILIZATION_MODE_OFF)

            // Noise reduction and edge enhancement — set only when the camera advertises
            // a usable mode; otherwise omit the key rather than force an unsupported value.
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

            // Torch — only if this camera actually has a flash unit.
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
        try {
            val s = captureSession ?: return
            val c = cameraDevice  ?: return
            s.setRepeatingRequest(buildRequest(c), ccmCaptureCallback, handler)
        } catch (_: Exception) {}
    }

    private fun switchCameraTo(entry: CameraEntry) {
        // Drop manual/flash/focus state the new lens can't honor instead of silently
        // carrying it across — buildRequest() would otherwise mask this at request
        // time while buildCamerasJson() kept reporting the stale values.
        //
        // currentOis is deliberately NOT reset here: buildRequest() already only
        // requests OIS-on when cam.hasOis is true, so leaving the user's desired
        // toggle alone lets OIS resume automatically when switching back to an
        // OIS-capable lens. Resetting it (as this used to do) silently turned OIS
        // off with no way to re-enable it except unchecking/rechecking the desktop
        // checkbox, since nothing here ever set it back to true on returning to an
        // OIS lens.
        if (!entry.supportsFlash) currentTorch = false
        if (!entry.supportsManualSensor) { currentIso = null; currentShutterNs = null }
        if (!entry.supportsManualFocus && currentFocusMode == "manual") currentFocusMode = "continuous"
        currentCamera = entry

        val myGeneration = ++cameraGeneration
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
                    if (myGeneration != cameraGeneration) { camera.close(); return }
                    cameraDevice = camera
                    if (physId != null && Build.VERSION.SDK_INT >= Build.VERSION_CODES.P)
                        createPhysicalSession(camera, physId, myGeneration)
                    else
                        createLegacySession(camera, myGeneration)
                }
                override fun onDisconnected(camera: CameraDevice) {
                    camera.close()
                    if (myGeneration == cameraGeneration) cameraDevice = null
                }
                override fun onError(camera: CameraDevice, error: Int) {
                    camera.close()
                    if (myGeneration == cameraGeneration) cameraDevice = null
                }
            }, handler)
        } catch (_: Exception) {}
    }

    fun stopStreaming() {
        isStreaming = false
        // Invalidates any open/session-configure callback already in flight on the
        // camera handler thread (e.g. from a start that was immediately followed by
        // a stop, before onOpened even fired) — without this, such a callback could
        // resurrect cameraDevice/captureSession using imageReader/previewSurface
        // that this call is about to close and null out, and then hand Camera2 an
        // empty or already-torn-down surface set, which throws and crashes the app.
        cameraGeneration++
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
