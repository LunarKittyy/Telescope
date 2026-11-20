package com.telescope

import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale
import kotlin.math.abs

// Pure data types, planning logic, timestamp matching and report formatting for the Camera2 probe.
// Kept free of Android runtime calls so every rule can be verified in JVM unit tests.

data class ProbeSize(
    val width: Int,
    val height: Int,
) {
    val pixels: Long get() = width.toLong() * height.toLong()
    override fun toString(): String = "${width}x${height}"
}

data class ProbeLens(
    val id: String,
    val logicalId: String?,
    val label: String,
    val facing: String,
    val focalMm: Int,
    val hasOis: Boolean,
    val lensKind: String,
    val maxPixels: Long = 0L,
) {
    val isPhysicalOnly: Boolean get() = logicalId != null
}

data class ProbeThermalSnapshot(
    val status: Int,
    val headroom: Float = Float.NaN,
    val batteryTempC: Double? = null,
) {
    val effectiveStatus: Int get() = HeatStep.status(status, headroom)

    fun summary(): String {
        val statusText = "${ProbeThermalPolicy.statusName(status)}($status)"
        val headroomText = if (headroom.isFinite()) String.format(Locale.ROOT, "%.2f", headroom) else "n/a"
        val batteryText = batteryTempC?.let { String.format(Locale.ROOT, "%.1f C", it) } ?: "n/a"
        return "thermal=$statusText, headroom=$headroomText, battery=$batteryText"
    }
}

// Thermal safety thresholds: refuse to start on a hot phone and stop mid-probe if heat rises.
object ProbeThermalPolicy {
    const val START_MAX_BATTERY_C = 42.0
    const val RUN_MAX_BATTERY_C = 44.0

    fun tooHotToStart(snapshot: ProbeThermalSnapshot): Boolean {
        if (snapshot.effectiveStatus >= HeatStep.SEVERE) return true
        val battery = snapshot.batteryTempC
        return battery != null && battery >= START_MAX_BATTERY_C
    }

    fun tooHotToContinue(snapshot: ProbeThermalSnapshot): Boolean {
        if (snapshot.effectiveStatus >= HeatStep.SEVERE) return true
        val battery = snapshot.batteryTempC
        return battery != null && battery >= RUN_MAX_BATTERY_C
    }

    fun statusName(status: Int): String = when (status) {
        0 -> "NONE"
        1 -> "LIGHT"
        2 -> "MODERATE"
        3 -> "SEVERE"
        4 -> "CRITICAL"
        5 -> "EMERGENCY"
        6 -> "SHUTDOWN"
        else -> "STATUS_$status"
    }
}

enum class ProbeOutcome {
    OK,
    CLAMPED,
    UNSUPPORTED,
    SKIPPED,
    TIMEOUT,
    CAPTURE_FAILURE,
    CANCELLED,
    INTERRUPTED,
    THERMAL_STOP,
    ERROR,
}

enum class ProbeRunStatus {
    COMPLETED,
    CANCELLED,
    INTERRUPTED,
    THERMAL_STOP,
    FAILED,
}

data class ExposureStepPlan(
    val askedExposureNs: Long,
    val askedFrameDurationNs: Long,
    val timeoutMs: Long,
)

data class VendorKeySelection(
    val vendorKeys: List<String>,
    val keywordKeys: List<String>,
)

// Pure selection and planning rules shared by the runner and unit tests.
object ProbePlan {
    const val MAIN_FOCAL_MM = 26
    const val EXPOSURE_TARGET_FRAMES = 3
    const val BURST_DURATION_MS = 3_000L
    const val BURST_MAX_IMAGES = 4
    const val CLAMP_TOLERANCE = 0.85

    // Standard exposure steps from 1/30 s up to 30 s, capped per lens at sensor and frame-duration limits.
    val EXPOSURE_STEPS_NS = listOf(
        33_333_333L,
        125_000_000L,
        500_000_000L,
        1_000_000_000L,
        2_000_000_000L,
        4_000_000_000L,
        8_000_000_000L,
        15_000_000_000L,
        30_000_000_000L,
    )

    val VENDOR_KEYWORDS = listOf("slow", "long", "shutter", "pro", "night", "exposure")

    // Orders every lens so the main back lens runs first, followed by other back lenses, physical back lenses, front lenses and external lenses.
    fun orderLenses(lenses: List<ProbeLens>): List<ProbeLens> {
        if (lenses.size <= 1) return lenses
        val backTopLevel = lenses.filter { it.facing == LensLabel.BACK && !it.isPhysicalOnly }
        val mainId = backTopLevel.firstOrNull { it.id == "0" }?.id
            ?: backTopLevel.minWithOrNull(withinTierComparator)?.id
        return lenses.sortedWith(
            compareBy<ProbeLens> { lensTier(it, mainId) }
                .then(withinTierComparator),
        )
    }

