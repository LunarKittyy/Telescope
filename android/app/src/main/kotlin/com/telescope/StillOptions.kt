package com.telescope

import kotlin.math.abs
import kotlin.math.pow
import kotlin.math.roundToLong

// What one lens can do for photos, read from Camera2 by StillCatalog; plain values, so the gating runs in JVM tests.
data class StillCaps(
    val jpeg: Pair<Int, Int>,             // the largest JPEG
    val heic: Pair<Int, Int>? = null,     // ImageFormat.HEIC, Android 10
    val ultraHdr: Pair<Int, Int>? = null, // ImageFormat.JPEG_R, Android 14
    val raw: Pair<Int, Int>? = null,      // RAW_SENSOR, for a DNG
    val maxResJpeg: Pair<Int, Int>? = null,  // the maximum resolution sensor mode's, when it's bigger (Android 12)
    val maxResRaw: Pair<Int, Int>? = null,
    val extensions: List<String> = emptyList(),  // StillOptions.EXTENSIONS names the maker offers here (Android 12)
    val extensionSizes: Map<String, Pair<Int, Int>> = emptyMap(),  // the JPEG size each of those takes its photos at
    val manualSensor: Boolean = false,
    val manualFocus: Boolean = false,
    val exposureMinNs: Long = 0L,
    val exposureMaxNs: Long = 0L,         // the longest exposure the sensor and its frame duration allow
    val isoMin: Int = 0,
    val isoMax: Int = 0,
    val minFocusDistance: Float = 0f,     // diopters; the nearest it focuses
    val nrHq: Boolean = false,
    val edgeHq: Boolean = false,
    val hotPixelHq: Boolean = false,
    val distortion: Boolean = false,      // DISTORTION_CORRECTION_MODE can be switched off and on (Android 9)
) {
    fun capability(): StillCapability = StillCapability(
        formats = listOfNotNull(
            StillOptions.JPEG, StillOptions.HEIC.takeIf { heic != null },
            StillOptions.ULTRA_HDR.takeIf { ultraHdr != null }, StillOptions.RAW.takeIf { raw != null }),
        width = jpeg.first, height = jpeg.second,
        maxResWidth = maxResJpeg?.first ?: 0, maxResHeight = maxResJpeg?.second ?: 0,
        maxResRaw = maxResRaw != null,
        extensions = extensions,
        extensionSizes = extensions.mapNotNull { name -> extensionSizes[name]?.let { name to StillSize(it.first, it.second) } }.toMap(),
        exposureMaxNs = if (manualSensor) exposureMaxNs else 0L,
        bracketing = manualSensor,
        focusBracketing = manualFocus,
        noiseReductionHq = nrHq, sharpeningHq = edgeHq, hotPixelHq = hotPixelHq, distortionCorrection = distortion,
        burstMax = StillOptions.MAX_SHOTS,
    )
}

// One picture to take: its exposure (null = the stream's) and focus (null = the stream's).
data class StillShot(val exposureNs: Long? = null, val iso: Int? = null, val focus: Float? = null)

// A photo as the computer asked for it, checked against the lens. raw and format can come together (DNG and JPEG).
data class StillRequest(
    val format: String?,        // JPEG, HEIC or ULTRA_HDR; null = RAW only
    val raw: Boolean,
    val quality: Int,
    val maxRes: Boolean,
    val extension: String?,
    val shots: List<StillShot>,
    val timerMs: Long,
    val rotation: Int,
    val nrHq: Boolean,
    val edgeHq: Boolean,
    val hotPixelHq: Boolean,
    val distortion: Boolean?,   // null = what the camera does anyway
) {
    // The lens's own session can't hold these next to the stream: the video pauses while it shoots.
    val ownSession: Boolean get() = extension != null
    // Each picture's file count: RAW and a processed one come from the same exposure
    val filesPerShot: Int get() = (if (raw) 1 else 0) + (if (format != null) 1 else 0)
    // How long the sensor is busy, for the job's time limit
    val exposureMs: Long get() = shots.sumOf { (it.exposureNs ?: 0L) / 1_000_000L }
    // Shot next to the stream, manual exposures leave the video out, so long ones freeze it noticeably: reported as paused
    val pausesVideo: Boolean get() = ownSession || exposureMs >= StillOptions.PAUSE_MS
}

