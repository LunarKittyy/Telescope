package com.telescope

import android.Manifest
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.PackageManager
import android.graphics.ImageFormat
import android.graphics.SurfaceTexture
import android.hardware.camera2.CameraCaptureSession
import android.hardware.camera2.CameraCharacteristics
import android.hardware.camera2.CameraDevice
import android.hardware.camera2.CameraManager
import android.hardware.camera2.CameraMetadata
import android.hardware.camera2.CaptureFailure
import android.hardware.camera2.CaptureRequest
import android.hardware.camera2.CaptureResult
import android.hardware.camera2.TotalCaptureResult
import android.hardware.camera2.params.OutputConfiguration
import android.hardware.camera2.params.SessionConfiguration
import android.hardware.camera2.params.StreamConfigurationMap
import android.media.ImageReader
import android.os.BatteryManager
import android.os.Build
import android.os.Handler
import android.os.HandlerThread
import android.os.Looper
import android.os.Message
import android.os.PowerManager
import android.os.SystemClock
import android.util.Log
import android.util.Size
import android.view.Surface
import androidx.core.content.ContextCompat
import java.io.File
import java.util.concurrent.Executor
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors

// Live progress update posted from the probe worker to the main thread for UI rendering only.
data class ProbeProgress(
    val lensIndex: Int,
    val totalLenses: Int,
    val lensLabel: String,
    val stepLabel: String,
    val elapsedMs: Long,
)

// Testable seam for cancellation handles, single-threaded camera scheduling and Camera2 hardware access.
internal fun interface ProbeCancellable {
    fun cancel()
}

internal interface ProbeWorker {
    fun post(block: () -> Unit): Boolean
    fun postDelayed(delayMs: Long, block: () -> Unit): ProbeCancellable
    fun quit()
}

internal interface ProbeLensHost {
    val worker: ProbeWorker
    val isFinished: Boolean
    val isStopping: Boolean
    fun checkStopBeforeStep(): Boolean
    fun postProgress(stepLabel: String)
    fun onLensFinished(outcome: ProbeOutcome, detail: String? = null)
    fun onFatalFailure(reason: String)
}

internal interface CameraProbeBackend {
    val betweenLensesMs: Long
    fun hasCameraPermission(): Boolean
    fun isStreamActive(): Boolean
    fun readThermalSnapshot(overrideStatus: Int? = null): ProbeThermalSnapshot
    fun registerThermalMonitor(worker: ProbeWorker, onHot: (ProbeThermalSnapshot) -> Unit): ProbeCancellable
    fun reportHeader(): String
    fun wallTimeMs(): Long
    fun monoTimeMs(): Long
    fun cacheFile(): File
    fun logWarn(message: String, t: Throwable? = null)
    fun createWorker(onUnhandledError: (Throwable) -> Unit): ProbeWorker
    fun postMain(block: () -> Unit)
    fun enumerateLenses(): List<ProbeLens>
    fun probeLens(lens: ProbeLens, acc: ProbeSessionAccumulator, host: ProbeLensHost)
    fun closeActive(abort: Boolean)

    // Calls onSettled on the camera thread once every pending openCamera has reported back and been closed if stale.
    fun whenOpensSettled(onSettled: () -> Unit)
}

// Tracks openCamera calls whose callback has not arrived yet, so teardown can wait for them. Camera thread only.
internal class PendingOpenTracker {
    private val pending = HashSet<Int>()
    private val waiters = mutableListOf<() -> Unit>()
    private var nextToken = 0

    fun begin(): Int {
        val token = nextToken++
        pending.add(token)
        return token
    }

    fun resolve(token: Int) {
        if (!pending.remove(token) || pending.isNotEmpty()) return
        val ready = waiters.toList()
        waiters.clear()
        ready.forEach { it() }
    }

    // Runs release (closing a stale device) before resolving, so waiters like the camera lease see the device already closed.
    fun resolveAfter(token: Int, release: () -> Unit) {
        try { release() } finally { resolve(token) }
    }

    fun whenSettled(onSettled: () -> Unit) {
        if (pending.isEmpty()) onSettled() else waiters.add(onSettled)
    }
}

internal object ProbeOpenPolicy {
    // A disconnect only fails the run when it hits the lens being probed right now and the run is still going.
    fun disconnectIsFatal(runFinished: Boolean, isCurrentLens: Boolean): Boolean = !runFinished && isCurrentLens
}

// Runs the full Camera2 probe on every lens, main lens first, with no total time limit.
// Threading: one dedicated worker thread owns every CameraDevice, session, ImageReader and timed step;
// a separate single-thread executor formats and writes the cache file; main-thread callbacks only update the UI.
class CameraProbeRunner internal constructor(
    private val backend: CameraProbeBackend,
    private val listener: Listener,
    private val ownership: CameraOwnerLock = CameraOwnership,
) {
    constructor(
        context: Context,
        listener: Listener,
        ownership: CameraOwnerLock = CameraOwnership,
    ) : this(AndroidCameraProbeBackend(context.applicationContext), listener, ownership)

    interface Listener {
        fun onProgress(progress: ProbeProgress)
        fun onFinished(data: ProbeReportData, reportText: String, savedToCache: Boolean)
    }

    sealed interface StartResult {
        data object Started : StartResult
        data object StreamRunning : StartResult
        data object NoPermission : StartResult
        data class TooHot(val thermal: ProbeThermalSnapshot) : StartResult
        data class Failed(val detail: String) : StartResult
    }

    private companion object {
        const val OPEN_SETTLE_TIMEOUT_MS = 11_000L
    }

    private var lease: CameraLease? = null
    private var worker: ProbeWorker? = null
    private var writerExecutor: ExecutorService? = null
    private var thermalMonitor: ProbeCancellable? = null
    private var nextLensCancellable: ProbeCancellable? = null

    private var startedAtWallMs = 0L
    private var startedAtMonoMs = 0L
    private var accumulator: ProbeSessionAccumulator? = null
    private var orderedLenses: List<ProbeLens> = emptyList()
    private var currentLensIndex = 0

    @Volatile
    private var finished = false

    @Volatile
    private var stopStatus: ProbeRunStatus? = null

    @Volatile
    private var stopReason: String? = null

    // Validates permission, thermal headroom and exclusive camera ownership before starting worker threads.
    fun start(): StartResult {
        if (!backend.hasCameraPermission()) {
            return StartResult.NoPermission
        }
        val initialThermal = backend.readThermalSnapshot()
        if (ProbeThermalPolicy.tooHotToStart(initialThermal)) {
            return StartResult.TooHot(initialThermal)
        }
        val acquired = ownership.tryAcquire(CameraOwner.PROBE, adopted = true)
            ?: return StartResult.StreamRunning
        if (backend.isStreamActive()) {
            ownership.release(acquired.id)
            return StartResult.StreamRunning
        }

        lease = acquired
        startedAtWallMs = backend.wallTimeMs()
        startedAtMonoMs = backend.monoTimeMs()
        try {
            val createdWorker = backend.createWorker { t ->
                backend.logWarn("Unexpected error during Camera2 probe", t)
                finishOnCameraThread(
                    ProbeRunStatus.FAILED,
                    "Unexpected error: ${t.javaClass.simpleName}: ${t.message}",
                )
            }
            worker = createdWorker
            writerExecutor = Executors.newSingleThreadExecutor { r ->
                Thread(r, "camera-probe-writer").apply { isDaemon = true }
            }
            thermalMonitor = backend.registerThermalMonitor(createdWorker) { snapshot ->
                requestStop(
                    ProbeRunStatus.THERMAL_STOP,
                    "Stopped because phone heat rose (${snapshot.summary()})",
                )
            }
            if (!createdWorker.post { beginOnCameraThread(initialThermal) }) {
                throw IllegalStateException("Camera probe worker rejected startup")
            }
            return StartResult.Started
        } catch (e: Exception) {
            backend.logWarn("Failed to start Camera2 probe", e)
            runCatching { thermalMonitor?.cancel() }
            thermalMonitor = null
            runCatching { worker?.quit() }
            worker = null
            runCatching { writerExecutor?.shutdownNow() }
            writerExecutor = null
            releaseOwnership()
            return StartResult.Failed("${e.javaClass.simpleName}: ${e.message}")
        }
    }

    // Requests an immediate cancellation from any thread while preserving every partial measurement collected so far.
    fun cancel(reason: String = "Cancelled by user") {
        requestStop(ProbeRunStatus.CANCELLED, reason)
    }

    private fun requestStop(status: ProbeRunStatus, reason: String) {
        synchronized(this) {
            if (stopStatus == null) {
                stopStatus = status
                stopReason = reason
            }
        }
        worker?.post {
            val s = stopStatus ?: status
            val r = stopReason ?: reason
            finishOnCameraThread(s, r)
        }
    }

    private fun beginOnCameraThread(initialThermal: ProbeThermalSnapshot) {
        if (checkStopBeforeStep()) return
        val probeLenses = runCatching { backend.enumerateLenses() }.getOrElse { e ->
            finishOnCameraThread(ProbeRunStatus.FAILED, "Could not enumerate cameras: ${e.message}")
            return
        }
        orderedLenses = ProbePlan.orderLenses(probeLenses)
        accumulator = ProbeSessionAccumulator(
            header = backend.reportHeader(),
            startedAtMs = startedAtWallMs,
            initialThermal = initialThermal,
            orderedLenses = orderedLenses,
        )
        if (orderedLenses.isEmpty()) {
            finishOnCameraThread(ProbeRunStatus.COMPLETED, "No cameras found")
            return
        }
        currentLensIndex = 0
        startNextLens()
    }

    private fun startNextLens() {
        if (checkStopBeforeStep()) return
        if (currentLensIndex >= orderedLenses.size) {
            finishOnCameraThread(ProbeRunStatus.COMPLETED, null)
            return
        }
        val lens = orderedLenses[currentLensIndex]
        val acc = accumulator ?: return
        val activeWorker = worker ?: return
        acc.beginLens(lens)
        postProgress(lens, "Reading characteristics")

        val host = object : ProbeLensHost {
            override val worker: ProbeWorker get() = activeWorker
            override val isFinished: Boolean get() = finished
            override val isStopping: Boolean get() = stopStatus != null

            override fun checkStopBeforeStep(): Boolean = this@CameraProbeRunner.checkStopBeforeStep()

            override fun postProgress(stepLabel: String) {
                this@CameraProbeRunner.postProgress(lens, stepLabel)
            }

            override fun onLensFinished(outcome: ProbeOutcome, detail: String?) {
                if (finished) return
                backend.closeActive(abort = false)
                acc.finishLens(outcome, detail)
                checkpointReport()
                currentLensIndex++
                postNextLens()
            }

            override fun onFatalFailure(reason: String) {
                if (finished) return
                finishOnCameraThread(ProbeRunStatus.FAILED, reason)
            }
        }
        backend.probeLens(lens, acc, host)
    }

    private fun postNextLens() {
        val activeWorker = worker ?: return
        if (finished) return
        nextLensCancellable?.cancel()
        nextLensCancellable = activeWorker.postDelayed(backend.betweenLensesMs) {
            nextLensCancellable = null
            startNextLens()
        }
    }

    private fun checkStopBeforeStep(): Boolean {
        if (finished) return true
        val requestedStatus = stopStatus
        if (requestedStatus != null) {
            finishOnCameraThread(requestedStatus, stopReason)
            return true
        }
        val thermal = backend.readThermalSnapshot()
        if (ProbeThermalPolicy.tooHotToContinue(thermal)) {
            finishOnCameraThread(
                ProbeRunStatus.THERMAL_STOP,
                "Stopped because phone heat rose (${thermal.summary()})",
            )
            return true
        }
        return false
    }

    // Writes completed lenses incrementally on the dedicated writer worker so progress survives an unexpected kill.
    private fun checkpointReport() {
        val acc = accumulator ?: return
        val snapshot = acc.snapshot(
            status = ProbeRunStatus.INTERRUPTED,
            stopReason = "in progress checkpoint",
            finishedAtMs = backend.wallTimeMs(),
            finalThermal = backend.readThermalSnapshot(),
        )
        val executor = writerExecutor ?: return
        runCatching {
            executor.execute {
                val text = CameraProbeReport.format(snapshot)
                CameraProbeCache.write(backend.cacheFile(), text)
            }
        }
    }

    private fun finishOnCameraThread(status: ProbeRunStatus, reason: String?) {
        if (finished) return
        finished = true
        runCatching { nextLensCancellable?.cancel() }
        nextLensCancellable = null
        runCatching { thermalMonitor?.cancel() }
        thermalMonitor = null
        runCatching { backend.closeActive(abort = status != ProbeRunStatus.COMPLETED) }
        releaseWhenOpensSettled()

        val finalThermal = runCatching { backend.readThermalSnapshot() }
            .getOrDefault(ProbeThermalSnapshot(status = 0))
        val acc = accumulator
        val data = acc?.snapshot(
            status = status,
            stopReason = reason,
            finishedAtMs = backend.wallTimeMs(),
            finalThermal = finalThermal,
        ) ?: ProbeReportData(
            header = backend.reportHeader(),
            startedAtMs = startedAtWallMs,
            finishedAtMs = backend.wallTimeMs(),
            status = status,
            stopReason = reason,
            initialThermal = finalThermal,
            finalThermal = finalThermal,
            totalLenses = 0,
            completedLenses = 0,
            lenses = emptyList(),
        )

        val executor = writerExecutor
        writerExecutor = null
        if (executor != null) {
            runCatching {
                executor.execute {
                    val reportText = CameraProbeReport.format(data)
                    val saved = CameraProbeCache.write(backend.cacheFile(), reportText)
                    backend.postMain { listener.onFinished(data, reportText, saved) }
                }
                executor.shutdown()
            }.onFailure {
                val reportText = CameraProbeReport.format(data)
                backend.postMain { listener.onFinished(data, reportText, false) }
            }
        } else {
            val reportText = CameraProbeReport.format(data)
            backend.postMain { listener.onFinished(data, reportText, false) }
        }
    }

    // A cancel can land while openCamera is still pending, so ownership and the worker stay until its callback closes the device.
    private fun releaseWhenOpensSettled() {
        val activeWorker = worker
        worker = null
        var released = false
        var guard: ProbeCancellable? = null
        val release = {
            if (!released) {
                released = true
                runCatching { guard?.cancel() }
                releaseOwnership()
                runCatching { activeWorker?.quit() }
            }
        }
        guard = activeWorker?.postDelayed(OPEN_SETTLE_TIMEOUT_MS) { release() }
        runCatching { backend.whenOpensSettled(release) }.onFailure { release() }
    }

    private fun releaseOwnership() {
        val held = lease ?: return
        lease = null
        ownership.release(held.id)
    }

    private fun postProgress(lens: ProbeLens, stepLabel: String) {
        val elapsedMs = (backend.monoTimeMs() - startedAtMonoMs).coerceAtLeast(0L)
        val progress = ProbeProgress(
            lensIndex = currentLensIndex,
            totalLenses = orderedLenses.size,
            lensLabel = lens.label,
            stepLabel = stepLabel,
            elapsedMs = elapsedMs,
        )
        backend.postMain { listener.onProgress(progress) }
    }
}