    private fun lensTier(lens: ProbeLens, mainId: String?): Int = when {
        mainId != null && !lens.isPhysicalOnly && lens.id == mainId -> 0
        lens.facing == LensLabel.BACK && !lens.isPhysicalOnly -> 1
        lens.facing == LensLabel.BACK && lens.isPhysicalOnly -> 2
        lens.facing == LensLabel.FRONT && !lens.isPhysicalOnly -> 3
        lens.facing == LensLabel.FRONT && lens.isPhysicalOnly -> 4
        !lens.isPhysicalOnly -> 5
        else -> 6
    }

    private val withinTierComparator = Comparator<ProbeLens> { a, b ->
        val aFocalDist = if (a.focalMm > 0) abs(a.focalMm - MAIN_FOCAL_MM) else Int.MAX_VALUE
        val bFocalDist = if (b.focalMm > 0) abs(b.focalMm - MAIN_FOCAL_MM) else Int.MAX_VALUE
        if (aFocalDist != bFocalDist) return@Comparator aFocalDist.compareTo(bFocalDist)
        val aOis = if (a.hasOis) 0 else 1
        val bOis = if (b.hasOis) 0 else 1
        if (aOis != bOis) return@Comparator aOis.compareTo(bOis)
        if (a.maxPixels != b.maxPixels) return@Comparator b.maxPixels.compareTo(a.maxPixels)
        compareNaturalIds(a.id, b.id)
    }

    private fun compareNaturalIds(a: String, b: String): Int {
        val ai = a.toIntOrNull()
        val bi = b.toIntOrNull()
        return when {
            ai != null && bi != null -> ai.compareTo(bi)
            ai != null -> -1
            bi != null -> 1
            else -> a.compareTo(b)
        }
    }

    // Builds exposure steps bounded by both sensor exposure range and max frame duration, with per-frame timeout = 2 * duration + 2 s.
    fun exposureSteps(
        minExposureNs: Long,
        maxExposureNs: Long,
        maxFrameDurationNs: Long?,
        minFrameDurationNs: Long = 0L,
    ): List<ExposureStepPlan> {
        if (minExposureNs <= 0L || maxExposureNs <= 0L) return emptyList()
        val frameCap = maxFrameDurationNs?.takeIf { it > 0L } ?: Long.MAX_VALUE
        val effectiveMax = minOf(maxExposureNs, frameCap)
        if (effectiveMax < minExposureNs) return emptyList()

        val chosen = EXPOSURE_STEPS_NS.filter { it in minExposureNs..effectiveMax }.toMutableList()
        val maxStandard = EXPOSURE_STEPS_NS.last()
        if (effectiveMax < maxStandard) {
            val last = chosen.lastOrNull()
            if (last == null || effectiveMax > (last * 1.01).toLong()) {
                chosen += effectiveMax
            }
        }
        if (chosen.isEmpty()) chosen += effectiveMax

        val floorFrame = minFrameDurationNs.coerceAtLeast(0L)
        return chosen.map { expNs ->
            val frameDurationNs = maxOf(expNs, floorFrame).coerceAtMost(frameCap)
            val timeoutMs = (frameDurationNs / 1_000_000L) * 2L + 2_000L
            ExposureStepPlan(
                askedExposureNs = expNs,
                askedFrameDurationNs = frameDurationNs,
                timeoutMs = timeoutMs,
            )
        }
    }

    // True when either the echoed exposure or the real timestamp gap falls below 85% of what was asked.
    fun isExposureClamped(
        askedExposureNs: Long,
        echoedExposureNs: Long?,
        observedGapNs: Long?,
    ): Boolean {
        if (askedExposureNs <= 0L) return false
        val floor = (askedExposureNs * CLAMP_TOLERANCE).toLong()
        if (echoedExposureNs != null && echoedExposureNs < floor) return true
        if (observedGapNs != null && observedGapNs < floor) return true
        return false
    }

    // Least processing mode available: OFF (0), then MINIMAL (3), then FAST (1).
    fun leastNrMode(advertised: Collection<Int>): Int? =
        listOf(0, 3, 1).firstOrNull { it in advertised } ?: advertised.minOrNull()

    // Least sharpening mode available: OFF (0), then FAST (1).
    fun leastEdgeMode(advertised: Collection<Int>): Int? =
        listOf(0, 1).firstOrNull { it in advertised } ?: advertised.minOrNull()

    fun nrModeName(mode: Int?): String = when (mode) {
        null -> "none"
        0 -> "OFF(0)"
        1 -> "FAST(1)"
        2 -> "HIGH_QUALITY(2)"
        3 -> "MINIMAL(3)"
        4 -> "ZERO_SHUTTER_LAG(4)"
        else -> "MODE_$mode($mode)"
    }

