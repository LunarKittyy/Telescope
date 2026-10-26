package com.telescope

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

class StillOptionsTest {
    // An emulator's camera: JPEG and nothing else
    private val plain = StillCaps(jpeg = 1920 to 1080)

    // A flagship's main camera
    private val full = StillCaps(
        jpeg = 4080 to 3072, heic = 4080 to 3072, ultraHdr = 4080 to 3072, raw = 4080 to 3072,
        maxResJpeg = 8160 to 6144, maxResRaw = 8160 to 6144, extensions = listOf("night", "hdr", "bokeh"),
        extensionSizes = mapOf("night" to (3840 to 2160), "hdr" to (4080 to 3072), "bokeh" to (4080 to 3072)),
        manualSensor = true, manualFocus = true, exposureMinNs = 10_000L, exposureMaxNs = 30_000_000_000L,
        isoMin = 50, isoMax = 6400, minFocusDistance = 10f, nrHq = true, edgeHq = true, hotPixelHq = true,
        distortion = true,
    )
    private val base = StillExposure(10_000_000L, 200)

    private fun ok(params: Map<String, String>, caps: StillCaps = full, exposure: StillExposure? = base): StillRequest {
        val parsed = StillOptions.parse(params, caps, exposure)
        assertTrue(parsed is StillOptions.Parsed.Ok, "refused: $parsed")
        return (parsed as StillOptions.Parsed.Ok).request
    }

    private fun refused(params: Map<String, String>, caps: StillCaps = full, exposure: StillExposure? = base): String {
        val parsed = StillOptions.parse(params, caps, exposure)
        assertTrue(parsed is StillOptions.Parsed.Refused, "taken: $parsed")
        return (parsed as StillOptions.Parsed.Refused).code
    }

    @Test
    fun `a plain JPEG works on any lens and goes next to the stream`() {
        val req = ok(mapOf("format" to "jpeg", "quality" to "90"), plain, null)
        assertEquals("jpeg", req.format)
        assertFalse(req.raw)
        assertEquals(90, req.quality)
        assertEquals(1, req.shots.size)
        assertFalse(req.ownSession)
        assertEquals(StillShot(), req.shots.single())
    }

    @Test
    fun `quality is fitted to 1 to 100 and defaults to 95`() {
        assertEquals(100, ok(mapOf("format" to "jpeg", "quality" to "500"), plain).quality)
        assertEquals(95, ok(mapOf("format" to "jpeg"), plain).quality)
    }

    @Test
    fun `formats the lens doesn't have are refused`() {
        assertEquals("unsupported", refused(mapOf("format" to "heic"), plain))
        assertEquals("unsupported", refused(mapOf("format" to "ultrahdr"), plain))
        assertEquals("unsupported", refused(mapOf("format" to "jpeg", "raw" to "1"), plain))
        assertEquals("unsupported", refused(mapOf("format" to "jpeg", "max_res" to "1"), plain))
        assertEquals("unsupported", refused(mapOf("format" to "jpeg", "extension" to "night"), plain))
        assertEquals("bad_option", refused(mapOf("format" to "png"), full))
        assertEquals("bad_option", refused(emptyMap(), full))
    }

    @Test
    fun `RAW on its own and RAW with a JPEG both make files per shot`() {
        val rawOnly = ok(mapOf("raw" to "1"))
        assertNull(rawOnly.format)
        assertEquals(1, rawOnly.filesPerShot)
        assertEquals(2, ok(mapOf("format" to "jpeg", "raw" to "1")).filesPerShot)
    }

    @Test
    fun `full resolution takes JPEG and RAW only where the lens has them`() {
        assertTrue(ok(mapOf("format" to "jpeg", "raw" to "1", "max_res" to "1")).maxRes)
        assertEquals("unsupported", refused(mapOf("format" to "heic", "max_res" to "1")))
        assertEquals("unsupported", refused(mapOf("raw" to "1", "max_res" to "1"), full.copy(maxResRaw = null)))
    }

    @Test
    fun `the maker's modes need their own session and only save JPEG`() {
        val req = ok(mapOf("format" to "jpeg", "extension" to "night", "count" to "2"))
        assertEquals("night", req.extension)
        assertTrue(req.ownSession)
        assertEquals(2, req.shots.size)
        assertEquals("unsupported", refused(mapOf("format" to "jpeg", "extension" to "auto")))
        assertEquals("unsupported", refused(mapOf("format" to "jpeg", "raw" to "1", "extension" to "hdr")))
        assertEquals("unsupported", refused(mapOf("format" to "jpeg", "extension" to "hdr", "bracket_ev" to "-1,1")))
        assertEquals("unsupported", refused(mapOf("format" to "jpeg", "extension" to "hdr", "exposure_ns" to "1000000")))
    }

