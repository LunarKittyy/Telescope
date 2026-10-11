package com.telescope

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

class DynamicFrameRateTest {

    private val w = 1920
    private val h = 1080

    // Feeds the same bitrate every 250 ms for seconds; returns every rate it stepped to, in order.
    private var now = 0L
    private fun DynamicFrameRate.hold(mbps: Double, seconds: Int): List<Int> {
        val steps = mutableListOf<Int>()
        repeat(seconds * 4) {
            now += 250
            update(now, (mbps * 1_000_000).toInt(), w, h)?.let { steps += it }
        }
        return steps
    }

    @Test
    fun `steps down through the usual rates and never under 15`() {
        assertEquals(listOf(60, 30, 24, 20, 15), DynamicFrameRate(60).rungs)
        assertEquals(listOf(30, 24, 20, 15), DynamicFrameRate(30).rungs)
        assertEquals(listOf(24, 20, 15), DynamicFrameRate(24).rungs)
        assertEquals(listOf(15), DynamicFrameRate(15).rungs)
        assertEquals(listOf(10), DynamicFrameRate(10).rungs)
    }

    @Test
    fun `only rates the camera can be held to are rungs`() {
        assertEquals(listOf(30, 24, 15), DynamicFrameRate(30) { it != 20 }.rungs)
        assertEquals(listOf(30), DynamicFrameRate(30) { false }.rungs)
    }

    @Test
    fun `a thin bitrate right after a stall isn't a slow link`() {
        val r = DynamicFrameRate(30)
        val stall = now
        val steps = mutableListOf<Int>()
        repeat(14 * 4) {  // DynamicBitrate's floor and climb back after the stall
            now += 250
            r.update(now, 500_000, w, h, lastStallMs = stall)?.let { steps += it }
        }
        assertEquals(emptyList<Int>(), steps)
        assertEquals(listOf(24), r.hold(0.5, 5))  // still thin well after it: that's the link
    }

    @Test
    fun `a link that carries Auto's bitrate keeps the rate asked for`() {
        val r = DynamicFrameRate(30)
        assertEquals(emptyList<Int>(), r.hold(8.0, 120))
        assertEquals(30, r.fps)
        assertNull(r.cap)
    }

    @Test
    fun `a thin link steps down one rung at a time and stops where the bitrate is enough`() {
        val r = DynamicFrameRate(30)
        // Auto at 1080p30 is about 8 Mbps; 2 Mbps is a quarter of it. 20 fps is the first rung it's enough for.
        assertEquals(listOf(24, 20), r.hold(2.0, 60))
        assertEquals(20, r.cap)
    }

    @Test
    fun `waits before stepping, and between steps`() {
        val r = DynamicFrameRate(30)
        assertEquals(emptyList<Int>(), r.hold(1.0, 3))  // under DOWN_MS of a thin link
        assertEquals(listOf(24), r.hold(1.0, 2))
        assertEquals(emptyList<Int>(), r.hold(1.0, 9))  // settling after the step
        assertEquals(listOf(20), r.hold(1.0, 2))
    }

    @Test
    fun `a short dip doesn't count`() {
        val r = DynamicFrameRate(30)
        repeat(10) {
            r.hold(1.0, 3)
            r.hold(8.0, 1)
        }
        assertNull(r.cap)
    }

    @Test
    fun `climbs back once the link has room, more slowly than it came down`() {
        val r = DynamicFrameRate(30)
        r.hold(1.0, 60)
        assertEquals(15, r.fps)
        // Room for 20 but not yet for 24 (60% of Auto at 24 is about 3.9 Mbps)
        assertEquals(listOf(20), r.hold(3.5, 40))
        assertEquals(listOf(24, 30), r.hold(8.0, 60))
        assertNull(r.cap)
    }

    @Test
    fun `doesn't flap around a threshold`() {
        val r = DynamicFrameRate(30)
        var steps = 0
        // Wobbling either side of 35% of Auto at 30 fps, every other second
        repeat(60) { i -> steps += r.hold(if (i % 2 == 0) 2.6 else 3.1, 1).size }
        assertTrue(steps <= 1, "$steps steps")
    }

    @Test
    fun `on a slow link with the real Dynamic bitrate it ends at fewer, fuller frames`() {
        val fps = 30
        val dyn = DynamicBitrate(H264Stream.defaultBitrate(w, h, fps), H264Stream.dynamicCeiling(w, h, fps))
        val rate = DynamicFrameRate(fps)
        val linkBps = 1_800_000.0
        var backlogBits = 0.0
        var t = 0L
        var queueMs = 0L
        repeat(4 * 90) {
            t += 250
            // The encoder makes about what it's asked for; the link takes what it can of the backlog
            val encoded = dyn.bitrate * 0.25 * 0.9
            backlogBits += encoded
            val sent = minOf(backlogBits, linkBps * 0.25)
            backlogBits -= sent
            queueMs = (backlogBits / linkBps * 1000).toLong()
            dyn.update(DynamicBitrate.Sample(t, queueMs, (sent / 8).toLong(), (encoded / 8).toLong()))
            rate.update(t, dyn.bitrate, w, h)
        }
        assertTrue(rate.fps < fps, "still at ${rate.fps} fps with ${dyn.bitrate} bps")
        assertTrue(queueMs < 200, "video still queueing: $queueMs ms")
    }
}