    fun edgeModeName(mode: Int?): String = when (mode) {
        null -> "none"
        0 -> "OFF(0)"
        1 -> "FAST(1)"
        2 -> "HIGH_QUALITY(2)"
        3 -> "ZERO_SHUTTER_LAG(3)"
        else -> "MODE_$mode($mode)"
    }

    fun extensionName(code: Int): String = when (code) {
        0 -> "AUTOMATIC(0)"
        1 -> "FACE_RETOUCH(1)"
        2 -> "BOKEH(2)"
        3 -> "HDR(3)"
        4 -> "NIGHT(4)"
        5 -> "EYES_FREE_VIDEOGRAPHY(5)"
        else -> "EXTENSION_$code($code)"
    }

    // Splits key names into vendor keys (not starting with android.) and keys containing any long-exposure or pro keyword.
    fun selectKeys(names: Collection<String>): VendorKeySelection {
        val sorted = names.filter { it.isNotBlank() }.distinct().sorted()
        val vendor = sorted.filter { !it.startsWith("android.") }
        val keyword = sorted.filter { key ->
            val lower = key.lowercase(Locale.ROOT)
            VENDOR_KEYWORDS.any { it in lower }
        }
        return VendorKeySelection(vendorKeys = vendor, keywordKeys = keyword)
    }

    fun largestSize(sizes: List<ProbeSize>): ProbeSize? = sizes.maxByOrNull { it.pixels }

    // Picks a small YUV output near 640x480 for repeating long-exposure tests.
    fun pickSmallYuvSize(sizes: List<ProbeSize>): ProbeSize? {
        if (sizes.isEmpty()) return null
        val targetPixels = 640L * 480L
        return sizes.filter { it.width in 160..1920 }
            .minByOrNull { abs(it.pixels - targetPixels) }
            ?: sizes.minByOrNull { it.pixels }
    }

    // Picks 1080p when listed, or the closest size up to 1920 wide to represent Telescope's video stream.
    fun pickStreamSize(sizes: List<ProbeSize>): ProbeSize? {
        if (sizes.isEmpty()) return null
        val fullHd = ProbeSize(1920, 1080)
        if (fullHd in sizes) return fullHd
        val pool = sizes.filter { it.width <= 1920 }.ifEmpty { sizes }
        val (w, h) = CameraRequestSelection.closestSize(1920, 1080, pool.map { it.width to it.height })
        return ProbeSize(w, h)
    }
}

// Matches Camera2 Image timestamps to TotalCaptureResult SENSOR_TIMESTAMP values regardless of arrival order.
class TimestampMatcher<T : Any>(private val maxPending: Int = 64) {
    data class MatchedFrame<T>(
        val timestampNs: Long,
        val payload: T,
    )

    private val pendingImages = LinkedHashMap<Long, Unit>()
    private val pendingResults = LinkedHashMap<Long, T>()
    private val matchedTimestamps = HashSet<Long>()
    private val matchedList = mutableListOf<MatchedFrame<T>>()

    var imagesSeen: Int = 0
        private set
    var resultsSeen: Int = 0
        private set
    var evictedImages: Int = 0
        private set
    var evictedResults: Int = 0
        private set

    fun onImage(timestampNs: Long): MatchedFrame<T>? {
        imagesSeen++
        if (timestampNs in matchedTimestamps) return null
        val payload = pendingResults.remove(timestampNs)
        if (payload != null) {
            val pair = MatchedFrame(timestampNs, payload)
            matchedTimestamps += timestampNs
            matchedList += pair
            return pair
        }
        if (timestampNs !in pendingImages) {
            pendingImages[timestampNs] = Unit
            while (pendingImages.size > maxPending) {
                val oldest = pendingImages.entries.iterator().next().key
                pendingImages.remove(oldest)
                evictedImages++
            }
        }
        return null
    }

    fun onResult(timestampNs: Long, payload: T): MatchedFrame<T>? {
        resultsSeen++
        if (timestampNs in matchedTimestamps) return null
        if (pendingImages.remove(timestampNs) != null) {
            val pair = MatchedFrame(timestampNs, payload)
            matchedTimestamps += timestampNs
            matchedList += pair
            return pair
        }
        if (timestampNs !in pendingResults) {
            pendingResults[timestampNs] = payload
            while (pendingResults.size > maxPending) {
                val oldest = pendingResults.entries.iterator().next().key
                pendingResults.remove(oldest)
                evictedResults++
            }
        }
        return null
    }

    val matched: List<MatchedFrame<T>> get() = matchedList.toList()
    val unmatchedImagesCount: Int get() = pendingImages.size + evictedImages
    val unmatchedResultsCount: Int get() = pendingResults.size + evictedResults