// Process-scoped coordinator for the active Camera2 probe so Activity recreation (theme or rotation)
// keeps a long run alive and reattaches the new Activity instance to live progress or its finished result.
open class ProbeSessionCoordinator(
    private val monoClock: () -> Long,
    private val runnerFactory: (CameraProbeRunner.Listener) -> CameraProbeRunner,
) {
    data class FinishedResult(
        val data: ProbeReportData,
        val reportText: String,
        val savedToCache: Boolean,
    )

    data class Snapshot(
        val running: Boolean,
        val cancelling: Boolean,
        val startedMonoMs: Long,
        val lastProgress: ProbeProgress?,
        val lastFinished: FinishedResult?,
    )

    interface Listener {
        fun onProgress(progress: ProbeProgress)
        fun onFinished(result: FinishedResult)
    }

    private var activeRunner: CameraProbeRunner? = null
    private var running = false
    private var cancelling = false
    private var startedMonoMs = 0L
    private var lastProgress: ProbeProgress? = null
    private var lastFinished: FinishedResult? = null
    private var uiListener: Listener? = null

    @Synchronized
    fun isRunning(): Boolean = running

    @Synchronized
    fun snapshot(): Snapshot = Snapshot(
        running = running,
        cancelling = cancelling,
        startedMonoMs = startedMonoMs,
        lastProgress = lastProgress,
        lastFinished = lastFinished,
    )

    @Synchronized
    fun attach(listener: Listener): Snapshot {
        uiListener = listener
        return snapshot()
    }

    @Synchronized
    fun detach(listener: Listener) {
        if (uiListener === listener) uiListener = null
    }

    fun start(): CameraProbeRunner.StartResult {
        lateinit var probe: CameraProbeRunner
        val internalListener = object : CameraProbeRunner.Listener {
            override fun onProgress(progress: ProbeProgress) {
                val target = synchronized(this@ProbeSessionCoordinator) {
                    if (activeRunner !== probe || !running) return
                    lastProgress = progress
                    uiListener
                }
                target?.onProgress(progress)
            }

            override fun onFinished(data: ProbeReportData, reportText: String, savedToCache: Boolean) {
                val result = FinishedResult(data, reportText, savedToCache)
                val target = synchronized(this@ProbeSessionCoordinator) {
                    if (activeRunner !== probe) return
                    activeRunner = null
                    running = false
                    cancelling = false
                    lastFinished = result
                    uiListener
                }
                target?.onFinished(result)
            }
        }
        val previousFinished: FinishedResult?
        synchronized(this) {
            if (running) return CameraProbeRunner.StartResult.Started
            previousFinished = lastFinished
            probe = runnerFactory(internalListener)
            activeRunner = probe
            running = true
            cancelling = false
            startedMonoMs = monoClock()
            lastProgress = null
            lastFinished = null
        }
        val result = probe.start()
        if (result !is CameraProbeRunner.StartResult.Started) {
            synchronized(this) {
                if (activeRunner === probe) {
                    activeRunner = null
                    running = false
                    cancelling = false
                    lastFinished = previousFinished
                }
            }
        }
        return result
    }

    fun cancel(reason: String = "Cancelled by user") {
        val probe = synchronized(this) {
            if (!running) return
            cancelling = true
            activeRunner
        }
        probe?.cancel(reason)
    }
}

object CameraProbeSession {
    @Volatile
    private var appContext: Context? = null

    private val coordinator = ProbeSessionCoordinator(
        monoClock = { SystemClock.elapsedRealtime() },
        runnerFactory = { listener -> CameraProbeRunner(requireNotNull(appContext), listener) },
    )

    fun isRunning(): Boolean = coordinator.isRunning()
    fun snapshot(): ProbeSessionCoordinator.Snapshot = coordinator.snapshot()
    fun attach(listener: ProbeSessionCoordinator.Listener): ProbeSessionCoordinator.Snapshot = coordinator.attach(listener)
    fun detach(listener: ProbeSessionCoordinator.Listener) = coordinator.detach(listener)

    fun start(context: Context): CameraProbeRunner.StartResult {
        appContext = context.applicationContext
        return coordinator.start()
    }

    fun cancel(reason: String = "Cancelled by user") = coordinator.cancel(reason)
}

