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
    val supportedSizes: List<android.util.Size> = emptyList(),
)

// Pure Camera2 request-parameter selection logic; no device/service state for JVM testability
object CameraRequestSelection {
    // Picks advertised FPS range closest to target; prefers ranges containing target
    fun pickAeFpsRange(available: List<Range<Int>>, target: Int): Range<Int>? {
        if (available.isEmpty()) return null
        val containing = available.filter { target in it.lower..it.upper }
        if (containing.isNotEmpty()) return containing.maxByOrNull { it.lower }
        return available.minByOrNull { kotlin.math.abs(it.upper - target) }
    }

    // Chooses AF mode; prefers CONTINUOUS_VIDEO, falls back to PICTURE, AUTO, OFF
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

    fun pickNrMode(available: Set<Int>, requested: Int): Int? = pickMode(
        available, requested,
        listOf(CaptureRequest.NOISE_REDUCTION_MODE_FAST, CaptureRequest.NOISE_REDUCTION_MODE_OFF)
    )

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
        const val EXTRA_REMOTE     = "remote"
        const val CHANNEL_ID       = "telescope_stream"
        const val NOTIF_ID         = 1
        const val DEFAULT_PORT     = 8080
        private const val TAG      = "CameraStreamService"

        // Fires when desktop is genuinely gone (no authorized /v1/state polls in this interval).
        private const val IDLE_STOP_MS = 60_000L
        private const val IDLE_CHECK_INTERVAL_MS = 5_000L