    // Consecutive positive intervals between matched frame sensor timestamps.
    fun timestampGapsNs(): List<Long> {
        if (matchedList.size < 2) return emptyList()
        val sorted = matchedList.map { it.timestampNs }.sorted()
        return (1 until sorted.size).map { sorted[it] - sorted[it - 1] }
    }

    // Frames per second from the first to last matched sensor timestamp, or null with fewer than two matched frames.
    fun sensorFps(): Double? {
        if (matchedList.size < 2) return null
        val sorted = matchedList.map { it.timestampNs }.sorted()
        val spanNs = sorted.last() - sorted.first()
        if (spanNs <= 0L) return null
        return (sorted.size - 1) * 1_000_000_000.0 / spanNs
    }
}

// Report data structures, all immutable once snapshotted for the writer worker.

data class CharacteristicsReport(
    val hwLevel: String,
    val exposureMinNs: Long?,
    val exposureMaxNs: Long?,
    val maxFrameDurationNs: Long?,
    val effectiveExposureMaxNs: Long?,
    val isoMin: Int?,
    val isoMax: Int?,
    val maxAnalogIso: Int?,
    val postRawBoostMin: Int?,
    val postRawBoostMax: Int?,
    val aeFpsRanges: List<String>,
    val highSpeedModes: List<String>,
    val capabilities: List<String>,
    val timestampSource: String,
    val minFocusDistance: Float?,
    val physicalCameraIds: List<String>,
)

data class FormatDurationEntry(
    val formatName: String,
    val targetLabel: String,
    val size: ProbeSize?,
    val minFrameDurationNs: Long?,
    val stallDurationNs: Long?,
    val skippedReason: String? = null,
)

data class ExposureStepResult(
    val askedExposureNs: Long,
    val askedFrameDurationNs: Long,
    val iso: Int,
    val outcome: ProbeOutcome,
    val matchedFrames: Int,
    val imagesSeen: Int,
    val resultsSeen: Int,
    val echoedExposuresNs: List<Long>,
    val echoedFrameDurationsNs: List<Long>,
    val timestampGapsNs: List<Long>,
    val detail: String? = null,
)

data class ExposureSweepReport(
    val outcome: ProbeOutcome,
    val yuvSize: ProbeSize?,
    val steps: List<ExposureStepResult>,
    val stopReason: String? = null,
    val detail: String? = null,
)

data class BurstFormatResult(
    val formatName: String,
    val size: ProbeSize?,
    val outcome: ProbeOutcome,
    val elapsedMs: Long,
    val imagesSeen: Int,
    val resultsSeen: Int,
    val matchedFrames: Int,
    val bufferLost: Int,
    val captureFailed: Int,
    val unmatchedResults: Int,
    val sensorFps: Double?,
    val wallFps: Double?,
    val detail: String? = null,
)

data class ExtensionEntry(
    val name: String,
    val maxJpegSize: ProbeSize?,
    val maxYuvSize: ProbeSize?,
)

data class VendorKeysReport(
    val requestKeys: VendorKeySelection,
    val resultKeys: VendorKeySelection,
    val sessionKeys: VendorKeySelection?,
    val sessionKeysSkippedReason: String? = null,
    val physicalRequestKeys: VendorKeySelection? = null,
    val extensions: List<ExtensionEntry>,
    val extensionsSkippedReason: String? = null,
)

data class ProcessingCheckEntry(
    val formatName: String,
    val size: ProbeSize?,
    val label: String,
    val askedNr: Int?,
    val askedEdge: Int?,
    val outcome: ProbeOutcome,
    val echoedNr: Int? = null,
    val echoedEdge: Int? = null,
    val detail: String? = null,
)

data class ProcessingReport(
    val advertisedNr: List<Int>,
    val advertisedEdge: List<Int>,
    val leastNr: Int?,
    val leastEdge: Int?,
    val checks: List<ProcessingCheckEntry>,
)

data class SessionCheckEntry(
    val label: String,
    val outcome: ProbeOutcome,
    val detail: String? = null,
)

data class SessionChecksReport(
    val skippedReason: String? = null,
    val checks: List<SessionCheckEntry> = emptyList(),
)

data class LensProbeReport(
    val lens: ProbeLens,
    val status: ProbeOutcome,
    val detail: String? = null,
    val characteristics: CharacteristicsReport? = null,
    val durations: List<FormatDurationEntry> = emptyList(),
    val exposureSweep: ExposureSweepReport? = null,
    val bursts: List<BurstFormatResult> = emptyList(),
    val vendorKeys: VendorKeysReport? = null,
    val processing: ProcessingReport? = null,
    val sessionChecks: SessionChecksReport? = null,
)