// Base exposure for bracketing and long exposures: what the stream runs at (set by hand, or what auto exposure picked).
data class StillExposure(val exposureNs: Long, val iso: Int)

// Reads POST /v1/still into a StillRequest, refusing what this lens can't do with a fixed code the desktop words.
object StillOptions {
    const val JPEG = "jpeg"
    const val HEIC = "heic"
    const val ULTRA_HDR = "ultrahdr"
    const val RAW = "raw"
    // Camera2 extension names, by CameraExtensionCharacteristics.EXTENSION_* value
    val EXTENSIONS = mapOf(0 to "auto", 2 to "bokeh", 3 to "hdr", 4 to "night")
    const val MAX_SHOTS = 60
    const val MAX_BRACKET = 9
    const val MAX_EV = 6.0
    const val MAX_FOCUS_STEPS = 30
    const val MAX_TIMER_S = 30
    const val PAUSE_MS = 500L  // manual exposures adding up to this freeze the video long enough to say so

    sealed class Parsed {
        data class Ok(val request: StillRequest) : Parsed()
        data class Refused(val code: String, val text: String) : Parsed()
    }

    private fun refuse(code: String, text: String) = Parsed.Refused(code, text)

    fun parse(params: Map<String, String>, caps: StillCaps, base: StillExposure?): Parsed {
        val format = params["format"]?.takeIf { it.isNotEmpty() && it != "none" }
        val raw = params["raw"] == "1"
        if (format == null && !raw) return refuse("bad_option", "Ask for a format or RAW")
        when (format) {
            null, JPEG -> {}
            HEIC -> if (caps.heic == null) return refuse("unsupported", "This lens can't save HEIC")
            ULTRA_HDR -> if (caps.ultraHdr == null) return refuse("unsupported", "This lens can't save Ultra HDR")
            else -> return refuse("bad_option", "Unknown format '$format'")
        }
        if (raw && caps.raw == null) return refuse("unsupported", "This lens can't save RAW")
        val quality = (params["quality"]?.toIntOrNull() ?: 95).coerceIn(1, 100)

        val maxRes = params["max_res"] == "1"
        if (maxRes) {
            if (format != null && format != JPEG) return refuse("unsupported", "Full resolution only saves JPEG and RAW")
            if (format != null && caps.maxResJpeg == null) return refuse("unsupported", "This lens has no full resolution mode")
            if (raw && caps.maxResRaw == null) return refuse("unsupported", "This lens can't save RAW at full resolution")
        }

        val extension = params["extension"]?.takeIf { it.isNotEmpty() && it != "none" }
        if (extension != null) {
            if (extension !in caps.extensions) return refuse("unsupported", "This lens has no '$extension' mode")
            // The maker's processing only hands out a finished JPEG, and runs its own exposure
            if (raw || (format != null && format != JPEG) || maxRes) return refuse("unsupported", "Night, HDR and Bokeh only save JPEG")
        }

        val manualExposure = params["exposure_ns"]?.toLongOrNull()
        val manualIso = params["iso"]?.toIntOrNull()
        val bracket = params["bracket_ev"]?.takeIf { it.isNotBlank() }?.split(',')?.map {
            it.trim().toDoubleOrNull()?.takeIf { ev -> ev.isFinite() && abs(ev) <= MAX_EV }
                ?: return refuse("bad_option", "Bracketing steps are EV numbers between -6 and 6")
        }
        val focusSteps = params["focus_steps"]?.toIntOrNull() ?: 0
        if (extension != null && (manualExposure != null || bracket != null || focusSteps > 0))
            return refuse("unsupported", "Night, HDR and Bokeh set their own exposure and focus")

        var exposures: List<Pair<Long, Int>?> = listOf(null)
        if (manualExposure != null || manualIso != null || bracket != null) {
            if (!caps.manualSensor) return refuse("unsupported", "This lens has no manual exposure")
            val start = base ?: return refuse("not_ready", "The camera hasn't measured the light yet")
            val exposure = fitExposure(manualExposure ?: start.exposureNs, caps)
            val iso = (manualIso ?: start.iso).coerceIn(caps.isoMin, caps.isoMax)
            exposures = (bracket ?: listOf(0.0)).also {
                if (it.isEmpty() || it.size > MAX_BRACKET) return refuse("bad_option", "Bracket 1 to $MAX_BRACKET pictures")
            }.map { ev -> bracketed(exposure, iso, ev, caps) }
        }

        var focuses: List<Float?> = listOf(null)
        if (focusSteps > 0) {
            if (!caps.manualFocus) return refuse("unsupported", "This lens has no manual focus")
            if (focusSteps !in 2..MAX_FOCUS_STEPS) return refuse("bad_option", "Focus bracketing takes 2 to $MAX_FOCUS_STEPS steps")
            val from = params["focus_from"]?.toFloatOrNull()?.takeIf { it.isFinite() } ?: caps.minFocusDistance
            val to = params["focus_to"]?.toFloatOrNull()?.takeIf { it.isFinite() } ?: 0f
            focuses = focusSteps(from.coerceIn(0f, caps.minFocusDistance), to.coerceIn(0f, caps.minFocusDistance), focusSteps)
        }

        val count = params["count"]?.toIntOrNull() ?: 1
        if (count < 1) return refuse("bad_option", "Take at least one picture")
        val total = count.toLong() * exposures.size * focuses.size
        if (total > MAX_SHOTS) return refuse("bad_option", "At most $MAX_SHOTS pictures at once")
        val shots = (1..count).flatMap {
            focuses.flatMap { focus -> exposures.map { e -> StillShot(e?.first, e?.second, focus) } }
        }

        val timer = (params["timer_s"]?.toIntOrNull() ?: 0).coerceIn(0, MAX_TIMER_S)
        val rotation = params["rotation"]?.toIntOrNull()?.takeIf { it in setOf(0, 90, 180, 270) } ?: 0
        val distortion = when (params["distortion"]) { "1" -> true; "0" -> false; else -> null }
        if (distortion != null && !caps.distortion) return refuse("unsupported", "This lens can't switch distortion correction")
        return Parsed.Ok(StillRequest(
            format = format, raw = raw, quality = quality, maxRes = maxRes, extension = extension, shots = shots,
            timerMs = timer * 1000L, rotation = rotation,
            nrHq = params["nr_hq"] == "1" && caps.nrHq, edgeHq = params["edge_hq"] == "1" && caps.edgeHq,
            hotPixelHq = params["hot_pixel_hq"] == "1" && caps.hotPixelHq, distortion = distortion,
        ))
    }

