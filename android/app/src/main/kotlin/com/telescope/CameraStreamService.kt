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
    val maxAfRegions: Int = 0,
    val maxAeRegions: Int = 0,
    val activeArray: SensorBox? = null,
    val zoomRatioMax: Float = 1f,     // CONTROL_ZOOM_RATIO's upper end; 1 = can't (Android 10-, physical lenses)
    val cropZoomMax: Float = 1f,      // SCALER_CROP_REGION's max zoom; 1 = the crop can't be set on this camera
    val freeformCrop: Boolean = false, // the crop can sit off-centre
    val lensZooms: List<Float> = emptyList(), // a multi-lens camera's zoom ratios where another lens takes over
)

// The sensor's active pixel array (SENSOR_INFO_ACTIVE_ARRAY_SIZE), kept free of android.graphics.Rect so
// the metering math runs in JVM tests.
data class SensorBox(val left: Int, val top: Int, val width: Int, val height: Int)

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

    // The metering region for a point picked in the stream frame (x, y in 0..1), as left, top, width,
    // height in request coordinates. The stream is the centre crop of `area` (the active array, or the
    // zoom crop) to the stream's aspect ratio (no JPEG rotation is applied), so points map through that
    // crop. size is the square's side as a fraction of the visible frame's shorter side.
    fun meteringRect(x: Float, y: Float, size: Float, area: SensorBox, streamW: Int, streamH: Int): IntArray {
        val (visLeft, visTop, visW, visH) = visibleFrame(area, streamW, streamH)
        val cx = visLeft + x.coerceIn(0f, 1f) * visW
        val cy = visTop + y.coerceIn(0f, 1f) * visH
        val side = (size.coerceIn(0.02f, 1f) * minOf(visW, visH)).coerceAtLeast(1f)
        // Kept inside what the stream shows, which is inside the array.
        val left = (cx - side / 2f).coerceIn(visLeft, visLeft + visW - side)
        val top = (cy - side / 2f).coerceIn(visTop, visTop + visH - side)
        return intArrayOf(left.toInt(), top.toInt(), side.toInt(), side.toInt())
    }

    // SCALER_CROP_REGION for phone-side zoom: 1/crop of the visible frame, centred on (cx, cy) in 0..1 of
    // it and kept inside it. Stream-shaped, so the camera crops nothing more to fit the output.
    fun cropRect(area: SensorBox, crop: Float, cx: Float, cy: Float, streamW: Int, streamH: Int): SensorBox {
        val (visLeft, visTop, visW, visH) = visibleFrame(area, streamW, streamH)
        val w = visW / crop.coerceAtLeast(1f)
        val h = visH / crop.coerceAtLeast(1f)
        val left = (visLeft + cx * visW - w / 2f).coerceIn(visLeft, visLeft + visW - w)
        val top = (visTop + cy * visH - h / 2f).coerceIn(visTop, visTop + visH - h)
        return SensorBox(left.toInt(), top.toInt(), w.toInt(), h.toInt())
    }

    // The centre crop of `area` to the stream's aspect ratio: left, top, width, height.
    private fun visibleFrame(area: SensorBox, streamW: Int, streamH: Int): FloatArray {
        val areaAspect = area.width.toFloat() / area.height
        val streamAspect = if (streamW > 0 && streamH > 0) streamW.toFloat() / streamH else areaAspect
        val visW: Float
        val visH: Float
        if (streamAspect > areaAspect) { visW = area.width.toFloat(); visH = visW / streamAspect }
        else { visH = area.height.toFloat(); visW = visH * streamAspect }
        return floatArrayOf(area.left + (area.width - visW) / 2f, area.top + (area.height - visH) / 2f, visW, visH)
    }

    // 35 mm-equivalent focal length from the real one and the sensor's size (mm); 0 when unknown.
    fun equivalentFocal(focalMm: Float, sensorW: Float, sensorH: Float): Float {
        val diag = sqrt(sensorW * sensorW + sensorH * sensorH)
        return if (focalMm > 0f && diag > 0f) focalMm * 43.27f / diag else 0f
    }

    // The zoom ratios at which a multi-lens camera's other lenses take over: each lens's equivalent focal
    // length over the camera's own, above 1 (not the main lens or the ultra-wide) and reachable, sorted.
    fun lensZooms(ownFocal: Float, lensFocals: List<Float>, zoomRatioMax: Float): List<Float> {
        if (ownFocal <= 0f) return emptyList()
        return lensFocals.map { it / ownFocal }.filter { it > 1.05f && it <= zoomRatioMax }.distinct().sorted()
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
        // Set once the desktop asked for the mic without permission; the phone's setup card then offers it.
        const val PREFS_SETUP      = "setup"
        const val KEY_MIC_WANTED   = "mic_wanted"

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
    private var audio: AudioStreamer? = null
    @Volatile private var micForeground = false
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
    // Checked once: whether this phone has a hardware H.264 encoder at all.
    private val h264Available: Boolean by lazy { H264Encoder.isAvailable() }

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
        sb.appendLine("App version: ${BuildConfig.VERSION_NAME} (build ${BuildConfig.VERSION_CODE})")
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
            onH264         = { bytes, key, config -> server?.sendH264(bytes, key, config) },
            onCodecFailed  = { server?.closeH264Clients() },
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

    private fun equivalentFocal(chars: CameraCharacteristics): Float {
        val focal = chars.get(CameraCharacteristics.LENS_INFO_AVAILABLE_FOCAL_LENGTHS)?.firstOrNull() ?: 0f
        val sensor = chars.get(CameraCharacteristics.SENSOR_INFO_PHYSICAL_SIZE) ?: return 0f
        return CameraRequestSelection.equivalentFocal(focal, sensor.width, sensor.height)
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
            val focalEq  = equivalentFocal(chars).toInt()

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
            val maxAfRegions = chars.get(CameraCharacteristics.CONTROL_MAX_REGIONS_AF) ?: 0
            val maxAeRegions = chars.get(CameraCharacteristics.CONTROL_MAX_REGIONS_AE) ?: 0
            val activeArray = chars.get(CameraCharacteristics.SENSOR_INFO_ACTIVE_ARRAY_SIZE)
                ?.let { SensorBox(it.left, it.top, it.width(), it.height()) }

            // Phone-side zoom. A physical lens goes through its logical parent's request, which sets its
            // crop only if the parent says so, and has no zoom ratio of its own.
            val zoomRatioMax = if (logicalParent == null && Build.VERSION.SDK_INT >= Build.VERSION_CODES.R)
                chars.get(CameraCharacteristics.CONTROL_ZOOM_RATIO_RANGE)?.upper ?: 1f else 1f
            val cropSettable = logicalParent == null || (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P &&
                manager.getCameraCharacteristics(logicalParent).availablePhysicalCameraRequestKeys
                    ?.contains(CaptureRequest.SCALER_CROP_REGION) == true)
            val cropZoomMax = if (cropSettable && activeArray != null)
                chars.get(CameraCharacteristics.SCALER_AVAILABLE_MAX_DIGITAL_ZOOM) ?: 1f else 1f
            val freeformCrop = chars.get(CameraCharacteristics.SCALER_CROPPING_TYPE) ==
                CameraCharacteristics.SCALER_CROPPING_TYPE_FREEFORM
            val multiLens = caps?.contains(
                CameraCharacteristics.REQUEST_AVAILABLE_CAPABILITIES_LOGICAL_MULTI_CAMERA) == true
            val lensZooms = if (multiLens && Build.VERSION.SDK_INT >= Build.VERSION_CODES.P)
                CameraRequestSelection.lensZooms(equivalentFocal(chars),
                    chars.physicalCameraIds.map { equivalentFocal(manager.getCameraCharacteristics(it)) },
                    zoomRatioMax)
            else emptyList()

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
            val pStr = if (logicalParent != null) " [phys]" else if (multiLens) " [auto]" else ""
            CameraEntry(id, logicalParent, "$facing $fStr$oStr$pStr", hasOis,
                        isoMin, isoMax, shtMinNs, shtMaxNs,
                        supportsManualSensor, supportsManualWB, supportsManualFocus, minFocusDist, hwLevel,
                        aeCompMin, aeCompMax, aeCompStep, supportsFlash,
                        aeFpsRanges, afModes, nrModes, edgeModes, supportedSizes,
                        maxAfRegions, maxAeRegions, activeArray,
                        zoomRatioMax.coerceAtLeast(1f), cropZoomMax.coerceAtLeast(1f), freeformCrop, lensZooms)
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
            tokens         = { PairedComputers.tokens(this) },
            onVideoClient  = ::onVideoClient,
            requestKeyFrame = { controller?.requestKeyFrame() },
            startAudio     = ::startAudio,
            stopAudio      = { audio?.stop() },
        ).also { it.start() }
        startIdleWatchdog()
    }

    // Null when the mic is recording; otherwise the reason the desktop shows.
    private fun startAudio(): String? {
        val mic = audio ?: AudioStreamer(this) { chunk -> server?.sendAudio(chunk) }.also { audio = it }
        if (!mic.permitted()) {
            // The setup card on the phone offers it from now on.
            getSharedPreferences(PREFS_SETUP, MODE_PRIVATE).edit().putBoolean(KEY_MIC_WANTED, true).apply()
            return "Allow the microphone in Telescope on the phone"
        }
        if (!micForeground) {
            // Recording in the background needs the microphone service type, added now the permission is there.
            try { startForegroundCompat(withMic = true) } catch (_: Exception) {
                return "Open Telescope on the phone once, then try again"
            }
        }
        return if (mic.start()) null else "The phone's microphone is in use"
    }

    // The newest viewer's route picks the codec; viewers of the other one lose their stream.
    private fun onVideoClient(codec: String) {
        val ctrl = controller ?: return
        if (codec == H264Stream.CODEC_H264 && !h264Available) return  // the reader sees no data and gives up
        if (ctrl.snapshot().codec == codec) return
        if (codec == H264Stream.CODEC_MJPEG) server?.closeH264Clients()
        ctrl.setCodec(codec)
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
                supportsFocusPoint = e.maxAfRegions > 0 && e.activeArray != null &&
                    CaptureRequest.CONTROL_AF_MODE_AUTO in e.afModes,
                zoomRatioMax = e.zoomRatioMax, cropZoomMax = e.cropZoomMax, freeformCrop = e.freeformCrop,
                lensZooms = e.lensZooms,
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
            codecs = if (h264Available) listOf(H264Stream.CODEC_MJPEG, H264Stream.CODEC_H264)
                     else listOf(H264Stream.CODEC_MJPEG),
            codec = snap?.codec ?: H264Stream.CODEC_MJPEG,
            bitrate = snap?.bitrate ?: 0,
            codec_error = snap?.codecError,
            active_lens = snap?.activeLens,
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
                "bitrate" -> {
                    // bits per second; 0 sizes it from the resolution and fps
                    val bps = params["value"]?.toIntOrNull() ?: return err("bad value")
                    ctrl.setBitrate(bps.coerceAtLeast(0))
                    ok()
                }
                "focus_point" -> {
                    // x, y: 0..1 in the stream frame as the phone sends it; size: fraction of its shorter side
                    val x = params["x"]?.toFloatOrNull() ?: return err("bad x")
                    val y = params["y"]?.toFloatOrNull() ?: return err("bad y")
                    val size = params["size"]?.toFloatOrNull() ?: 0.1f
                    if (!ctrl.setFocusPoint(x, y, size)) return err("this lens can't focus on a point")
                    ok()
                }
                "zoom" -> {
                    // ratio: centred zoom; crop: extra zoom inside that; x, y: the crop's centre, 0..1 of the view
                    val ratio = params["ratio"]?.toFloatOrNull() ?: return err("bad ratio")
                    val crop  = params["crop"]?.toFloatOrNull()  ?: return err("bad crop")
                    val x     = params["x"]?.toFloatOrNull()     ?: return err("bad x")
                    val y     = params["y"]?.toFloatOrNull()     ?: return err("bad y")
                    ctrl.setZoom(ZoomRequest(ratio.coerceAtLeast(1f), crop.coerceAtLeast(1f),
                                             x.coerceIn(0f, 1f), y.coerceIn(0f, 1f)))
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
        audio?.stop()
        wakeLock?.let { if (it.isHeld) it.release() }
        controller = null; server = null
        setState(StreamState.Idle, op)
        // Release session; endpoint stays bound if MainActivity holds a reference.
        SessionEndpoint.release(SessionEndpoint.OWNER_SERVICE)
        stopForeground(STOP_FOREGROUND_REMOVE); stopSelf()
    }

    private fun createNotificationChannel() {
        val ch = NotificationChannel(CHANNEL_ID, "Streaming", NotificationManager.IMPORTANCE_LOW)
            .apply { description = "Shown while the camera is streaming to your computer" }
        (getSystemService(NOTIFICATION_SERVICE) as NotificationManager).createNotificationChannel(ch)
    }

    private fun startForegroundCompat(withMic: Boolean = AudioStreamer.permitted(this)) {
        val pi = PendingIntent.getActivity(this, 0,
            Intent(this, MainActivity::class.java), PendingIntent.FLAG_IMMUTABLE)
        val n = NotificationCompat.Builder(this, CHANNEL_ID)
            .setContentTitle("Telescope").setContentText("Camera is streaming")
            .setSmallIcon(R.drawable.ic_notification)
            .setColor(ContextCompat.getColor(this, R.color.colorPrimary))
            .setColorized(false)
            .setContentIntent(pi).setOngoing(true).build()
        // Type parameter only works on R+; pre-R relies on manifest declaration.
        // Microphone only once it's allowed: Android refuses a type whose permission is missing.
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            val types = ServiceInfo.FOREGROUND_SERVICE_TYPE_CAMERA or
                (if (withMic) ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE else 0)
            startForeground(NOTIF_ID, n, types)
        } else {
            startForeground(NOTIF_ID, n)
        }
        micForeground = withMic || Build.VERSION.SDK_INT < Build.VERSION_CODES.R
    }

    private fun acquireWakeLock() {
        val pm = getSystemService(POWER_SERVICE) as PowerManager
        wakeLock = pm.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "telescope::stream")
        wakeLock?.acquire(12 * 60 * 60 * 1000L)
    }
}