data class ProbeReportData(
    val header: String,
    val startedAtMs: Long,
    val finishedAtMs: Long,
    val status: ProbeRunStatus,
    val stopReason: String?,
    val initialThermal: ProbeThermalSnapshot,
    val finalThermal: ProbeThermalSnapshot,
    val totalLenses: Int,
    val completedLenses: Int,
    val lenses: List<LensProbeReport>,
)

// Accumulates per-lens probe results so any mid-run cancellation, thermal stop or error keeps every partial measurement.
class ProbeSessionAccumulator(
    private val header: String,
    private val startedAtMs: Long,
    private val initialThermal: ProbeThermalSnapshot,
    val orderedLenses: List<ProbeLens>,
) {
    private class ActiveLens(val lens: ProbeLens) {
        var detail: String? = null
        var characteristics: CharacteristicsReport? = null
        var durations: List<FormatDurationEntry> = emptyList()
        var exposureYuvSize: ProbeSize? = null
        val exposureSteps = mutableListOf<ExposureStepResult>()
        var exposureSweep: ExposureSweepReport? = null
        val bursts = mutableListOf<BurstFormatResult>()
        var vendorKeys: VendorKeysReport? = null
        var processing: ProcessingReport? = null
        var sessionChecks: SessionChecksReport? = null

        fun build(status: ProbeOutcome, fallbackDetail: String?): LensProbeReport {
            val sweep = exposureSweep ?: if (exposureSteps.isNotEmpty()) {
                ExposureSweepReport(
                    outcome = status,
                    yuvSize = exposureYuvSize,
                    steps = exposureSteps.toList(),
                    stopReason = fallbackDetail,
                )
            } else {
                null
            }
            return LensProbeReport(
                lens = lens,
                status = status,
                detail = detail ?: fallbackDetail,
                characteristics = characteristics,
                durations = durations,
                exposureSweep = sweep,
                bursts = bursts.toList(),
                vendorKeys = vendorKeys,
                processing = processing,
                sessionChecks = sessionChecks,
            )
        }
    }

    private val completed = mutableListOf<LensProbeReport>()
    private var active: ActiveLens? = null

    val completedCount: Int get() = completed.size

    fun beginLens(lens: ProbeLens) {
        active = ActiveLens(lens)
    }

    fun setCharacteristics(report: CharacteristicsReport) {
        active?.characteristics = report
    }

    fun setDurations(entries: List<FormatDurationEntry>) {
        active?.durations = entries
    }

    fun setVendorKeys(report: VendorKeysReport) {
        active?.vendorKeys = report
    }

    fun setSessionChecks(report: SessionChecksReport) {
        active?.sessionChecks = report
    }

    fun setProcessing(report: ProcessingReport) {
        active?.processing = report
    }

    fun beginExposureSweep(yuvSize: ProbeSize?) {
        active?.exposureYuvSize = yuvSize
    }

    fun addExposureStep(step: ExposureStepResult) {
        active?.exposureSteps?.add(step)
    }

    fun finishExposureSweep(
        outcome: ProbeOutcome,
        yuvSize: ProbeSize?,
        stopReason: String? = null,
        detail: String? = null,
    ) {
        val current = active ?: return
        current.exposureYuvSize = yuvSize
        current.exposureSweep = ExposureSweepReport(
            outcome = outcome,
            yuvSize = yuvSize,
            steps = current.exposureSteps.toList(),
            stopReason = stopReason,
            detail = detail,
        )
    }

    fun addBurst(result: BurstFormatResult) {
        active?.bursts?.add(result)
    }

    fun finishLens(status: ProbeOutcome, detail: String? = null) {
        val current = active ?: return
        if (detail != null) current.detail = detail
        completed += current.build(status, detail)
        active = null
    }

    fun snapshot(
        status: ProbeRunStatus,
        stopReason: String?,
        finishedAtMs: Long,
        finalThermal: ProbeThermalSnapshot,
    ): ProbeReportData {
        val allLenses = mutableListOf<LensProbeReport>()
        allLenses.addAll(completed)
        val inProgress = active
        if (inProgress != null) {
            val partialOutcome = when (status) {
                ProbeRunStatus.CANCELLED -> ProbeOutcome.CANCELLED
                ProbeRunStatus.INTERRUPTED -> ProbeOutcome.INTERRUPTED
                ProbeRunStatus.THERMAL_STOP -> ProbeOutcome.THERMAL_STOP
                ProbeRunStatus.FAILED -> ProbeOutcome.ERROR
                ProbeRunStatus.COMPLETED -> ProbeOutcome.OK
            }
            allLenses += inProgress.build(partialOutcome, stopReason)
        }
        return ProbeReportData(
            header = header,
            startedAtMs = startedAtMs,
            finishedAtMs = finishedAtMs,
            status = status,
            stopReason = stopReason,
            initialThermal = initialThermal,
            finalThermal = finalThermal,
            totalLenses = orderedLenses.size,
            completedLenses = completed.size,
            lenses = allLenses,
        )
    }
}