    @Test
    fun `a long exposure is fitted to the sensor and keeps the stream's ISO`() {
        val shot = ok(mapOf("format" to "jpeg", "exposure_ns" to "60000000000")).shots.single()
        assertEquals(30_000_000_000L, shot.exposureNs)
        assertEquals(200, shot.iso)
        assertEquals(800, ok(mapOf("format" to "jpeg", "exposure_ns" to "1000000000", "iso" to "800")).shots.single().iso)
        assertEquals(1_000L, ok(mapOf("format" to "jpeg", "exposure_ns" to "1000000000")).exposureMs)
    }

    @Test
    fun `manual exposure needs a lens that has it, and a light reading to start from`() {
        assertEquals("unsupported", refused(mapOf("format" to "jpeg", "exposure_ns" to "1000"), full.copy(manualSensor = false)))
        assertEquals("unsupported", refused(mapOf("format" to "jpeg", "bracket_ev" to "-1,1"), full.copy(manualSensor = false)))
        assertEquals("not_ready", refused(mapOf("format" to "jpeg", "bracket_ev" to "-1,1"), full, null))
    }

    @Test
    fun `bracketing steps the exposure time from the stream's exposure`() {
        val shots = ok(mapOf("format" to "jpeg", "bracket_ev" to "-2, 0, 2")).shots
        assertEquals(listOf(2_500_000L, 10_000_000L, 40_000_000L), shots.map { it.exposureNs })
        assertEquals(listOf(200, 200, 200), shots.map { it.iso })
    }

    @Test
    fun `bracketing moves ISO once the exposure time runs out`() {
        val caps = full.copy(exposureMaxNs = 20_000_000L, exposureMinNs = 5_000_000L)
        val (longTime, highIso) = StillOptions.bracketed(10_000_000L, 200, 2.0, caps)
        assertEquals(20_000_000L, longTime)
        assertEquals(400, highIso)
        val (shortTime, lowIso) = StillOptions.bracketed(10_000_000L, 200, -2.0, caps)
        assertEquals(5_000_000L, shortTime)
        assertEquals(100, lowIso)
        assertEquals(50, StillOptions.bracketed(10_000_000L, 100, -4.0, caps).second)  // ISO's floor
    }

    @Test
    fun `bracket steps outside 6 EV or past 9 pictures are refused`() {
        assertEquals("bad_option", refused(mapOf("format" to "jpeg", "bracket_ev" to "-7,0")))
        assertEquals("bad_option", refused(mapOf("format" to "jpeg", "bracket_ev" to "a,b")))
        assertEquals("bad_option", refused(mapOf("format" to "jpeg", "bracket_ev" to (1..10).joinToString(","))))
    }

    @Test
    fun `focus bracketing spreads the steps from near to far`() {
        val shots = ok(mapOf("format" to "jpeg", "focus_steps" to "5", "focus_from" to "8", "focus_to" to "0")).shots
        assertEquals(listOf(8f, 6f, 4f, 2f, 0f), shots.map { it.focus })
        assertEquals(listOf(10f, 0f), ok(mapOf("format" to "jpeg", "focus_steps" to "2")).shots.map { it.focus })
        assertEquals(10f, ok(mapOf("format" to "jpeg", "focus_steps" to "2", "focus_from" to "99")).shots.first().focus)
        assertEquals("unsupported", refused(mapOf("format" to "jpeg", "focus_steps" to "3"), full.copy(manualFocus = false)))
        assertEquals("bad_option", refused(mapOf("format" to "jpeg", "focus_steps" to "1")))
    }

    @Test
    fun `bursts repeat every bracket, and the whole lot is capped`() {
        val shots = ok(mapOf("format" to "jpeg", "count" to "3", "bracket_ev" to "-1,1")).shots
        assertEquals(6, shots.size)
        assertEquals(30, ok(mapOf("format" to "jpeg", "count" to "30"), plain).shots.size)
        assertEquals("bad_option", refused(mapOf("format" to "jpeg", "count" to "61"), plain))
        assertEquals("bad_option", refused(mapOf("format" to "jpeg", "count" to "21", "bracket_ev" to "-1,0,1")))
        assertEquals("bad_option", refused(mapOf("format" to "jpeg", "count" to "0"), plain))
    }

    @Test
    fun `timer, rotation and the quality switches`() {
        val req = ok(mapOf("format" to "jpeg", "timer_s" to "99", "rotation" to "90", "nr_hq" to "1", "edge_hq" to "1",
            "hot_pixel_hq" to "1", "distortion" to "0"))
        assertEquals(30_000L, req.timerMs)
        assertEquals(90, req.rotation)
        assertTrue(req.nrHq && req.edgeHq && req.hotPixelHq)
        assertEquals(false, req.distortion)
        assertEquals(0, ok(mapOf("format" to "jpeg", "rotation" to "45")).rotation)
        // Asked of a lens without them: quietly left as the camera does it, except distortion, which is a promise
        val plainReq = ok(mapOf("format" to "jpeg", "nr_hq" to "1"), plain)
        assertFalse(plainReq.nrHq)
        assertNull(plainReq.distortion)
        assertEquals("unsupported", refused(mapOf("format" to "jpeg", "distortion" to "1"), plain))
    }