// Android Camera2 implementation of CameraProbeBackend.
private class AndroidCameraProbeBackend(
    private val appContext: Context,
) : CameraProbeBackend {

    private class AndroidProbeWorker(
        private val onUnhandledError: (Throwable) -> Unit,
    ) : ProbeWorker {
        private val thread = HandlerThread("CameraProbeCam").apply { start() }
        val handler: Handler = object : Handler(thread.looper) {
            override fun dispatchMessage(msg: Message) {
                try {
                    super.dispatchMessage(msg)
                } catch (t: Throwable) {
                    onUnhandledError(t)
                }
            }
        }

        override fun post(block: () -> Unit): Boolean = handler.post(block)

        override fun postDelayed(delayMs: Long, block: () -> Unit): ProbeCancellable {
            val runnable = Runnable { block() }
            handler.postDelayed(runnable, delayMs)
            return ProbeCancellable { handler.removeCallbacks(runnable) }
        }

        override fun quit() {
            runCatching { thread.quitSafely() }
        }
    }

    private class LensContext(
        val lens: ProbeLens,
        val chars: CameraCharacteristics,
        val openId: String,
        val physId: String?,
        val physicalRequestKeys: Set<CaptureRequest.Key<*>>,
        val map: StreamConfigurationMap?,
        val capabilities: Set<Int>,
    ) {
        var device: CameraDevice? = null
    }

    private data class BurstSpec(
        val formatName: String,
        val imageFormat: Int,
        val requiresRawCapability: Boolean,
    )

    private data class ProcessingModePair(
        val label: String,
        val askedNr: Int?,
        val askedEdge: Int?,
    )

    private val manager = appContext.getSystemService(Context.CAMERA_SERVICE) as CameraManager
    private val powerManager = appContext.getSystemService(Context.POWER_SERVICE) as PowerManager
    private val mainHandler = Handler(Looper.getMainLooper())

    private var androidWorker: AndroidProbeWorker? = null
    private val cameraHandler: Handler? get() = androidWorker?.handler
    private var currentHost: ProbeLensHost? = null
    private var accumulator: ProbeSessionAccumulator? = null
    private var currentLensContext: LensContext? = null
    private val openTracker = PendingOpenTracker()

    override fun whenOpensSettled(onSettled: () -> Unit) = openTracker.whenSettled(onSettled)

    private var activeDevice: CameraDevice? = null
    private var activeSession: CameraCaptureSession? = null
    private val activeReaders = mutableListOf<ImageReader>()
    private var activeTimeout: Runnable? = null

    // Incremented on every camera/session/step transition so late Camera2 callbacks are ignored cleanly.
    private var stepGeneration = 0

    private val finished: Boolean get() = currentHost?.isFinished ?: true
    private val stopping: Boolean get() = currentHost?.isStopping ?: false

    private fun checkStopBeforeStep(): Boolean = currentHost?.checkStopBeforeStep() ?: true

    private fun postProgress(@Suppress("UNUSED_PARAMETER") lens: ProbeLens, stepLabel: String) {
        currentHost?.postProgress(stepLabel)
    }

    private fun finishLensWith(outcome: ProbeOutcome, detail: String? = null) {
        currentHost?.onLensFinished(outcome, detail)
    }

    override val betweenLensesMs: Long = BETWEEN_LENSES_MS

    override fun hasCameraPermission(): Boolean =
        ContextCompat.checkSelfPermission(appContext, Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED

    override fun isStreamActive(): Boolean {
        val liveService = CameraStreamService.instance
        return liveService != null && liveService.state != StreamState.Idle && liveService.state != StreamState.Failed
    }

    override fun reportHeader(): String = CameraStreamService.reportHeader()
    override fun wallTimeMs(): Long = System.currentTimeMillis()
    override fun monoTimeMs(): Long = SystemClock.elapsedRealtime()
    override fun cacheFile(): File = CameraProbeCache.cacheFile(appContext)
    override fun logWarn(message: String, t: Throwable?) { Log.w(TAG, message, t) }

    override fun createWorker(onUnhandledError: (Throwable) -> Unit): ProbeWorker {
        val created = AndroidProbeWorker(onUnhandledError)
        androidWorker = created
        return created
    }

    override fun postMain(block: () -> Unit) {
        mainHandler.post(block)
    }

    override fun enumerateLenses(): List<ProbeLens> {
        val entries = CameraStreamService.enumerateCameras(manager)
        return entries.map { entry ->
            val maxPixels = entry.supportedSizes.maxOfOrNull { it.width.toLong() * it.height.toLong() } ?: 0L
            ProbeLens(
                id = entry.id,
                logicalId = entry.logicalId,
                label = entry.label,
                facing = entry.facing,
                focalMm = entry.focalMm,
                hasOis = entry.hasOis,
                lensKind = entry.lensKind,
                maxPixels = maxPixels,
            )
        }
    }

    override fun probeLens(lens: ProbeLens, acc: ProbeSessionAccumulator, host: ProbeLensHost) {
        currentHost = host
        accumulator = acc
        val chars = runCatching { manager.getCameraCharacteristics(lens.id) }.getOrElse { e ->
            finishLensWith(ProbeOutcome.ERROR, "getCameraCharacteristics failed: ${e.message}")
            return
        }
        val openId = lens.logicalId ?: lens.id
        val physId = lens.logicalId?.let { lens.id }
        val physicalRequestKeys = if (lens.logicalId != null && Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
            runCatching {
                manager.getCameraCharacteristics(lens.logicalId).availablePhysicalCameraRequestKeys?.toSet().orEmpty()
            }.getOrDefault(emptySet())
        } else {
            emptySet()
        }
        val map = runCatching { chars.get(CameraCharacteristics.SCALER_STREAM_CONFIGURATION_MAP) }.getOrNull()
        val capabilities = runCatching {
            chars.get(CameraCharacteristics.REQUEST_AVAILABLE_CAPABILITIES)?.toSet().orEmpty()
        }.getOrDefault(emptySet())

        val ctx = LensContext(
            lens = lens,
            chars = chars,
            openId = openId,
            physId = physId,
            physicalRequestKeys = physicalRequestKeys,
            map = map,
            capabilities = capabilities,
        )
        currentLensContext = ctx

        // Collect all static metadata before opening the camera so partial results already carry it on any failure.
        acc.setCharacteristics(readCharacteristics(chars, map, capabilities))
        acc.setDurations(readFormatDurations(map))
        acc.setVendorKeys(readVendorKeysAndExtensions(lens, chars, capabilities))

        if (lens.isPhysicalOnly && Build.VERSION.SDK_INT < Build.VERSION_CODES.P) {
            acc.setSessionChecks(SessionChecksReport(skippedReason = "physical-only lens requires Android 9"))
            finishLensWith(ProbeOutcome.SKIPPED, "physical-only lens requires Android 9")
            return
        }

        openCameraForLens(ctx)
    }

    override fun closeActive(abort: Boolean) {
        stepGeneration++
        closeActiveDevice(abort)
        currentLensContext?.device = null
        currentLensContext = null
    }

    private fun openCameraForLens(ctx: LensContext) {
        if (checkStopBeforeStep()) return
        postProgress(ctx.lens, "Opening camera")
        val myGen = ++stepGeneration
        val handler = cameraHandler ?: return

        val timeout = Runnable {
            if (myGen != stepGeneration || finished) return@Runnable
            finishLensWith(ProbeOutcome.TIMEOUT, "openCamera timed out after ${CAMERA_OPEN_TIMEOUT_MS} ms")
        }
        armTimeout(timeout, CAMERA_OPEN_TIMEOUT_MS)

        val openToken = openTracker.begin()
        try {
            @Suppress("MissingPermission")
            manager.openCamera(ctx.openId, object : CameraDevice.StateCallback() {
                override fun onOpened(camera: CameraDevice) {
                    if (myGen != stepGeneration || finished) {
                        openTracker.resolveAfter(openToken) { runCatching { camera.close() } }
                        return
                    }
                    openTracker.resolve(openToken)
                    cancelActiveTimeout()
                    activeDevice = camera
                    ctx.device = camera
                    if (checkStopBeforeStep()) return
                    runSessionChecksThenProcessing(ctx)
                }

                override fun onDisconnected(camera: CameraDevice) {
                    runCatching { camera.close() }
                    openTracker.resolve(openToken)
                    if (activeDevice === camera) activeDevice = null
                    if (ctx.device === camera) ctx.device = null
                    if (!ProbeOpenPolicy.disconnectIsFatal(finished, currentLensContext === ctx)) return
                    currentHost?.onFatalFailure("Camera disconnected while probing lens ${ctx.lens.id}")
                }

                override fun onError(camera: CameraDevice, error: Int) {
                    runCatching { camera.close() }
                    openTracker.resolve(openToken)
                    if (activeDevice === camera) activeDevice = null
                    if (ctx.device === camera) ctx.device = null
                    if (finished) return
                    if (myGen == stepGeneration || currentLensContext === ctx) {
                        finishLensWith(ProbeOutcome.ERROR, "Camera2 device error $error")
                    }
                }
            }, handler)
        } catch (e: Exception) {
            openTracker.resolve(openToken)
            cancelActiveTimeout()
            val detail = "${e.javaClass.simpleName}: ${e.message}"
            finishLensWith(ProbeOutcome.ERROR, "openCamera failed: $detail")
        }
    }

    private fun runSessionChecksThenProcessing(ctx: LensContext) {
        if (checkStopBeforeStep()) return
        postProgress(ctx.lens, "Checking session configurations")
        val device = ctx.device
        if (device != null) {
            accumulator?.setSessionChecks(runSessionChecks(ctx, device))
        }
        if (checkStopBeforeStep()) return
        runProcessingChecks(ctx)
    }

    // Checks whether Telescope's stream target fits alongside full-size YUV or RAW on Android 10+.
    private fun runSessionChecks(ctx: LensContext, device: CameraDevice): SessionChecksReport {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.Q) {
            return SessionChecksReport(skippedReason = "requires Android 10")
        }
        val map = ctx.map ?: return SessionChecksReport(skippedReason = "StreamConfigurationMap unavailable")
        val privSizes = outputSizesForClass(map, SurfaceTexture::class.java)
        val jpegSizes = outputSizesForFormat(map, ImageFormat.JPEG)
        val yuvSizes = outputSizesForFormat(map, ImageFormat.YUV_420_888)
        val hasRawCap = CameraCharacteristics.REQUEST_AVAILABLE_CAPABILITIES_RAW in ctx.capabilities
        val rawSizes = if (hasRawCap) outputSizesForFormat(map, ImageFormat.RAW_SENSOR) else emptyList()

        val privStreamSize = ProbePlan.pickStreamSize(privSizes)
        val jpegStreamSize = ProbePlan.pickStreamSize(jpegSizes)
        val maxYuv = ProbePlan.largestSize(yuvSizes)
        val maxRaw = ProbePlan.largestSize(rawSizes)

        val checks = mutableListOf<SessionCheckEntry>()
        checks += checkPrivPlusReader(device, ctx.physId, privStreamSize, "full YUV", ImageFormat.YUV_420_888, maxYuv, null)
        checks += checkPrivPlusReader(
            device, ctx.physId, privStreamSize, "full RAW", ImageFormat.RAW_SENSOR, maxRaw,
            if (!hasRawCap) "no RAW capability" else null,
        )
        checks += checkJpegPlusReader(device, ctx.physId, jpegStreamSize, "full YUV", ImageFormat.YUV_420_888, maxYuv, null)
        checks += checkJpegPlusReader(
            device, ctx.physId, jpegStreamSize, "full RAW", ImageFormat.RAW_SENSOR, maxRaw,
            if (!hasRawCap) "no RAW capability" else null,
        )
        return SessionChecksReport(checks = checks)
    }

    @androidx.annotation.RequiresApi(Build.VERSION_CODES.Q)
    private fun checkPrivPlusReader(
        device: CameraDevice,
        physId: String?,
        streamSize: ProbeSize?,
        extraName: String,
        extraFormat: Int,
        extraSize: ProbeSize?,
        unsupportedReason: String?,
    ): SessionCheckEntry {
        val label = "stream(PRIV ${streamSize ?: "none"}) + $extraName(${extraSize ?: "none"})"
        if (unsupportedReason != null) return SessionCheckEntry(label, ProbeOutcome.UNSUPPORTED, unsupportedReason)
        if (streamSize == null) return SessionCheckEntry(label, ProbeOutcome.SKIPPED, "no PRIV stream size")
        if (extraSize == null) return SessionCheckEntry(label, ProbeOutcome.SKIPPED, "$extraName size unavailable")

        var reader: ImageReader? = null
        return try {
            val privConfig = OutputConfiguration(Size(streamSize.width, streamSize.height), SurfaceTexture::class.java)
                .also { cfg -> physId?.let(cfg::setPhysicalCameraId) }
            reader = ImageReader.newInstance(extraSize.width, extraSize.height, extraFormat, 2)
            val readerConfig = OutputConfiguration(reader.surface)
                .also { cfg -> physId?.let(cfg::setPhysicalCameraId) }
            querySessionSupported(device, listOf(privConfig, readerConfig), label)
        } catch (t: Throwable) {
            SessionCheckEntry(label, ProbeOutcome.ERROR, "${t.javaClass.simpleName}: ${t.message}")
        } finally {
            runCatching { reader?.close() }
        }
    }

    @androidx.annotation.RequiresApi(Build.VERSION_CODES.Q)
    private fun checkJpegPlusReader(
        device: CameraDevice,
        physId: String?,
        streamSize: ProbeSize?,
        extraName: String,
        extraFormat: Int,
        extraSize: ProbeSize?,
        unsupportedReason: String?,
    ): SessionCheckEntry {
        val label = "stream(JPEG ${streamSize ?: "none"}) + $extraName(${extraSize ?: "none"})"
        if (unsupportedReason != null) return SessionCheckEntry(label, ProbeOutcome.UNSUPPORTED, unsupportedReason)
        if (streamSize == null) return SessionCheckEntry(label, ProbeOutcome.SKIPPED, "no JPEG stream size")
        if (extraSize == null) return SessionCheckEntry(label, ProbeOutcome.SKIPPED, "$extraName size unavailable")

        var streamReader: ImageReader? = null
        var extraReader: ImageReader? = null
        return try {
            streamReader = ImageReader.newInstance(streamSize.width, streamSize.height, ImageFormat.JPEG, 2)
            extraReader = ImageReader.newInstance(extraSize.width, extraSize.height, extraFormat, 2)
            val cfgs = listOf(streamReader.surface, extraReader.surface).map { surface ->
                OutputConfiguration(surface).also { cfg -> physId?.let(cfg::setPhysicalCameraId) }
            }
            querySessionSupported(device, cfgs, label)
        } catch (t: Throwable) {
            SessionCheckEntry(label, ProbeOutcome.ERROR, "${t.javaClass.simpleName}: ${t.message}")
        } finally {
            runCatching { extraReader?.close() }
            runCatching { streamReader?.close() }
        }
    }

    @androidx.annotation.RequiresApi(Build.VERSION_CODES.Q)
    private fun querySessionSupported(
        device: CameraDevice,
        cfgs: List<OutputConfiguration>,
        label: String,
    ): SessionCheckEntry {
        return try {
            val sessionConfig = SessionConfiguration(
                SessionConfiguration.SESSION_REGULAR,
                cfgs,
                Executor { it.run() },
                object : CameraCaptureSession.StateCallback() {
                    override fun onConfigured(session: CameraCaptureSession) {}
                    override fun onConfigureFailed(session: CameraCaptureSession) {}
                },
            )
            val supported = device.isSessionConfigurationSupported(sessionConfig)
            if (supported) {
                SessionCheckEntry(label, ProbeOutcome.OK, "supported")
            } else {
                SessionCheckEntry(label, ProbeOutcome.UNSUPPORTED, "not supported")
            }
        } catch (_: UnsupportedOperationException) {
            SessionCheckEntry(label, ProbeOutcome.SKIPPED, "isSessionConfigurationSupported not supported by HAL")
        } catch (e: Exception) {
            SessionCheckEntry(label, ProbeOutcome.ERROR, "${e.javaClass.simpleName}: ${e.message}")
        }
    }

    // Probing NR and edge modes: checks OFF and the least advertised mode on both JPEG and YUV captures.
    private fun runProcessingChecks(ctx: LensContext) {
        if (checkStopBeforeStep()) return
        postProgress(ctx.lens, "Checking noise reduction and edge modes")
        val advertisedNr = runCatching {
            ctx.chars.get(CameraCharacteristics.NOISE_REDUCTION_AVAILABLE_NOISE_REDUCTION_MODES)?.toList().orEmpty()
        }.getOrDefault(emptyList())
        val advertisedEdge = runCatching {
            ctx.chars.get(CameraCharacteristics.EDGE_AVAILABLE_EDGE_MODES)?.toList().orEmpty()
        }.getOrDefault(emptyList())
        val leastNr = ProbePlan.leastNrMode(advertisedNr)
        val leastEdge = ProbePlan.leastEdgeMode(advertisedEdge)

        val pairs = mutableListOf<ProcessingModePair>()
        if (advertisedNr.isNotEmpty() || advertisedEdge.isNotEmpty()) {
            val offNr = if (advertisedNr.isNotEmpty()) CaptureRequest.NOISE_REDUCTION_MODE_OFF else null
            val offEdge = if (advertisedEdge.isNotEmpty()) CaptureRequest.EDGE_MODE_OFF else null
            pairs += ProcessingModePair("OFF", offNr, offEdge)
            if (leastNr != offNr || leastEdge != offEdge) {
                pairs += ProcessingModePair("least", leastNr, leastEdge)
            }
        }

        val checks = mutableListOf<ProcessingCheckEntry>()
        // Store advertised modes upfront and refresh after each check so partial results survive mid-test cancellation.
        fun publishProcessing() {
            accumulator?.setProcessing(ProcessingReport(advertisedNr, advertisedEdge, leastNr, leastEdge, checks.toList()))
        }
        publishProcessing()
        if (pairs.isEmpty()) {
            runExposureSweep(ctx)
            return
        }

        val map = ctx.map
        val jpegSize = map?.let { ProbePlan.largestSize(outputSizesForFormat(it, ImageFormat.JPEG)) }
        val yuvSize = map?.let { ProbePlan.pickSmallYuvSize(outputSizesForFormat(it, ImageFormat.YUV_420_888)) }

        val formatPlan = listOf(
            Triple("JPEG", ImageFormat.JPEG, jpegSize),
            Triple("YUV_420_888", ImageFormat.YUV_420_888, yuvSize),
        )

        fun finishProcessing() {
            publishProcessing()
            runExposureSweep(ctx)
        }

        fun runFormatAt(formatIdx: Int) {
            if (checkStopBeforeStep()) return
            if (formatIdx >= formatPlan.size) {
                finishProcessing()
                return
            }
            val (formatName, imageFormat, size) = formatPlan[formatIdx]
            if (size == null) {
                for (pair in pairs) {
                    checks += ProcessingCheckEntry(
                        formatName = formatName,
                        size = null,
                        label = pair.label,
                        askedNr = pair.askedNr,
                        askedEdge = pair.askedEdge,
                        outcome = ProbeOutcome.SKIPPED,
                        detail = "format unavailable",
                    )
                }
                publishProcessing()
                runFormatAt(formatIdx + 1)
                return
            }
            val device = ctx.device ?: run {
                finishProcessing()
                return
            }
            val reader = try {
                ImageReader.newInstance(size.width, size.height, imageFormat, 2)
            } catch (t: Throwable) {
                for (pair in pairs) {
                    checks += ProcessingCheckEntry(
                        formatName = formatName,
                        size = size,
                        label = pair.label,
                        askedNr = pair.askedNr,
                        askedEdge = pair.askedEdge,
                        outcome = ProbeOutcome.ERROR,
                        detail = "ImageReader failed: ${t.javaClass.simpleName}: ${t.message}",
                    )
                }
                publishProcessing()
                runFormatAt(formatIdx + 1)
                return
            }
            activeReaders += reader

            openSession(
                device = device,
                surfaces = listOf(reader.surface),
                physId = ctx.physId,
                onConfigured = { session ->
                    fun runPairAt(pairIdx: Int) {
                        if (checkStopBeforeStep()) return
                        if (pairIdx >= pairs.size) {
                            closeActiveCapture()
                            runFormatAt(formatIdx + 1)
                            return
                        }
                        val pair = pairs[pairIdx]
                        captureSingleProcessingCheck(
                            ctx = ctx,
                            device = device,
                            session = session,
                            reader = reader,
                            formatName = formatName,
                            imageFormat = imageFormat,
                            size = size,
                            pair = pair,
                            advertisedNr = advertisedNr,
                            advertisedEdge = advertisedEdge,
                        ) { entry ->
                            checks += entry
                            publishProcessing()
                            runPairAt(pairIdx + 1)
                        }
                    }
                    runPairAt(0)
                },
                onFailed = { outcome, detail ->
                    closeActiveCapture()
                    for (pair in pairs) {
                        checks += ProcessingCheckEntry(
                            formatName = formatName,
                            size = size,
                            label = pair.label,
                            askedNr = pair.askedNr,
                            askedEdge = pair.askedEdge,
                            outcome = outcome,
                            detail = detail,
                        )
                    }
                    publishProcessing()
                    runFormatAt(formatIdx + 1)
                },
            )
        }

        runFormatAt(0)
    }

    private fun captureSingleProcessingCheck(
        ctx: LensContext,
        device: CameraDevice,
        session: CameraCaptureSession,
        reader: ImageReader,
        formatName: String,
        imageFormat: Int,
        size: ProbeSize,
        pair: ProcessingModePair,
        advertisedNr: List<Int>,
        advertisedEdge: List<Int>,
        onDone: (ProcessingCheckEntry) -> Unit,
    ) {
        val myGen = ++stepGeneration
        val handler = cameraHandler ?: return
        val tag = "proc:$myGen"
        val matcher = TimestampMatcher<Pair<Int?, Int?>>(maxPending = 8)
        var completed = false

        val unadvertisedNote = buildList {
            if (pair.askedNr != null && pair.askedNr !in advertisedNr) add("NR not advertised")
            if (pair.askedEdge != null && pair.askedEdge !in advertisedEdge) add("Edge not advertised")
        }.joinToString(", ").takeIf { it.isNotEmpty() }

        fun finishPair(outcome: ProbeOutcome, echoedNr: Int? = null, echoedEdge: Int? = null, detail: String? = null) {
            if (completed || myGen != stepGeneration || finished) return
            completed = true
            cancelActiveTimeout()
            reader.setOnImageAvailableListener(null, null)
            drainReader(reader)
            val combinedDetail = listOfNotNull(unadvertisedNote, detail).joinToString("; ").takeIf { it.isNotEmpty() }
            onDone(
                ProcessingCheckEntry(
                    formatName = formatName,
                    size = size,
                    label = pair.label,
                    askedNr = pair.askedNr,
                    askedEdge = pair.askedEdge,
                    outcome = outcome,
                    echoedNr = echoedNr,
                    echoedEdge = echoedEdge,
                    detail = combinedDetail,
                ),
            )
        }

        fun checkMatched() {
            val first = matcher.matched.firstOrNull() ?: return
            finishPair(ProbeOutcome.OK, echoedNr = first.payload.first, echoedEdge = first.payload.second)
        }

        drainReader(reader)
        reader.setOnImageAvailableListener({ r ->
            if (myGen != stepGeneration || finished) {
                drainReader(r)
                return@setOnImageAvailableListener
            }
            while (true) {
                val image = try {
                    r.acquireNextImage()
                } catch (_: Exception) {
                    null
                } ?: break
                val ts = try {
                    image.timestamp
                } finally {
                    runCatching { image.close() }
                }
                if (matcher.onImage(ts) != null) checkMatched()
            }
        }, handler)

        val timeout = Runnable {
            val detail = "timed out after ${SINGLE_CAPTURE_TIMEOUT_MS} ms (img=${matcher.imagesSeen}, res=${matcher.resultsSeen})"
            finishPair(ProbeOutcome.TIMEOUT, detail = detail)
        }
        armTimeout(timeout, SINGLE_CAPTURE_TIMEOUT_MS)

        try {
            val template = if (imageFormat == ImageFormat.JPEG) {
                CameraDevice.TEMPLATE_STILL_CAPTURE
            } else {
                CameraDevice.TEMPLATE_PREVIEW
            }
            val builder = createRequestBuilder(device, template, ctx.physId, ctx.physicalRequestKeys).apply {
                addTarget(reader.surface)
                setTag(tag)
                setKey(this, CaptureRequest.CONTROL_MODE, CaptureRequest.CONTROL_MODE_AUTO, ctx.physId, ctx.physicalRequestKeys)
                setKey(this, CaptureRequest.CONTROL_AE_MODE, CaptureRequest.CONTROL_AE_MODE_ON, ctx.physId, ctx.physicalRequestKeys)
                if (imageFormat == ImageFormat.JPEG) {
                    setKey(this, CaptureRequest.JPEG_QUALITY, 95.toByte(), ctx.physId, ctx.physicalRequestKeys)
                }
                pair.askedNr?.let {
                    setKey(this, CaptureRequest.NOISE_REDUCTION_MODE, it, ctx.physId, ctx.physicalRequestKeys)
                }
                pair.askedEdge?.let {
                    setKey(this, CaptureRequest.EDGE_MODE, it, ctx.physId, ctx.physicalRequestKeys)
                }
            }
            session.capture(
                builder.build(),
                object : CameraCaptureSession.CaptureCallback() {
                    override fun onCaptureCompleted(
                        s: CameraCaptureSession,
                        request: CaptureRequest,
                        result: TotalCaptureResult,
                    ) {
                        if (myGen != stepGeneration || finished || request.tag != tag) return
                        val ts = readResultKey(result, CaptureResult.SENSOR_TIMESTAMP, ctx.physId)
                            ?: result.get(CaptureResult.SENSOR_TIMESTAMP)
                        val echoedNr = readResultKey(result, CaptureResult.NOISE_REDUCTION_MODE, ctx.physId)
                        val echoedEdge = readResultKey(result, CaptureResult.EDGE_MODE, ctx.physId)
                        if (ts == null) {
                            finishPair(ProbeOutcome.ERROR, echoedNr, echoedEdge, "missing SENSOR_TIMESTAMP")
                            return
                        }
                        if (matcher.onResult(ts, echoedNr to echoedEdge) != null) checkMatched()
                    }

                    override fun onCaptureFailed(
                        s: CameraCaptureSession,
                        request: CaptureRequest,
                        failure: CaptureFailure,
                    ) {
                        if (myGen != stepGeneration || finished || request.tag != tag) return
                        if (failure.reason == CaptureFailure.REASON_FLUSHED && stopping) return
                        finishPair(
                            ProbeOutcome.CAPTURE_FAILURE,
                            detail = "capture failed (reason=${failure.reason}, imageCaptured=${failure.wasImageCaptured()})",
                        )
                    }
                },
                handler,
            )
        } catch (e: Exception) {
            finishPair(ProbeOutcome.CAPTURE_FAILURE, detail = "${e.javaClass.simpleName}: ${e.message}")
        }
    }

    // Repeating manual exposure sweep on a small YUV output from 1/30 s up to 30 s capped at sensor and frame-duration limits.
    private fun runExposureSweep(ctx: LensContext) {
        if (checkStopBeforeStep()) return
        val acc = accumulator ?: return
        if (CameraCharacteristics.REQUEST_AVAILABLE_CAPABILITIES_MANUAL_SENSOR !in ctx.capabilities) {
            acc.finishExposureSweep(ProbeOutcome.UNSUPPORTED, null, detail = "no MANUAL_SENSOR capability")
            runBurstFormats(ctx)
            return
        }
        val expRange = runCatching { ctx.chars.get(CameraCharacteristics.SENSOR_INFO_EXPOSURE_TIME_RANGE) }.getOrNull()
        val isoRange = runCatching { ctx.chars.get(CameraCharacteristics.SENSOR_INFO_SENSITIVITY_RANGE) }.getOrNull()
        if (expRange == null || isoRange == null) {
            acc.finishExposureSweep(ProbeOutcome.UNSUPPORTED, null, detail = "missing exposure or ISO range")
            runBurstFormats(ctx)
            return
        }
        val map = ctx.map
        if (map == null) {
            acc.finishExposureSweep(ProbeOutcome.SKIPPED, null, detail = "StreamConfigurationMap unavailable")
            runBurstFormats(ctx)
            return
        }
        val smallYuv = ProbePlan.pickSmallYuvSize(outputSizesForFormat(map, ImageFormat.YUV_420_888))
        if (smallYuv == null) {
            acc.finishExposureSweep(ProbeOutcome.SKIPPED, null, detail = "no YUV_420_888 output size")
            runBurstFormats(ctx)
            return
        }
        val maxFrameDurationNs = runCatching { ctx.chars.get(CameraCharacteristics.SENSOR_INFO_MAX_FRAME_DURATION) }.getOrNull()
        val minFrameDurationNs = runCatching {
            map.getOutputMinFrameDuration(ImageFormat.YUV_420_888, Size(smallYuv.width, smallYuv.height))
        }.getOrDefault(0L)
        val steps = ProbePlan.exposureSteps(
            minExposureNs = expRange.lower,
            maxExposureNs = expRange.upper,
            maxFrameDurationNs = maxFrameDurationNs,
            minFrameDurationNs = minFrameDurationNs,
        )
        if (steps.isEmpty()) {
            acc.finishExposureSweep(ProbeOutcome.SKIPPED, smallYuv, detail = "no exposure steps within sensor limits")
            runBurstFormats(ctx)
            return
        }
        val iso = 100.coerceIn(isoRange.lower, isoRange.upper)
        val device = ctx.device ?: run {
            acc.finishExposureSweep(ProbeOutcome.ERROR, smallYuv, detail = "camera device closed")
            runBurstFormats(ctx)
            return
        }

        acc.beginExposureSweep(smallYuv)
        postProgress(ctx.lens, "Opening repeating exposure session ($smallYuv)")
        val reader = try {
            ImageReader.newInstance(smallYuv.width, smallYuv.height, ImageFormat.YUV_420_888, ProbePlan.BURST_MAX_IMAGES)
        } catch (t: Throwable) {
            acc.finishExposureSweep(ProbeOutcome.ERROR, smallYuv, detail = "ImageReader failed: ${t.javaClass.simpleName}: ${t.message}")
            runBurstFormats(ctx)
            return
        }
        activeReaders += reader

        openSession(
            device = device,
            surfaces = listOf(reader.surface),
            physId = ctx.physId,
            onConfigured = { session ->
                fun runStepAt(stepIdx: Int) {
                    if (checkStopBeforeStep()) return
                    if (stepIdx >= steps.size) {
                        closeActiveCapture()
                        acc.finishExposureSweep(ProbeOutcome.OK, smallYuv)
                        runBurstFormats(ctx)
                        return
                    }
                    val step = steps[stepIdx]
                    postProgress(
                        ctx.lens,
                        "Repeating exposure ${CameraProbeReport.formatNs(step.askedExposureNs)} (${stepIdx + 1}/${steps.size})",
                    )
                    runSingleExposureStep(ctx, device, session, reader, step, iso) { result ->
                        acc.addExposureStep(result)
                        if (checkStopBeforeStep()) return@runSingleExposureStep
                        when (result.outcome) {
                            ProbeOutcome.OK -> runStepAt(stepIdx + 1)
                            ProbeOutcome.CLAMPED -> {
                                closeActiveCapture(abort = true)
                                acc.finishExposureSweep(
                                    ProbeOutcome.CLAMPED,
                                    smallYuv,
                                    stopReason = "clamped at ${CameraProbeReport.formatNs(step.askedExposureNs)}",
                                )
                                runBurstFormats(ctx)
                            }
                            ProbeOutcome.TIMEOUT -> {
                                closeActiveCapture(abort = true)
                                acc.finishExposureSweep(
                                    ProbeOutcome.TIMEOUT,
                                    smallYuv,
                                    stopReason = "timed out at ${CameraProbeReport.formatNs(step.askedExposureNs)}",
                                )
                                runBurstFormats(ctx)
                            }
                            ProbeOutcome.CAPTURE_FAILURE -> {
                                closeActiveCapture(abort = true)
                                acc.finishExposureSweep(
                                    ProbeOutcome.CAPTURE_FAILURE,
                                    smallYuv,
                                    stopReason = "capture failure at ${CameraProbeReport.formatNs(step.askedExposureNs)}",
                                )
                                runBurstFormats(ctx)
                            }
                            else -> {
                                closeActiveCapture(abort = true)
                                acc.finishExposureSweep(result.outcome, smallYuv, stopReason = result.detail)
                                runBurstFormats(ctx)
                            }
                        }
                    }
                }
                runStepAt(0)
            },
            onFailed = { outcome, detail ->
                closeActiveCapture()
                acc.finishExposureSweep(outcome, smallYuv, detail = detail)
                runBurstFormats(ctx)
            },
        )
    }

    private fun runSingleExposureStep(
        ctx: LensContext,
        device: CameraDevice,
        session: CameraCaptureSession,
        reader: ImageReader,
        step: ExposureStepPlan,
        iso: Int,
        onDone: (ExposureStepResult) -> Unit,
    ) {
        val myGen = ++stepGeneration
        val handler = cameraHandler ?: return
        val tag = "exp:$myGen"
        val matcher = TimestampMatcher<Pair<Long?, Long?>>(maxPending = 32)
        var completed = false

        fun finishStep(outcome: ProbeOutcome, detail: String? = null) {
            if (completed || myGen != stepGeneration || finished) return
            completed = true
            cancelActiveTimeout()
            reader.setOnImageAvailableListener(null, null)
            drainReader(reader)
            val echoedExp = matcher.matched.mapNotNull { it.payload.first }
            val echoedFrame = matcher.matched.mapNotNull { it.payload.second }
            val gaps = matcher.timestampGapsNs()
            onDone(
                ExposureStepResult(
                    askedExposureNs = step.askedExposureNs,
                    askedFrameDurationNs = step.askedFrameDurationNs,
                    iso = iso,
                    outcome = outcome,
                    matchedFrames = matcher.matched.size,
                    imagesSeen = matcher.imagesSeen,
                    resultsSeen = matcher.resultsSeen,
                    echoedExposuresNs = echoedExp,
                    echoedFrameDurationsNs = echoedFrame,
                    timestampGapsNs = gaps,
                    detail = detail,
                ),
            )
        }

        val watchdog = Runnable {
            val detail = "no matched frame within ${step.timeoutMs} ms"
            finishStep(ProbeOutcome.TIMEOUT, detail)
        }

        fun onFrameMatched() {
            if (completed || myGen != stepGeneration || finished) return
            if (matcher.matched.size >= ProbePlan.EXPOSURE_TARGET_FRAMES) {
                val echoedExp = matcher.matched.mapNotNull { it.payload.first }
                val gaps = matcher.timestampGapsNs()
                val clamped = ProbePlan.isExposureClamped(
                    askedExposureNs = step.askedExposureNs,
                    echoedExposureNs = echoedExp.lastOrNull(),
                    observedGapNs = gaps.lastOrNull(),
                )
                finishStep(if (clamped) ProbeOutcome.CLAMPED else ProbeOutcome.OK)
            } else {
                // Reset the per-frame deadline after each matched frame of this step arrives.
                armTimeout(watchdog, step.timeoutMs)
            }
        }

        drainReader(reader)
        reader.setOnImageAvailableListener({ r ->
            if (myGen != stepGeneration || finished) {
                drainReader(r)
                return@setOnImageAvailableListener
            }
            while (true) {
                val image = try {
                    r.acquireNextImage()
                } catch (_: Exception) {
                    null
                } ?: break
                val ts = try {
                    image.timestamp
                } finally {
                    runCatching { image.close() }
                }
                if (matcher.onImage(ts) != null) onFrameMatched()
            }
        }, handler)

        armTimeout(watchdog, step.timeoutMs)

        try {
            val builder = createRequestBuilder(device, CameraDevice.TEMPLATE_PREVIEW, ctx.physId, ctx.physicalRequestKeys).apply {
                addTarget(reader.surface)
                setTag(tag)
                setKey(this, CaptureRequest.CONTROL_MODE, CaptureRequest.CONTROL_MODE_AUTO, ctx.physId, ctx.physicalRequestKeys)
                setKey(this, CaptureRequest.CONTROL_AE_MODE, CaptureRequest.CONTROL_AE_MODE_OFF, ctx.physId, ctx.physicalRequestKeys)
                setKey(this, CaptureRequest.CONTROL_AF_MODE, CaptureRequest.CONTROL_AF_MODE_OFF, ctx.physId, ctx.physicalRequestKeys)
                setKey(
                    this,
                    CaptureRequest.CONTROL_VIDEO_STABILIZATION_MODE,
                    CaptureRequest.CONTROL_VIDEO_STABILIZATION_MODE_OFF,
                    ctx.physId,
                    ctx.physicalRequestKeys,
                )
                setKey(
                    this,
                    CaptureRequest.LENS_OPTICAL_STABILIZATION_MODE,
                    CaptureRequest.LENS_OPTICAL_STABILIZATION_MODE_OFF,
                    ctx.physId,
                    ctx.physicalRequestKeys,
                )
                setKey(this, CaptureRequest.SENSOR_SENSITIVITY, iso, ctx.physId, ctx.physicalRequestKeys)
                setKey(this, CaptureRequest.SENSOR_EXPOSURE_TIME, step.askedExposureNs, ctx.physId, ctx.physicalRequestKeys)
                setKey(this, CaptureRequest.SENSOR_FRAME_DURATION, step.askedFrameDurationNs, ctx.physId, ctx.physicalRequestKeys)
            }
            session.setRepeatingRequest(
                builder.build(),
                object : CameraCaptureSession.CaptureCallback() {
                    override fun onCaptureCompleted(
                        s: CameraCaptureSession,
                        request: CaptureRequest,
                        result: TotalCaptureResult,
                    ) {
                        if (myGen != stepGeneration || finished || request.tag != tag) return
                        val ts = readResultKey(result, CaptureResult.SENSOR_TIMESTAMP, ctx.physId)
                            ?: result.get(CaptureResult.SENSOR_TIMESTAMP)
                            ?: return
                        val echoedExp = readResultKey(result, CaptureResult.SENSOR_EXPOSURE_TIME, ctx.physId)
                        val echoedFrame = readResultKey(result, CaptureResult.SENSOR_FRAME_DURATION, ctx.physId)
                        if (matcher.onResult(ts, echoedExp to echoedFrame) != null) onFrameMatched()
                    }

                    override fun onCaptureFailed(
                        s: CameraCaptureSession,
                        request: CaptureRequest,
                        failure: CaptureFailure,
                    ) {
                        if (myGen != stepGeneration || finished || request.tag != tag) return
                        if (failure.reason == CaptureFailure.REASON_FLUSHED && stopping) return
                        finishStep(
                            ProbeOutcome.CAPTURE_FAILURE,
                            "capture failed (reason=${failure.reason}, imageCaptured=${failure.wasImageCaptured()})",
                        )
                    }
                },
                handler,
            )
        } catch (e: Exception) {
            finishStep(ProbeOutcome.CAPTURE_FAILURE, "${e.javaClass.simpleName}: ${e.message}")
        }
    }

    // Measures full-size repeating burst rates over ~3 s for YUV_420_888, JPEG and RAW_SENSOR with 4 images in flight.
    private fun runBurstFormats(ctx: LensContext) {
        val specs = listOf(
            BurstSpec("YUV_420_888", ImageFormat.YUV_420_888, requiresRawCapability = false),
            BurstSpec("JPEG", ImageFormat.JPEG, requiresRawCapability = false),
            BurstSpec("RAW_SENSOR", ImageFormat.RAW_SENSOR, requiresRawCapability = true),
        )

        fun runSpecAt(idx: Int) {
            if (checkStopBeforeStep()) return
            if (idx >= specs.size) {
                finishCurrentLens()
                return
            }
            val spec = specs[idx]
            if (spec.requiresRawCapability && CameraCharacteristics.REQUEST_AVAILABLE_CAPABILITIES_RAW !in ctx.capabilities) {
                accumulator?.addBurst(
                    BurstFormatResult(
                        formatName = spec.formatName,
                        size = null,
                        outcome = ProbeOutcome.UNSUPPORTED,
                        elapsedMs = 0L,
                        imagesSeen = 0,
                        resultsSeen = 0,
                        matchedFrames = 0,
                        bufferLost = 0,
                        captureFailed = 0,
                        unmatchedResults = 0,
                        sensorFps = null,
                        wallFps = null,
                        detail = "no RAW capability",
                    ),
                )
                runSpecAt(idx + 1)
                return
            }
            val map = ctx.map
            val size = map?.let { ProbePlan.largestSize(outputSizesForFormat(it, spec.imageFormat)) }
            if (size == null) {
                accumulator?.addBurst(
                    BurstFormatResult(
                        formatName = spec.formatName,
                        size = null,
                        outcome = ProbeOutcome.SKIPPED,
                        elapsedMs = 0L,
                        imagesSeen = 0,
                        resultsSeen = 0,
                        matchedFrames = 0,
                        bufferLost = 0,
                        captureFailed = 0,
                        unmatchedResults = 0,
                        sensorFps = null,
                        wallFps = null,
                        detail = "format unavailable",
                    ),
                )
                runSpecAt(idx + 1)
                return
            }
            val device = ctx.device ?: run {
                finishCurrentLens()
                return
            }
            postProgress(ctx.lens, "Full-size ${spec.formatName} burst ($size)")
            runSingleBurst(ctx, device, spec, size) { result ->
                accumulator?.addBurst(result)
                runSpecAt(idx + 1)
            }
        }

        runSpecAt(0)
    }

    private fun runSingleBurst(
        ctx: LensContext,
        device: CameraDevice,
        spec: BurstSpec,
        size: ProbeSize,
        onDone: (BurstFormatResult) -> Unit,
    ) {
        val reader = try {
            ImageReader.newInstance(size.width, size.height, spec.imageFormat, ProbePlan.BURST_MAX_IMAGES)
        } catch (t: Throwable) {
            onDone(
                BurstFormatResult(
                    formatName = spec.formatName,
                    size = size,
                    outcome = ProbeOutcome.ERROR,
                    elapsedMs = 0L,
                    imagesSeen = 0,
                    resultsSeen = 0,
                    matchedFrames = 0,
                    bufferLost = 0,
                    captureFailed = 0,
                    unmatchedResults = 0,
                    sensorFps = null,
                    wallFps = null,
                    detail = "ImageReader failed: ${t.javaClass.simpleName}: ${t.message}",
                ),
            )
            return
        }
        activeReaders += reader

        openSession(
            device = device,
            surfaces = listOf(reader.surface),
            physId = ctx.physId,
            onConfigured = { session ->
                val myGen = ++stepGeneration
                val handler = cameraHandler ?: return@openSession
                val tag = "burst:$myGen"
                val matcher = TimestampMatcher<Unit>(maxPending = 128)
                var bufferLost = 0
                var captureFailed = 0
                var lastFailureDetail: String? = null
                var stoppingBurst = false
                var completed = false
                val startMs = SystemClock.elapsedRealtime()
                var burstEndMs = 0L

                fun finishBurst(overrideOutcome: ProbeOutcome? = null, detail: String? = null) {
                    if (completed || myGen != stepGeneration || finished) return
                    completed = true
                    cancelActiveTimeout()
                    val endMs = if (burstEndMs > startMs) burstEndMs else SystemClock.elapsedRealtime()
                    val elapsedMs = (endMs - startMs).coerceAtLeast(1L)
                    closeActiveCapture(abort = overrideOutcome == ProbeOutcome.TIMEOUT)
                    val outcome = overrideOutcome ?: when {
                        matcher.matched.isEmpty() && matcher.imagesSeen == 0 && captureFailed > 0 -> ProbeOutcome.CAPTURE_FAILURE
                        matcher.matched.isEmpty() && matcher.imagesSeen == 0 -> ProbeOutcome.TIMEOUT
                        else -> ProbeOutcome.OK
                    }
                    val wallFps = if (matcher.imagesSeen > 0) matcher.imagesSeen * 1000.0 / elapsedMs else null
                    val finalDetail = detail ?: lastFailureDetail
                    onDone(
                        BurstFormatResult(
                            formatName = spec.formatName,
                            size = size,
                            outcome = outcome,
                            elapsedMs = elapsedMs,
                            imagesSeen = matcher.imagesSeen,
                            resultsSeen = matcher.resultsSeen,
                            matchedFrames = matcher.matched.size,
                            bufferLost = bufferLost,
                            captureFailed = captureFailed,
                            unmatchedResults = matcher.unmatchedResultsCount,
                            sensorFps = matcher.sensorFps(),
                            wallFps = wallFps,
                            detail = finalDetail,
                        ),
                    )
                }

                drainReader(reader)
                reader.setOnImageAvailableListener({ r ->
                    if (myGen != stepGeneration || finished) {
                        drainReader(r)
                        return@setOnImageAvailableListener
                    }
                    while (true) {
                        val image = try {
                            r.acquireNextImage()
                        } catch (_: Exception) {
                            null
                        } ?: break
                        val ts = try {
                            image.timestamp
                        } finally {
                            runCatching { image.close() }
                        }
                        matcher.onImage(ts)
                    }
                }, handler)

                val stopTimer = Runnable {
                    if (myGen != stepGeneration || finished || completed) return@Runnable
                    stoppingBurst = true
                    burstEndMs = SystemClock.elapsedRealtime()
                    runCatching { session.stopRepeating() }
                    val drainTimer = Runnable { finishBurst() }
                    armTimeout(drainTimer, BURST_DRAIN_MS)
                }
                armTimeout(stopTimer, ProbePlan.BURST_DURATION_MS)

                try {
                    val afModes = runCatching {
                        ctx.chars.get(CameraCharacteristics.CONTROL_AF_AVAILABLE_MODES)?.toSet().orEmpty()
                    }.getOrDefault(emptySet())
                    val builder = createRequestBuilder(device, CameraDevice.TEMPLATE_PREVIEW, ctx.physId, ctx.physicalRequestKeys).apply {
                        addTarget(reader.surface)
                        setTag(tag)
                        setKey(this, CaptureRequest.CONTROL_MODE, CaptureRequest.CONTROL_MODE_AUTO, ctx.physId, ctx.physicalRequestKeys)
                        setKey(this, CaptureRequest.CONTROL_AE_MODE, CaptureRequest.CONTROL_AE_MODE_ON, ctx.physId, ctx.physicalRequestKeys)
                        setKey(
                            this,
                            CaptureRequest.CONTROL_AF_MODE,
                            CameraRequestSelection.pickAfMode(afModes, wantContinuousVideo = false),
                            ctx.physId,
                            ctx.physicalRequestKeys,
                        )
                        if (spec.imageFormat == ImageFormat.JPEG) {
                            setKey(this, CaptureRequest.JPEG_QUALITY, 95.toByte(), ctx.physId, ctx.physicalRequestKeys)
                        }
                    }
                    session.setRepeatingRequest(
                        builder.build(),
                        object : CameraCaptureSession.CaptureCallback() {
                            override fun onCaptureCompleted(
                                s: CameraCaptureSession,
                                request: CaptureRequest,
                                result: TotalCaptureResult,
                            ) {
                                if (myGen != stepGeneration || finished || request.tag != tag) return
                                val ts = readResultKey(result, CaptureResult.SENSOR_TIMESTAMP, ctx.physId)
                                    ?: result.get(CaptureResult.SENSOR_TIMESTAMP)
                                    ?: return
                                matcher.onResult(ts, Unit)
                            }

                            override fun onCaptureBufferLost(
                                s: CameraCaptureSession,
                                request: CaptureRequest,
                                target: Surface,
                                frameNumber: Long,
                            ) {
                                if (myGen != stepGeneration || finished || request.tag != tag) return
                                bufferLost++
                            }

                            override fun onCaptureFailed(
                                s: CameraCaptureSession,
                                request: CaptureRequest,
                                failure: CaptureFailure,
                            ) {
                                if (myGen != stepGeneration || finished || request.tag != tag) return
                                if (stoppingBurst && failure.reason == CaptureFailure.REASON_FLUSHED) return
                                captureFailed++
                                lastFailureDetail = "reason=${failure.reason}, imageCaptured=${failure.wasImageCaptured()}"
                            }
                        },
                        handler,
                    )
                } catch (e: Exception) {
                    finishBurst(ProbeOutcome.CAPTURE_FAILURE, "${e.javaClass.simpleName}: ${e.message}")
                }
            },
            onFailed = { outcome, detail ->
                closeActiveCapture()
                onDone(
                    BurstFormatResult(
                        formatName = spec.formatName,
                        size = size,
                        outcome = outcome,
                        elapsedMs = 0L,
                        imagesSeen = 0,
                        resultsSeen = 0,
                        matchedFrames = 0,
                        bufferLost = 0,
                        captureFailed = 0,
                        unmatchedResults = 0,
                        sensorFps = null,
                        wallFps = null,
                        detail = detail,
                    ),
                )
            },
        )
    }

    private fun finishCurrentLens() {
        finishLensWith(ProbeOutcome.OK, null)
    }

    private fun openSession(
        device: CameraDevice,
        surfaces: List<Surface>,
        physId: String?,
        onConfigured: (CameraCaptureSession) -> Unit,
        onFailed: (ProbeOutcome, String) -> Unit,
    ) {
        val myGen = ++stepGeneration
        val handler = cameraHandler ?: return
        var resolved = false

        val timeout = Runnable {
            if (resolved || myGen != stepGeneration || finished) return@Runnable
            resolved = true
            onFailed(ProbeOutcome.TIMEOUT, "capture session timed out after ${SESSION_OPEN_TIMEOUT_MS} ms")
        }
        armTimeout(timeout, SESSION_OPEN_TIMEOUT_MS)

        val callback = object : CameraCaptureSession.StateCallback() {
            override fun onConfigured(session: CameraCaptureSession) {
                if (resolved || myGen != stepGeneration || finished) {
                    runCatching { session.close() }
                    return
                }
                resolved = true
                cancelActiveTimeout()
                activeSession = session
                onConfigured(session)
            }

            override fun onConfigureFailed(session: CameraCaptureSession) {
                runCatching { session.close() }
                if (resolved || myGen != stepGeneration || finished) return
                resolved = true
                cancelActiveTimeout()
                onFailed(ProbeOutcome.UNSUPPORTED, "session configure failed")
            }
        }

        try {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
                val cfgs = surfaces.map { surface ->
                    OutputConfiguration(surface).also { cfg -> physId?.let(cfg::setPhysicalCameraId) }
                }
                val exec = Executor { cmd -> handler.post(cmd) }
                device.createCaptureSession(
                    SessionConfiguration(SessionConfiguration.SESSION_REGULAR, cfgs, exec, callback),
                )
            } else if (physId != null) {
                resolved = true
                cancelActiveTimeout()
                onFailed(ProbeOutcome.SKIPPED, "physical-only lens requires Android 9")
            } else {
                @Suppress("DEPRECATION")
                device.createCaptureSession(surfaces, callback, handler)
            }
        } catch (e: Exception) {
            if (!resolved) {
                resolved = true
                cancelActiveTimeout()
                onFailed(ProbeOutcome.ERROR, "createCaptureSession failed: ${e.javaClass.simpleName}: ${e.message}")
            }
        }
    }

    private fun createRequestBuilder(
        device: CameraDevice,
        template: Int,
        physId: String?,
        physicalRequestKeys: Set<CaptureRequest.Key<*>>,
    ): CaptureRequest.Builder {
        if (physId != null && Build.VERSION.SDK_INT >= Build.VERSION_CODES.P && physicalRequestKeys.isNotEmpty()) {
            return runCatching { device.createCaptureRequest(template, setOf(physId)) }
                .getOrElse { device.createCaptureRequest(template) }
        }
        return device.createCaptureRequest(template)
    }

    private fun <T> setKey(
        builder: CaptureRequest.Builder,
        key: CaptureRequest.Key<T>,
        value: T,
        physId: String?,
        physicalRequestKeys: Set<CaptureRequest.Key<*>>,
    ) {
        builder.set(key, value)
        if (physId != null && Build.VERSION.SDK_INT >= Build.VERSION_CODES.P && key in physicalRequestKeys) {
            runCatching { builder.setPhysicalCameraKey(key, value, physId) }
        }
    }

    private fun <T> readResultKey(
        result: TotalCaptureResult,
        key: CaptureResult.Key<T>,
        physId: String?,
    ): T? {
        if (physId != null) {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
                val phys = runCatching { result.physicalCameraTotalResults[physId]?.get(key) }.getOrNull()
                if (phys != null) return phys
            } else if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
                @Suppress("DEPRECATION")
                val phys = runCatching { result.physicalCameraResults[physId]?.get(key) }.getOrNull()
                if (phys != null) return phys
            }
        }
        return result.get(key)
    }

    private fun readCharacteristics(
        chars: CameraCharacteristics,
        map: StreamConfigurationMap?,
        capabilities: Set<Int>,
    ): CharacteristicsReport {
        val hwLevel = when (chars.get(CameraCharacteristics.INFO_SUPPORTED_HARDWARE_LEVEL)) {
            CameraCharacteristics.INFO_SUPPORTED_HARDWARE_LEVEL_LEGACY -> "LEGACY"
            CameraCharacteristics.INFO_SUPPORTED_HARDWARE_LEVEL_LIMITED -> "LIMITED"
            CameraCharacteristics.INFO_SUPPORTED_HARDWARE_LEVEL_FULL -> "FULL"
            CameraCharacteristics.INFO_SUPPORTED_HARDWARE_LEVEL_3 -> "LEVEL_3"
            CameraCharacteristics.INFO_SUPPORTED_HARDWARE_LEVEL_EXTERNAL -> "EXTERNAL"
            else -> "UNKNOWN"
        }
        val expRange = runCatching { chars.get(CameraCharacteristics.SENSOR_INFO_EXPOSURE_TIME_RANGE) }.getOrNull()
        val maxFrameDurationNs = runCatching { chars.get(CameraCharacteristics.SENSOR_INFO_MAX_FRAME_DURATION) }.getOrNull()
        val effectiveExposureMax = expRange?.upper?.let { upper ->
            val cap = maxFrameDurationNs?.takeIf { it > 0L } ?: Long.MAX_VALUE
            minOf(upper, cap)
        }
        val isoRange = runCatching { chars.get(CameraCharacteristics.SENSOR_INFO_SENSITIVITY_RANGE) }.getOrNull()
        val maxAnalogIso = runCatching { chars.get(CameraCharacteristics.SENSOR_MAX_ANALOG_SENSITIVITY) }.getOrNull()
        val boostRange = runCatching { chars.get(CameraCharacteristics.CONTROL_POST_RAW_SENSITIVITY_BOOST_RANGE) }.getOrNull()
        val fpsRanges = runCatching {
            chars.get(CameraCharacteristics.CONTROL_AE_AVAILABLE_TARGET_FPS_RANGES)?.map { "${it.lower}-${it.upper}" }.orEmpty()
        }.getOrDefault(emptyList())
        val highSpeed = if (map != null &&
            CameraCharacteristics.REQUEST_AVAILABLE_CAPABILITIES_CONSTRAINED_HIGH_SPEED_VIDEO in capabilities
        ) {
            runCatching {
                map.highSpeedVideoSizes.orEmpty()
                    .sortedByDescending { it.width.toLong() * it.height.toLong() }
                    .groupBy { size ->
                        map.getHighSpeedVideoFpsRangesFor(size).orEmpty().joinToString(" ") { "${it.lower}-${it.upper}" }
                    }
                    .map { (rates, sizes) -> "${sizes.joinToString { "${it.width}x${it.height}" }} at $rates" }
            }.getOrDefault(emptyList())
        } else {
            emptyList()
        }
        val timestampSource = when (runCatching { chars.get(CameraCharacteristics.SENSOR_INFO_TIMESTAMP_SOURCE) }.getOrNull()) {
            CameraMetadata.SENSOR_INFO_TIMESTAMP_SOURCE_UNKNOWN -> "UNKNOWN"
            CameraMetadata.SENSOR_INFO_TIMESTAMP_SOURCE_REALTIME -> "REALTIME"
            null -> "unavailable"
            else -> "OTHER"
        }
        val minFocusDistance = runCatching { chars.get(CameraCharacteristics.LENS_INFO_MINIMUM_FOCUS_DISTANCE) }.getOrNull()
        val physicalIds = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
            runCatching { chars.physicalCameraIds.sorted() }.getOrDefault(emptyList())
        } else {
            emptyList()
        }
        return CharacteristicsReport(
            hwLevel = hwLevel,
            exposureMinNs = expRange?.lower,
            exposureMaxNs = expRange?.upper,
            maxFrameDurationNs = maxFrameDurationNs,
            effectiveExposureMaxNs = effectiveExposureMax,
            isoMin = isoRange?.lower,
            isoMax = isoRange?.upper,
            maxAnalogIso = maxAnalogIso,
            postRawBoostMin = boostRange?.lower,
            postRawBoostMax = boostRange?.upper,
            aeFpsRanges = fpsRanges,
            highSpeedModes = highSpeed,
            capabilities = capabilities.sorted().map(::cameraCapabilityName),
            timestampSource = timestampSource,
            minFocusDistance = minFocusDistance,
            physicalCameraIds = physicalIds,
        )
    }

    private fun readFormatDurations(map: StreamConfigurationMap?): List<FormatDurationEntry> {
        val formats = listOf(
            "YUV_420_888" to ImageFormat.YUV_420_888,
            "JPEG" to ImageFormat.JPEG,
            "RAW_SENSOR" to ImageFormat.RAW_SENSOR,
        )
        val out = mutableListOf<FormatDurationEntry>()
        for ((name, format) in formats) {
            if (map == null) {
                out += FormatDurationEntry(name, "largest", null, null, null, "StreamConfigurationMap unavailable")
                out += FormatDurationEntry(name, "1080p", null, null, null, "StreamConfigurationMap unavailable")
                continue
            }
            val sizes = outputSizesForFormat(map, format)
            val largest = ProbePlan.largestSize(sizes)
            if (largest == null) {
                out += FormatDurationEntry(name, "largest", null, null, null, "format unavailable")
                out += FormatDurationEntry(name, "1080p", null, null, null, "format unavailable")
                continue
            }
            out += measureDurationEntry(map, name, format, "largest", largest)
            val fullHd = ProbeSize(1920, 1080)
            if (fullHd in sizes) {
                out += measureDurationEntry(map, name, format, "1080p", fullHd)
            } else {
                out += FormatDurationEntry(name, "1080p", null, null, null, "1920x1080 unavailable")
            }
        }
        return out
    }

    private fun measureDurationEntry(
        map: StreamConfigurationMap,
        formatName: String,
        format: Int,
        targetLabel: String,
        size: ProbeSize,
    ): FormatDurationEntry {
        val androidSize = Size(size.width, size.height)
        val minDur = runCatching { map.getOutputMinFrameDuration(format, androidSize) }.getOrNull()
        val stallDur = runCatching { map.getOutputStallDuration(format, androidSize) }.getOrNull()
        return FormatDurationEntry(
            formatName = formatName,
            targetLabel = targetLabel,
            size = size,
            minFrameDurationNs = minDur,
            stallDurationNs = stallDur,
        )
    }

    private fun readVendorKeysAndExtensions(
        lens: ProbeLens,
        chars: CameraCharacteristics,
        capabilities: Set<Int>,
    ): VendorKeysReport {
        val reqNames = runCatching { chars.availableCaptureRequestKeys.map { it.name }.orEmpty() }.getOrDefault(emptyList())
        val resNames = runCatching { chars.availableCaptureResultKeys.map { it.name }.orEmpty() }.getOrDefault(emptyList())
        val sessionSelection: VendorKeySelection?
        val sessionSkipped: String?
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
            val sessionNames = runCatching { chars.availableSessionKeys?.map { it.name }.orEmpty() }.getOrDefault(emptyList())
            sessionSelection = ProbePlan.selectKeys(sessionNames)
            sessionSkipped = null
        } else {
            sessionSelection = null
            sessionSkipped = "requires Android 9"
        }
        val physReqSelection = if (!lens.isPhysicalOnly &&
            Build.VERSION.SDK_INT >= Build.VERSION_CODES.P &&
            CameraCharacteristics.REQUEST_AVAILABLE_CAPABILITIES_LOGICAL_MULTI_CAMERA in capabilities
        ) {
            val physNames = runCatching { chars.availablePhysicalCameraRequestKeys?.map { it.name }.orEmpty() }
                .getOrDefault(emptyList())
            ProbePlan.selectKeys(physNames)
        } else {
            null
        }

        val extensions = mutableListOf<ExtensionEntry>()
        var extSkipped: String? = null
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.S) {
            extSkipped = "requires Android 12"
        } else if (lens.isPhysicalOnly) {
            extSkipped = "physical-only lens"
        } else {
            runCatching {
                val extChars = manager.getCameraExtensionCharacteristics(lens.id)
                for (code in extChars.supportedExtensions) {
                    val jpegSizes = runCatching {
                        extChars.getExtensionSupportedSizes(code, ImageFormat.JPEG).map { ProbeSize(it.width, it.height) }
                    }.getOrDefault(emptyList())
                    val yuvSizes = runCatching {
                        extChars.getExtensionSupportedSizes(code, ImageFormat.YUV_420_888).map { ProbeSize(it.width, it.height) }
                    }.getOrDefault(emptyList())
                    extensions += ExtensionEntry(
                        name = ProbePlan.extensionName(code),
                        maxJpegSize = ProbePlan.largestSize(jpegSizes),
                        maxYuvSize = ProbePlan.largestSize(yuvSizes),
                    )
                }
            }.onFailure { e ->
                extSkipped = "query failed: ${e.javaClass.simpleName}: ${e.message}"
            }
        }
        return VendorKeysReport(
            requestKeys = ProbePlan.selectKeys(reqNames),
            resultKeys = ProbePlan.selectKeys(resNames),
            sessionKeys = sessionSelection,
            sessionKeysSkippedReason = sessionSkipped,
            physicalRequestKeys = physReqSelection,
            extensions = extensions,
            extensionsSkippedReason = extSkipped,
        )
    }

    private fun outputSizesForFormat(map: StreamConfigurationMap, format: Int): List<ProbeSize> =
        runCatching { map.getOutputSizes(format)?.map { ProbeSize(it.width, it.height) }.orEmpty() }
            .getOrDefault(emptyList())

    private fun outputSizesForClass(map: StreamConfigurationMap, klass: Class<*>): List<ProbeSize> =
        runCatching { map.getOutputSizes(klass)?.map { ProbeSize(it.width, it.height) }.orEmpty() }
            .getOrDefault(emptyList())


    override fun registerThermalMonitor(
        worker: ProbeWorker,
        onHot: (ProbeThermalSnapshot) -> Unit,
    ): ProbeCancellable {
        val handler = (worker as AndroidProbeWorker).handler
        var listener: Any? = null
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            val thermalCb = PowerManager.OnThermalStatusChangedListener { status ->
                val snapshot = readThermalSnapshot(overrideStatus = status)
                if (ProbeThermalPolicy.tooHotToContinue(snapshot)) {
                    onHot(snapshot)
                }
            }
            runCatching {
                powerManager.addThermalStatusListener(Executor { cmd -> handler.post(cmd) }, thermalCb)
                listener = thermalCb
            }
        }
        var active = true
        val poller = object : Runnable {
            override fun run() {
                if (!active) return
                val snapshot = readThermalSnapshot()
                if (ProbeThermalPolicy.tooHotToContinue(snapshot)) {
                    onHot(snapshot)
                    return
                }
                handler.postDelayed(this, HeatStep.FORECAST_EVERY_MS)
            }
        }
        handler.postDelayed(poller, HeatStep.FORECAST_EVERY_MS)
        return ProbeCancellable {
            active = false
            handler.removeCallbacks(poller)
            val registered = listener
            listener = null
            if (registered != null && Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                runCatching {
                    powerManager.removeThermalStatusListener(registered as PowerManager.OnThermalStatusChangedListener)
                }
            }
        }
    }

    override fun readThermalSnapshot(overrideStatus: Int?): ProbeThermalSnapshot {
        val status = overrideStatus ?: if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            runCatching { powerManager.currentThermalStatus }.getOrDefault(0)
        } else {
            0
        }
        val headroom = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            runCatching { powerManager.getThermalHeadroom(HeatStep.FORECAST_S) }.getOrDefault(Float.NaN)
        } else {
            Float.NaN
        }
        val batteryTempC = runCatching {
            val intent = appContext.registerReceiver(null, IntentFilter(Intent.ACTION_BATTERY_CHANGED))
            val tenths = intent?.getIntExtra(BatteryManager.EXTRA_TEMPERATURE, Int.MIN_VALUE) ?: Int.MIN_VALUE
            if (tenths == Int.MIN_VALUE || tenths <= 0) null else tenths / 10.0
        }.getOrNull()
        return ProbeThermalSnapshot(status = status, headroom = headroom, batteryTempC = batteryTempC)
    }

    private fun armTimeout(runnable: Runnable, delayMs: Long) {
        cancelActiveTimeout()
        activeTimeout = runnable
        cameraHandler?.postDelayed(runnable, delayMs)
    }

    private fun cancelActiveTimeout() {
        activeTimeout?.let { cameraHandler?.removeCallbacks(it) }
        activeTimeout = null
    }

    private fun closeActiveCapture(abort: Boolean = false) {
        cancelActiveTimeout()
        val session = activeSession
        activeSession = null
        if (session != null) {
            runCatching { session.stopRepeating() }
            if (abort) runCatching { session.abortCaptures() }
            runCatching { session.close() }
        }
        val readers = activeReaders.toList()
        activeReaders.clear()
        for (reader in readers) {
            runCatching { reader.setOnImageAvailableListener(null, null) }
            drainReader(reader)
            runCatching { reader.close() }
        }
    }

    private fun closeActiveDevice(abort: Boolean = false) {
        closeActiveCapture(abort)
        val device = activeDevice
        activeDevice = null
        if (device != null) {
            runCatching { device.close() }
        }
    }

    private fun drainReader(reader: ImageReader) {
        while (true) {
            val image = try {
                reader.acquireNextImage()
            } catch (_: Exception) {
                null
            } ?: break
            runCatching { image.close() }
        }
    }

    companion object {
        private const val TAG = "CameraProbeRunner"
        private const val CAMERA_OPEN_TIMEOUT_MS = 10_000L
        private const val SESSION_OPEN_TIMEOUT_MS = 10_000L
        private const val SINGLE_CAPTURE_TIMEOUT_MS = 5_000L
        private const val BURST_DRAIN_MS = 600L
        private const val BETWEEN_LENSES_MS = 150L
    }
}