// Formats a ProbeReportData snapshot into a plain text report matching Telescope's diagnostics style.
object CameraProbeReport {

    fun format(data: ProbeReportData): String = buildString {
        append(data.header.trimEnd())
        appendLine()
        appendLine("Report type: Camera2 probe")
        appendLine("Started: ${formatTime(data.startedAtMs)}")
        appendLine("Finished: ${formatTime(data.finishedAtMs)} (elapsed ${formatElapsedMs(data.finishedAtMs - data.startedAtMs)})")
        val reasonSuffix = data.stopReason?.takeIf { it.isNotBlank() }?.let { " ($it)" } ?: ""
        appendLine("Status: ${data.status.name.lowercase(Locale.ROOT)}$reasonSuffix")
        appendLine("Lenses completed: ${data.completedLenses}/${data.totalLenses}")
        appendLine("Initial thermal: ${data.initialThermal.summary()}")
        appendLine("Final thermal: ${data.finalThermal.summary()}")
        appendLine()
        if (data.lenses.isEmpty()) {
            appendLine("Lenses: (none)")
            return@buildString
        }
        data.lenses.forEachIndexed { index, report ->
            appendLens(this, index + 1, data.totalLenses, report)
            appendLine()
        }
    }

    private fun appendLens(sb: StringBuilder, number: Int, total: Int, report: LensProbeReport) {
        val lens = report.lens
        val physNote = lens.logicalId?.let { " via logical $it" } ?: ""
        sb.appendLine("== Lens $number/$total: id=${lens.id}$physNote (${lens.label}) ==")
        val detailSuffix = report.detail?.takeIf { it.isNotBlank() }?.let { " ($it)" } ?: ""
        sb.appendLine("Lens status: ${outcomeTag(report.status)}$detailSuffix")

        sb.appendLine("1. Characteristics:")
        val chars = report.characteristics
        if (chars == null) {
            sb.appendLine("  skipped (not collected)")
        } else {
            sb.appendLine("  hardware level: ${chars.hwLevel}")
            val expRange = if (chars.exposureMinNs != null && chars.exposureMaxNs != null) {
                "${formatNs(chars.exposureMinNs)} .. ${formatNs(chars.exposureMaxNs)} (${chars.exposureMinNs}..${chars.exposureMaxNs} ns)"
            } else {
                "unavailable"
            }
            sb.appendLine("  exposure range: $expRange")
            sb.appendLine("  max frame duration: ${chars.maxFrameDurationNs?.let { "${formatNs(it)} ($it ns)" } ?: "unavailable"}")
            sb.appendLine("  effective manual exposure ceiling: ${chars.effectiveExposureMaxNs?.let { formatNs(it) } ?: "unavailable"}")
            val isoRange = if (chars.isoMin != null && chars.isoMax != null) "${chars.isoMin}..${chars.isoMax}" else "unavailable"
            sb.appendLine("  ISO range: $isoRange, max analogue: ${chars.maxAnalogIso ?: "unavailable"}")
            val boostRange = if (chars.postRawBoostMin != null && chars.postRawBoostMax != null) {
                "${chars.postRawBoostMin}..${chars.postRawBoostMax}"
            } else {
                "unavailable"
            }
            sb.appendLine("  post-RAW boost range: $boostRange")
            sb.appendLine("  AE FPS ranges: ${if (chars.aeFpsRanges.isEmpty()) "none" else chars.aeFpsRanges.joinToString(", ")}")
            sb.appendLine("  high-speed video: ${if (chars.highSpeedModes.isEmpty()) "none" else chars.highSpeedModes.joinToString("; ")}")
            sb.appendLine("  timestamp source: ${chars.timestampSource}")
            sb.appendLine("  min focus distance: ${chars.minFocusDistance?.let { String.format(Locale.ROOT, "%.2f dioptres", it) } ?: "unavailable"}")
            sb.appendLine("  capabilities: ${if (chars.capabilities.isEmpty()) "none" else chars.capabilities.joinToString(", ")}")
            if (chars.physicalCameraIds.isNotEmpty()) {
                sb.appendLine("  physical camera ids: ${chars.physicalCameraIds.joinToString(", ")}")
            }
        }

        sb.appendLine("2. Output min frame duration and stall:")
        if (report.durations.isEmpty()) {
            sb.appendLine("  skipped (not collected)")
        } else {
            for (d in report.durations) {
                if (d.skippedReason != null) {
                    sb.appendLine("  ${d.formatName} ${d.targetLabel}: skipped (${d.skippedReason})")
                } else {
                    val minDur = d.minFrameDurationNs?.let { formatNs(it) } ?: "unavailable"
                    val impliedFps = d.minFrameDurationNs?.takeIf { it > 0L }?.let {
                        String.format(Locale.ROOT, " (%.1f fps max)", 1_000_000_000.0 / it)
                    } ?: ""
                    val stall = d.stallDurationNs?.let { formatNs(it) } ?: "unavailable"
                    sb.appendLine("  ${d.formatName} ${d.targetLabel} (${d.size}): minFrame=$minDur$impliedFps, stall=$stall")
                }
            }
        }

        sb.appendLine("3. Repeating manual exposure sweep:")
        val sweep = report.exposureSweep
        if (sweep == null) {
            sb.appendLine("  skipped (not run)")
        } else {
            val sizeText = sweep.yuvSize?.toString() ?: "none"
            val extra = listOfNotNull(sweep.stopReason, sweep.detail).filter { it.isNotBlank() }.joinToString("; ")
            val extraSuffix = if (extra.isNotEmpty()) " ($extra)" else ""
            sb.appendLine("  outcome: ${outcomeTag(sweep.outcome)}, small YUV=$sizeText$extraSuffix")
            for (step in sweep.steps) {
                val echoedExp = if (step.echoedExposuresNs.isEmpty()) "none"
                else step.echoedExposuresNs.joinToString(",") { formatNs(it) }
                val echoedFrame = if (step.echoedFrameDurationsNs.isEmpty()) "none"
                else step.echoedFrameDurationsNs.joinToString(",") { formatNs(it) }
                val gaps = if (step.timestampGapsNs.isEmpty()) "none"
                else step.timestampGapsNs.joinToString(",") { formatNs(it) }
                val stepDetail = step.detail?.takeIf { it.isNotBlank() }?.let { " [$it]" } ?: ""
                sb.appendLine(
                    "  asked exp=${formatNs(step.askedExposureNs)} frame=${formatNs(step.askedFrameDurationNs)} iso=${step.iso}: " +
                        "${outcomeTag(step.outcome)}, matched=${step.matchedFrames} (img=${step.imagesSeen}, res=${step.resultsSeen}), " +
                        "echoExp=$echoedExp, echoFrame=$echoedFrame, tsGap=$gaps$stepDetail",
                )
            }
        }

        sb.appendLine("4. Full-size burst rates (AE on, 4 images in flight):")
        if (report.bursts.isEmpty()) {
            sb.appendLine("  skipped (not run)")
        } else {
            for (burst in report.bursts) {
                val sizeText = burst.size?.toString() ?: "none"
                if (burst.outcome == ProbeOutcome.UNSUPPORTED || burst.outcome == ProbeOutcome.SKIPPED) {
                    val why = burst.detail?.let { " ($it)" } ?: ""
                    sb.appendLine("  ${burst.formatName} ($sizeText): ${outcomeTag(burst.outcome)}$why")
                } else {
                    val fpsTs = burst.sensorFps?.let { String.format(Locale.ROOT, "%.2f", it) } ?: "n/a"
                    val fpsWall = burst.wallFps?.let { String.format(Locale.ROOT, "%.2f", it) } ?: "n/a"
                    val drops = burst.bufferLost + burst.captureFailed + burst.unmatchedResults
                    val why = burst.detail?.takeIf { it.isNotBlank() }?.let { " [$it]" } ?: ""
                    sb.appendLine(
                        "  ${burst.formatName} ($sizeText): ${outcomeTag(burst.outcome)}, " +
                            "fps=${fpsTs} (wall=${fpsWall}), elapsed=${burst.elapsedMs}ms, " +
                            "matched=${burst.matchedFrames}, img=${burst.imagesSeen}, res=${burst.resultsSeen}, " +
                            "drops=$drops (bufferLost=${burst.bufferLost}, failed=${burst.captureFailed}, missingImg=${burst.unmatchedResults})$why",
                    )
                }
            }
        }

        sb.appendLine("5. Vendor keys and extensions:")
        val vendor = report.vendorKeys
        if (vendor == null) {
            sb.appendLine("  skipped (not collected)")
        } else {
            sb.appendLine("  note: vendor key names alone do not establish a wider usable exposure range.")
            appendKeySummary(sb, "request", vendor.requestKeys)
            appendKeySummary(sb, "result", vendor.resultKeys)
            if (vendor.sessionKeysSkippedReason != null) {
                sb.appendLine("  session keys: skipped (${vendor.sessionKeysSkippedReason})")
            } else if (vendor.sessionKeys != null) {
                appendKeySummary(sb, "session", vendor.sessionKeys)
            }
            vendor.physicalRequestKeys?.let { appendKeySummary(sb, "physical request", it) }
            if (vendor.extensionsSkippedReason != null) {
                sb.appendLine("  extensions: skipped (${vendor.extensionsSkippedReason})")
            } else if (vendor.extensions.isEmpty()) {
                sb.appendLine("  extensions: none")
            } else {
                val list = vendor.extensions.joinToString("; ") { ext ->
                    val jpeg = ext.maxJpegSize?.toString() ?: "none"
                    val yuv = ext.maxYuvSize?.toString() ?: "none"
                    "${ext.name} jpeg=$jpeg yuv=$yuv"
                }
                sb.appendLine("  extensions: $list")
            }
        }

        sb.appendLine("6. Noise reduction and edge modes:")
        val proc = report.processing
        if (proc == null) {
            sb.appendLine("  skipped (not run)")
        } else {
            sb.appendLine("  note: echoed modes describe reported settings and cannot prove pixel processing.")
            val advNr = if (proc.advertisedNr.isEmpty()) "none" else proc.advertisedNr.joinToString(", ") { ProbePlan.nrModeName(it) }
            val advEdge = if (proc.advertisedEdge.isEmpty()) "none" else proc.advertisedEdge.joinToString(", ") { ProbePlan.edgeModeName(it) }
            sb.appendLine("  advertised NR: $advNr (least=${ProbePlan.nrModeName(proc.leastNr)})")
            sb.appendLine("  advertised Edge: $advEdge (least=${ProbePlan.edgeModeName(proc.leastEdge)})")
            if (proc.checks.isEmpty()) {
                sb.appendLine("  checks: skipped (none run)")
            } else {
                for (check in proc.checks) {
                    val sizeText = check.size?.toString() ?: "none"
                    val why = check.detail?.takeIf { it.isNotBlank() }?.let { " [$it]" } ?: ""
                    sb.appendLine(
                        "  ${check.formatName} ($sizeText) ${check.label}: ${outcomeTag(check.outcome)}, " +
                            "asked NR=${ProbePlan.nrModeName(check.askedNr)} Edge=${ProbePlan.edgeModeName(check.askedEdge)}, " +
                            "echoed NR=${ProbePlan.nrModeName(check.echoedNr)} Edge=${ProbePlan.edgeModeName(check.echoedEdge)}$why",
                    )
                }
            }
        }

        sb.appendLine("7. Session configuration checks:")
        val sessions = report.sessionChecks
        if (sessions == null) {
            sb.appendLine("  skipped (not run)")
        } else if (sessions.skippedReason != null) {
            sb.appendLine("  skipped (${sessions.skippedReason})")
        } else if (sessions.checks.isEmpty()) {
            sb.appendLine("  skipped (no stream combinations)")
        } else {
            for (check in sessions.checks) {
                val why = check.detail?.takeIf { it.isNotBlank() }?.let { " ($it)" } ?: ""
                sb.appendLine("  ${check.label}: ${outcomeTag(check.outcome)}$why")
            }
        }
    }

