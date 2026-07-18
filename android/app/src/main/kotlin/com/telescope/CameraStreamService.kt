package com.telescope

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.ServiceInfo
import android.hardware.camera2.CameraCharacteristics
import android.hardware.camera2.CameraManager
import android.hardware.camera2.CaptureRequest
import android.hardware.camera2.params.RggbChannelVector
import android.os.BatteryManager
import android.os.Binder
import android.os.Build
import android.os.IBinder
import android.os.PowerManager
import android.util.Range
import androidx.core.app.NotificationCompat
import androidx.core.content.ContextCompat
import android.view.Surface
import kotlin.math.sqrt
import kotlinx.serialization.json.Json

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

    private var controller: CameraSessionController? = null
    private var server: MjpegServer? = null
    private var wakeLock: PowerManager.WakeLock? = null

    // Stream config
    private var streamWidth  = 1920
    private var streamHeight = 1080
    private var bindAddr     = "0.0.0.0"

    // Camera catalogue
    private var allCameras: List<CameraEntry> = emptyList()

    private val stateMachine = StreamStateMachine()
    val state: StreamState get() = stateMachine.state
    val isStreaming: Boolean get() = stateMachine.isStreaming
    val port: Int get() = DEFAULT_PORT

    /** Records a state transition and logs it with structured context (camera
     *  id, generation, operation, sanitized exception details - class name and
     *  message only, never a raw stack trace or anything from request
     *  headers/URLs). History for "Copy diagnostics" lives in [stateMachine]. */
    private fun setState(newState: StreamState, op: String, error: Throwable? = null) {
        val old = state
        val transition = stateMachine.transition(newState, op, error)
        android.util.Log.i(
            TAG,
            "StreamState $old -> $newState (op=$op, camera=${controller?.getCurrentCameraId()}, " +
                "generation=${controller?.currentGeneration()}${transition.error?.let { ", error=$it" } ?: ""})",
        )
    }

    /** Sanitized diagnostics report for the "Copy diagnostics" action: app/device
     *  info, current state, and recent transitions/errors. Never includes the
     *  pairing token, any URL, or raw configuration. */
    fun buildDiagnosticsReport(): String {
        val sb = StringBuilder()
        sb.appendLine("Telescope diagnostics")
        val versionName = runCatching { packageManager.getPackageInfo(packageName, 0).versionName }.getOrNull() ?: "unknown"
        sb.appendLine("App version: $versionName")
        sb.appendLine("Device: ${Build.MANUFACTURER} ${Build.MODEL}, Android ${Build.VERSION.RELEASE} (SDK ${Build.VERSION.SDK_INT})")
        sb.appendLine("Current state: $state")
        val cur = controller?.snapshot()?.currentCamera
        sb.appendLine("Current camera: ${cur?.id ?: "none"} (${cur?.label ?: "-"})")
        sb.appendLine("Recent transitions:")
        val snapshot = stateMachine.recentTransitions()
        if (snapshot.isEmpty()) {
            sb.appendLine("  (none)")
        } else {
            for (t in snapshot) {
                sb.append("  ${t.from} -> ${t.to}  op=${t.op}")
                if (t.error != null) sb.append("  error=${t.error}")
                sb.appendLine()
            }
        }
        return sb.toString()
    }

    // ── Live preview (for PreviewActivity) ───────────────────────────────────

    fun getCameras(): List<CameraEntry> = allCameras
    fun getCurrentCameraId(): String? = controller?.getCurrentCameraId()
    fun getStreamSize(): android.util.Size = android.util.Size(streamWidth, streamHeight)

    fun switchCamera(id: String) {
        val entry = allCameras.find { it.id == id } ?: return
        controller?.switchTo(entry)
    }

    fun attachPreviewSurface(surface: Surface) {
        controller?.attachPreviewSurface(surface)
    }

    fun detachPreviewSurface(onDetached: (() -> Unit)? = null) {
        controller?.detachPreviewSurface(onDetached)
    }

    // ── Lifecycle ─────────────────────────────────────────────────────────────

    override fun onBind(intent: Intent?): IBinder = binder
    override fun onCreate() { super.onCreate(); createNotificationChannel() }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        val cameraId  = intent?.getStringExtra(EXTRA_CAMERA_ID)  ?: "0"
        val logicalId = intent?.getStringExtra(EXTRA_LOGICAL_ID) ?: ""
        streamWidth   = intent?.getIntExtra(EXTRA_WIDTH,  1920)  ?: 1920
        streamHeight  = intent?.getIntExtra(EXTRA_HEIGHT, 1080)  ?: 1080
        val initialOis = intent?.getBooleanExtra(EXTRA_OIS,        true)  ?: true
        val localOnly = intent?.getBooleanExtra(EXTRA_LOCAL_ONLY, false) ?: false
        bindAddr      = if (localOnly) "127.0.0.1" else "0.0.0.0"

        // Must be called unconditionally and early, before any return below: this
        // service starts via startForegroundService(), and Android kills the app if
        // the promotion doesn't happen soon after, regardless of what fails below.
        startForegroundCompat()
        setState(StreamState.StartingServer, "onStartCommand")

        try {
            enumerateAllCameras()
        } catch (e: Exception) {
            // e.g. EADDRINUSE if a just-stopped instance's port hasn't been released yet.
            setState(StreamState.Failed, "startServer", e)
            stopForeground(STOP_FOREGROUND_REMOVE)
            stopSelf()
            return START_NOT_STICKY
        }
        acquireWakeLock()

        val physId = if (logicalId.isNotEmpty()) cameraId else null
        val openId = if (logicalId.isNotEmpty()) logicalId else cameraId
        val initialEntry = allCameras.find { it.id == cameraId }
            ?: CameraEntry(cameraId, logicalId.ifEmpty { null }, "ID $cameraId",
                           initialOis, 50, 3200, 100_000L, 1_000_000_000L)

        controller = CameraSessionController(
            context        = this,
            streamWidth    = streamWidth,
            streamHeight   = streamHeight,
            onFrame        = { bytes -> server?.sendFrame(bytes) },
            onStateChanged = { newState, op, error -> setState(newState, op, error) },
            onFatalError   = { stopSelf() },
        )

        setState(StreamState.OpeningCamera, "onStartCommand")
        controller!!.open(openId, physId, initialEntry, initialOis)
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
            token          = TokenStore.get(this),
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
        val snap = controller?.snapshot()
        val cams = allCameras.map { e ->
            CameraCapability(
                id = e.id, logicalId = e.logicalId, label = e.label, current = (e.id == snap?.currentCamera?.id),
                hasOis = e.hasOis, isoMin = e.isoMin, isoMax = e.isoMax,
                shutterMinNs = e.shutterMinNs, shutterMaxNs = e.shutterMaxNs,
                supportsManualSensor = e.supportsManualSensor, supportsManualWB = e.supportsManualWB,
                supportsManualFocus = e.supportsManualFocus, minFocusDistance = e.minFocusDistance,
                aeCompMin = e.aeCompMin, aeCompMax = e.aeCompMax, aeCompStep = e.aeCompStep,
                supportsFlash = e.supportsFlash, hwLevel = e.hwLevel,
            )
        }
        val (battLevel, battCharging, battTempC) = getBatteryInfo()
        val state = V1State(
            cameras = cams,
            auto = snap?.iso == null,
            iso = snap?.iso,
            shutter_ns = snap?.shutterNs,
            wb_manual = snap?.wbGains != null,
            wb_r = snap?.measuredGains?.red, wb_ge = snap?.measuredGains?.greenEven,
            wb_go = snap?.measuredGains?.greenOdd, wb_b = snap?.measuredGains?.blue,
            ois = snap?.ois ?: true,
            focus_mode = snap?.focusMode ?: "continuous",
            focus_distance = snap?.focusDistance ?: 0f,
            nr_mode = snap?.nrMode ?: CaptureRequest.NOISE_REDUCTION_MODE_FAST,
            edge_mode = snap?.edgeMode ?: CaptureRequest.EDGE_MODE_FAST,
            ae_comp = snap?.aeComp ?: 0,
            black_level_lock = snap?.blackLevelLock ?: false,
            torch = snap?.torch ?: false,
            jpeg_quality = snap?.jpegQuality ?: 85,
            phone_fps = snap?.phoneFps ?: 30,
            battery = battLevel,
            charging = battCharging,
            battery_temp_c = battTempC,
        )
        return Json.encodeToString(V1State.serializer(), state)
    }

    private fun handleControlCommand(params: Map<String, String>): String {
        val ctrl = controller ?: return err("camera not ready")
        return try {
            when (params["action"]) {
                "camera" -> {
                    val id    = params["id"] ?: return err("no id")
                    val entry = allCameras.find { it.id == id } ?: return err("unknown id $id")
                    ctrl.switchTo(entry)
                    ok()
                }
                "iso" -> {
                    val iso = params["value"]?.toIntOrNull() ?: return err("bad iso")
                    ctrl.setIso(iso)
                    ok()
                }
                "shutter" -> {
                    val ns = params["value"]?.toLongOrNull() ?: return err("bad shutter")
                    ctrl.setShutter(ns)
                    ok()
                }
                "auto" -> {
                    ctrl.setAuto()
                    ok()
                }
                "ois" -> {
                    ctrl.setOis(params["value"] == "1")
                    ok()
                }
                "wb_gains" -> {
                    val r  = params["r"]?.toFloatOrNull()  ?: return err("bad r")
                    val ge = params["ge"]?.toFloatOrNull() ?: return err("bad ge")
                    val go = params["go"]?.toFloatOrNull() ?: return err("bad go")
                    val b  = params["b"]?.toFloatOrNull()  ?: return err("bad b")
                    ctrl.setWbGains(RggbChannelVector(r, ge, go, b))
                    ok()
                }
                "wb_auto" -> {
                    ctrl.setWbAuto()
                    ok()
                }
                "jpeg_quality" -> {
                    val q = params["value"]?.toIntOrNull() ?: return err("bad value")
                    ctrl.setJpegQuality(q.coerceIn(1, 100))
                    ok()
                }
                "fps_target" -> {
                    val fps = params["value"]?.toIntOrNull() ?: return err("bad value")
                    ctrl.setFpsTarget(fps.coerceIn(1, 120))
                    ok()
                }
                "focus_mode" -> {
                    val mode = params["value"] ?: return err("no value")
                    if (mode != "continuous" && mode != "manual") return err("bad mode")
                    ctrl.setFocusMode(mode)
                    ok()
                }
                "focus_distance" -> {
                    val d = params["value"]?.toFloatOrNull() ?: return err("bad distance")
                    ctrl.setFocusDistance(d.coerceAtLeast(0f))
                    ok()
                }
                "nr_mode" -> {
                    val m = params["value"]?.toIntOrNull() ?: return err("bad value")
                    ctrl.setNrMode(m.coerceIn(0, 4))
                    ok()
                }
                "edge_mode" -> {
                    val m = params["value"]?.toIntOrNull() ?: return err("bad value")
                    ctrl.setEdgeMode(m.coerceIn(0, 3))
                    ok()
                }
                "ae_comp" -> {
                    val v = params["value"]?.toIntOrNull() ?: return err("bad value")
                    ctrl.setAeComp(v)
                    ok()
                }
                "black_level_lock" -> {
                    ctrl.setBlackLevelLock(params["value"] == "1")
                    ok()
                }
                "torch" -> {
                    ctrl.setTorch(params["value"] == "1")
                    ok()
                }
                else -> err("unknown action '${params["action"]}'")
            }
        } catch (e: Exception) { err(e.message ?: "exception") }
    }

    private fun ok()             = Json.encodeToString(ControlResult.serializer(), ControlResult(ok = true))
    private fun err(msg: String) = Json.encodeToString(ControlResult.serializer(), ControlResult(ok = false, error = msg))

    fun stopStreaming() {
        setState(StreamState.Stopping, "stopStreaming")
        controller?.stop()
        server?.stop()
        wakeLock?.let { if (it.isHeld) it.release() }
        controller = null; server = null
        setState(StreamState.Idle, "stopStreaming")
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
        // FOREGROUND_SERVICE_TYPE_CAMERA is only documented from API 30 (R) on, even
        // though the 3-arg startForeground(id, notification, type) overload itself
        // exists since API 29 (Q) - passing it a level early is what lint flags.
        // Below R, the manifest's android:foregroundServiceType attribute already
        // declares "camera", so the 2-arg overload is sufficient.
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R)
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