// A REQUEST_AVAILABLE_CAPABILITIES code by its name, for the probe's report and the lens's capabilities in /v1/state
internal fun cameraCapabilityName(code: Int): String = when (code) {
    CameraCharacteristics.REQUEST_AVAILABLE_CAPABILITIES_BACKWARD_COMPATIBLE -> "BACKWARD_COMPATIBLE"
    CameraCharacteristics.REQUEST_AVAILABLE_CAPABILITIES_MANUAL_SENSOR -> "MANUAL_SENSOR"
    CameraCharacteristics.REQUEST_AVAILABLE_CAPABILITIES_MANUAL_POST_PROCESSING -> "MANUAL_POST_PROCESSING"
    CameraCharacteristics.REQUEST_AVAILABLE_CAPABILITIES_RAW -> "RAW"
    CameraCharacteristics.REQUEST_AVAILABLE_CAPABILITIES_PRIVATE_REPROCESSING -> "PRIVATE_REPROCESSING"
    CameraCharacteristics.REQUEST_AVAILABLE_CAPABILITIES_READ_SENSOR_SETTINGS -> "READ_SENSOR_SETTINGS"
    CameraCharacteristics.REQUEST_AVAILABLE_CAPABILITIES_BURST_CAPTURE -> "BURST_CAPTURE"
    CameraCharacteristics.REQUEST_AVAILABLE_CAPABILITIES_YUV_REPROCESSING -> "YUV_REPROCESSING"
    CameraCharacteristics.REQUEST_AVAILABLE_CAPABILITIES_DEPTH_OUTPUT -> "DEPTH_OUTPUT"
    CameraCharacteristics.REQUEST_AVAILABLE_CAPABILITIES_CONSTRAINED_HIGH_SPEED_VIDEO -> "CONSTRAINED_HIGH_SPEED_VIDEO"
    CameraCharacteristics.REQUEST_AVAILABLE_CAPABILITIES_MOTION_TRACKING -> "MOTION_TRACKING"
    CameraCharacteristics.REQUEST_AVAILABLE_CAPABILITIES_LOGICAL_MULTI_CAMERA -> "LOGICAL_MULTI_CAMERA"
    CameraCharacteristics.REQUEST_AVAILABLE_CAPABILITIES_MONOCHROME -> "MONOCHROME"
    13 -> "SECURE_IMAGE_DATA"
    14 -> "SYSTEM_CAMERA"
    15 -> "OFFLINE_PROCESSING"
    16 -> "ULTRA_HIGH_RESOLUTION_SENSOR"
    17 -> "REMOSAIC_REPROCESSING"
    18 -> "DYNAMIC_RANGE_TEN_BIT"
    19 -> "STREAM_USE_CASE"
    20 -> "COLOR_SPACE_PROFILES"
    else -> "CAP_$code"
}