        // The live service, or null when none is running. Neither MainActivity (binds without BIND_AUTO_CREATE) nor SessionServer (unbound socket thread) has another way to reach it. Cleared in onDestroy, so this can't outlive the instance.
        @Volatile
        var instance: CameraStreamService? = null
            private set
    }

    inner class LocalBinder : Binder() {
        fun getService(): CameraStreamService = this@CameraStreamService
    }
    private val binder = LocalBinder()

    private var controller: CameraSessionController? = null
    private var server: MjpegServer? = null
    private var wakeLock: PowerManager.WakeLock? = null
    private var idleWatchdogThread: Thread? = null
    private val idleWatchdogRunning = java.util.concurrent.atomic.AtomicBoolean(false)
    private val mainHandler = android.os.Handler(android.os.Looper.getMainLooper())

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

    // True when this session was started by the desktop rather than the button on this phone; MainActivity uses it to tell the user where an unrequested stream came from.
    @Volatile
    var startedRemotely: Boolean = false
        private set

    // Records a state transition with sanitized context (class name + message only, never a stack trace or request data); history for "Copy diagnostics" lives in stateMachine.
    private fun setState(newState: StreamState, op: String, error: Throwable? = null) {
        val old = state
        val transition = stateMachine.transition(newState, op, error)
        android.util.Log.i(
            TAG,
            "StreamState $old -> $newState (op=$op, camera=${controller?.getCurrentCameraId()}, " +
                "generation=${controller?.currentGeneration()}${transition.error?.let { ", error=$it" } ?: ""})",
        )
    }

    // Records a non-fatal control-update failure (e.g. a live exposure/WB change that failed) into the same sanitized history, without changing state or tearing the session down.
    private fun recordControlError(op: String, error: Throwable) {
        val transition = stateMachine.record(op, error)
        android.util.Log.w(
            TAG,
            "Non-fatal control error (op=$op, camera=${controller?.getCurrentCameraId()}, " +
                "generation=${controller?.currentGeneration()}${transition.error?.let { ", error=$it" } ?: ""})",
        )
    }

    // Sanitized diagnostics report for "Copy diagnostics": app/device info, current state, recent transitions/errors. Never includes the pairing token, a URL, or raw config.
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

    fun getCameras(): List<CameraEntry> = allCameras
    fun getCurrentCameraId(): String? = controller?.getCurrentCameraId()

    // Live camera/OIS/resolution state for MainActivity spinners to stay in sync
    fun getControlSnapshot(): CameraControlSnapshot? = controller?.snapshot()
    fun getStreamSize(): android.util.Size =
        controller?.getStreamSize() ?: android.util.Size(streamWidth, streamHeight)

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

    override fun onBind(intent: Intent?): IBinder = binder

    override fun onCreate() {
        super.onCreate()
        instance = this
        createNotificationChannel()
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        val cameraId  = intent?.getStringExtra(EXTRA_CAMERA_ID)  ?: "0"
        val logicalId = intent?.getStringExtra(EXTRA_LOGICAL_ID) ?: ""
        streamWidth   = intent?.getIntExtra(EXTRA_WIDTH,  1920)  ?: 1920
        streamHeight  = intent?.getIntExtra(EXTRA_HEIGHT, 1080)  ?: 1080
        val initialOis = intent?.getBooleanExtra(EXTRA_OIS,        true)  ?: true
        val localOnly = intent?.getBooleanExtra(EXTRA_LOCAL_ONLY, false) ?: false
        bindAddr      = if (localOnly) "127.0.0.1" else "0.0.0.0"
        startedRemotely = intent?.getBooleanExtra(EXTRA_REMOTE, false) ?: false

        // Must be called early: Android kills app if foreground promotion doesn't happen soon
        startForegroundCompat()
        // Keep session reachable after MainActivity loses focus (idempotent refcount)
        SessionEndpoint.acquire(this, SessionEndpoint.OWNER_SERVICE)
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
            context             = this,
            initialStreamWidth  = streamWidth,
            initialStreamHeight = streamHeight,
            onFrame        = { bytes -> server?.sendFrame(bytes) },
            onStateChanged = { newState, op, error -> setState(newState, op, error) },
            onFatalError   = { stopSelf() },
            onControlError = { op, error -> recordControlError(op, error) },
        )

        setState(StreamState.OpeningCamera, "onStartCommand")
        controller!!.open(openId, physId, initialEntry, initialOis)
        return START_NOT_STICKY
    }

    override fun onDestroy() {
        stopStreaming()
        instance = null
        super.onDestroy()
    }

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

            val streamMap = chars.get(CameraCharacteristics.SCALER_STREAM_CONFIGURATION_MAP)
            val supportedSizes = streamMap?.getOutputSizes(ImageFormat.JPEG)
                ?.sortedByDescending { it.width * it.height }
                ?.takeIf { it.isNotEmpty() }
                ?.toList()
                ?: listOf(android.util.Size(1920, 1080), android.util.Size(1280, 720))

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
                        aeFpsRanges, afModes, nrModes, edgeModes, supportedSizes)
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

    private fun startServer() {
        server = MjpegServer(
            port           = DEFAULT_PORT,
            getCamerasJson = ::buildCamerasJson,
            handleControl  = ::handleControlCommand,
            bindAddr       = bindAddr,
            token          = TokenStore.get(this),
        ).also { it.start() }
        startIdleWatchdog()
    }

    private fun startIdleWatchdog() {
        idleWatchdogRunning.set(true)
        idleWatchdogThread = kotlin.concurrent.thread(name = "idle-watchdog", isDaemon = true) {
            while (idleWatchdogRunning.get()) {
                Thread.sleep(IDLE_CHECK_INTERVAL_MS)
                if (!idleWatchdogRunning.get()) break
                val srv = server ?: continue
                val watchedLocally = controller?.hasPreviewSurface() == true
                if (!watchedLocally && srv.idleForMs() >= IDLE_STOP_MS) {
                    android.util.Log.i(TAG, "No desktop activity for ${IDLE_STOP_MS / 1000}s - stopping to save battery")
                    mainHandler.post { stopStreaming("idleWatchdog") }
                    break
                }
            }
        }
    }

    private fun stopIdleWatchdog() {
        idleWatchdogRunning.set(false)
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
                supportedSizes = e.supportedSizes.map { CameraSize(it.width, it.height) },
            )
        }
        val (battLevel, battCharging, battTempC) = getBatteryInfo()
        val liveSize = controller?.getStreamSize() ?: android.util.Size(streamWidth, streamHeight)
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
            stream_width = liveSize.width,
            stream_height = liveSize.height,
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
                "resolution" -> {
                    val w = params["width"]?.toIntOrNull()  ?: return err("bad width")
                    val h = params["height"]?.toIntOrNull() ?: return err("bad height")
                    if (w <= 0 || h <= 0) return err("bad size")
                    ctrl.switchResolution(w, h)
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

    fun stopStreaming(op: String = "stopStreaming") {
        stopIdleWatchdog()
        setState(StreamState.Stopping, op)
        controller?.stop()
        server?.stop()
        wakeLock?.let { if (it.isHeld) it.release() }
        controller = null; server = null
        setState(StreamState.Idle, op)
        // Release session; endpoint stays bound if MainActivity holds a reference.
        SessionEndpoint.release(SessionEndpoint.OWNER_SERVICE)
        stopForeground(STOP_FOREGROUND_REMOVE); stopSelf()
    }

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
        // Type parameter only works on R+; pre-R relies on manifest declaration.
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