    fun fitExposure(ns: Long, caps: StillCaps): Long =
        if (caps.exposureMaxNs <= 0L) ns else ns.coerceIn(caps.exposureMinNs, caps.exposureMaxNs)

    // ev stops away from exposure at iso: the exposure time moves first, then ISO where the time runs out
    fun bracketed(exposureNs: Long, iso: Int, ev: Double, caps: StillCaps): Pair<Long, Int> {
        val light = exposureNs.toDouble() * iso * 2.0.pow(ev)
        val time = fitExposure((light / iso).roundToLong(), caps)
        val needIso = (light / time).roundToLong().toInt()
        return time to needIso.coerceIn(caps.isoMin, caps.isoMax)
    }

    // A box on one sensor array moved to the same place on another (the full resolution mode's is bigger)
    fun scaleBox(box: SensorBox, from: SensorBox, to: SensorBox): SensorBox {
        val sx = to.width.toDouble() / from.width
        val sy = to.height.toDouble() / from.height
        return SensorBox(to.left + ((box.left - from.left) * sx).roundToLong().toInt(),
            to.top + ((box.top - from.top) * sy).roundToLong().toInt(),
            (box.width * sx).roundToLong().toInt(), (box.height * sy).roundToLong().toInt())
    }

    // count focus distances from from to to diopters, evenly spaced (diopters are already 1/distance)
    fun focusSteps(from: Float, to: Float, count: Int): List<Float> =
        (0 until count).map { from + (to - from) * it / (count - 1) }
}