    private fun appendKeySummary(sb: StringBuilder, label: String, selection: VendorKeySelection) {
        sb.appendLine("  $label vendor keys (${selection.vendorKeys.size}): ${if (selection.vendorKeys.isEmpty()) "none" else selection.vendorKeys.joinToString(", ")}")
        sb.appendLine("  $label keyword matches (${selection.keywordKeys.size}): ${if (selection.keywordKeys.isEmpty()) "none" else selection.keywordKeys.joinToString(", ")}")
    }

    fun outcomeTag(outcome: ProbeOutcome): String = outcome.name.lowercase(Locale.ROOT)

    fun formatNs(ns: Long): String {
        return when {
            ns >= 1_000_000_000L -> String.format(Locale.ROOT, "%.3fs", ns / 1_000_000_000.0)
            ns >= 1_000_000L -> String.format(Locale.ROOT, "%.2fms", ns / 1_000_000.0)
            ns >= 1_000L -> String.format(Locale.ROOT, "%.1fus", ns / 1_000.0)
            else -> "${ns}ns"
        }
    }

    fun formatElapsedMs(ms: Long): String {
        val totalSeconds = (ms.coerceAtLeast(0L)) / 1000L
        val minutes = totalSeconds / 60L
        val seconds = totalSeconds % 60L
        return if (minutes > 0L) "${minutes}m ${seconds}s" else "${seconds}s"
    }

    // Caps the on-screen monospace preview length while Copy report always copies the full report.
    fun preview(reportText: String, maxChars: Int = 24_000): String {
        if (reportText.length <= maxChars) return reportText
        return reportText.substring(0, maxChars) + "\n..."
    }

    private fun formatTime(ms: Long): String =
        SimpleDateFormat("yyyy-MM-dd HH:mm:ss", Locale.ROOT).format(Date(ms))
}