    @Test
    fun `the capability says only what the lens has`() {
        val emulator = plain.capability()
        assertEquals(listOf("jpeg"), emulator.formats)
        assertEquals(0, emulator.maxResWidth)
        assertEquals(0L, emulator.exposureMaxNs)
        assertFalse(emulator.bracketing || emulator.focusBracketing || emulator.maxResRaw)
        assertTrue(emulator.extensions.isEmpty())

        val flagship = full.capability()
        assertEquals(listOf("jpeg", "heic", "ultrahdr", "raw"), flagship.formats)
        assertEquals(8160, flagship.maxResWidth)
        assertTrue(flagship.maxResRaw)
        assertEquals(30_000_000_000L, flagship.exposureMaxNs)
        assertTrue(flagship.bracketing && flagship.focusBracketing && flagship.distortionCorrection)
        assertEquals(StillOptions.MAX_SHOTS, flagship.burstMax)
        // Night takes its photos smaller than the lens's own JPEG
        assertEquals(StillSize(3840, 2160), flagship.extensionSizes["night"])
        assertEquals(4080, flagship.width)
        assertTrue(emulator.extensionSizes.isEmpty())
    }

    @Test
    fun `a long exposure next to the stream counts as pausing the video`() {
        assertFalse(ok(mapOf("format" to "jpeg")).pausesVideo)
        assertFalse(ok(mapOf("format" to "jpeg", "bracket_ev" to "-2,0,2")).pausesVideo)
        assertTrue(ok(mapOf("format" to "jpeg", "exposure_ns" to "4000000000")).pausesVideo)
        // Short ones add up in a burst
        assertTrue(ok(mapOf("format" to "jpeg", "exposure_ns" to "100000000", "count" to "6")).pausesVideo)
        assertTrue(ok(mapOf("format" to "jpeg", "extension" to "night")).pausesVideo)
    }

    @Test
    fun `crop and metering boxes move onto the full resolution array`() {
        val array = SensorBox(0, 0, 4080, 3072)
        val full = SensorBox(0, 0, 8160, 6144)
        assertEquals(SensorBox(2040, 1536, 4080, 3072), StillOptions.scaleBox(SensorBox(1020, 768, 2040, 1536), array, full))
        assertEquals(full, StillOptions.scaleBox(array, array, full))
        // Arrays that don't start at 0
        assertEquals(SensorBox(16, 8, 200, 100), StillOptions.scaleBox(SensorBox(8, 4, 100, 50), SensorBox(0, 0, 100, 50), SensorBox(0, 0, 200, 100)))
    }

    @Test
    fun `a single photo gets a bounded time, plus the exposure of a long one`() {
        val plainShot = ok(mapOf("format" to "heic", "nr_hq" to "1", "edge_hq" to "1", "hot_pixel_hq" to "1"))
        assertEquals(StillTiming.SETUP_MS + StillTiming.SHOT_MS, StillTiming.limitMs(plainShot))
        assertTrue(StillTiming.limitMs(plainShot) <= 20_000L)
        val long = ok(mapOf("format" to "jpeg", "exposure_ns" to "8000000000"))
        assertEquals(StillTiming.SETUP_MS + 8_000L + StillTiming.SHOT_MS, StillTiming.limitMs(long))
        val burst = ok(mapOf("format" to "jpeg", "count" to "10"))
        assertEquals(StillTiming.SETUP_MS + 10 * StillTiming.SHOT_MS, StillTiming.limitMs(burst))
    }

    @Test
    fun `a photo stalls after a while without news, a long exposure later`() {
        val plainShot = ok(mapOf("format" to "heic"))
        assertFalse(StillTiming.stalled(1_000L + StillTiming.STALL_MS, 1_000L, plainShot))
        assertTrue(StillTiming.stalled(1_001L + StillTiming.STALL_MS, 1_000L, plainShot))
        val long = ok(mapOf("format" to "jpeg", "bracket_ev" to "-1,0,1", "exposure_ns" to "4000000000"))
        assertEquals(8_000L, long.longestExposureMs)  // +1 EV of 4 s
        assertFalse(StillTiming.stalled(StillTiming.STALL_MS + 8_000L, 0L, long))
        assertTrue(StillTiming.stalled(StillTiming.STALL_MS + 8_001L, 0L, long))
    }

    @Test
    fun `the best quality modes can be dropped for a second go, which the backstop leaves room for`() {
        val best = ok(mapOf("format" to "heic", "nr_hq" to "1", "edge_hq" to "1", "hot_pixel_hq" to "1"))
        assertTrue(best.hq)
        val without = best.withoutHq()
        assertFalse(without.hq)
        assertEquals(best.format, without.format)
        assertEquals(best.shots, without.shots)
        assertEquals(2 * StillTiming.limitMs(best) + StillTiming.BACKSTOP_MS, StillTiming.backstopMs(best))
        assertEquals(StillTiming.limitMs(without) + StillTiming.BACKSTOP_MS, StillTiming.backstopMs(without))
        // Lenses without the modes never get them asked for
        assertFalse(ok(mapOf("format" to "jpeg", "nr_hq" to "1"), plain, null).hq)
    }
}
