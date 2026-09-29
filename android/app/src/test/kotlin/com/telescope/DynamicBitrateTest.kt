package com.telescope

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

class DynamicBitrateTest {

    private val auto = H264Stream.defaultBitrate(1920, 1080, 30)  // about 8 Mbps

    // A sample 250 ms after the last: the queue, and Mbps that went out and that the encoder made.
    private var now = 0L
    private fun DynamicBitrate.feed(queueMs: Long, sentMbps: Double, encodedMbps: Double): Int? {
        now += 250
        return update(DynamicBitrate.Sample(now, queueMs, (sentMbps * 1_000_000 / 8 / 4).toLong(),
            (encodedMbps * 1_000_000 / 8 / 4).toLong()))
    }

    @Test
    fun `starts at Auto's bitrate, tops out at two and a half times it`() {
        assertEquals(auto, DynamicBitrate(auto).bitrate)
        assertEquals(20_200_000, DynamicBitrate.ceiling(auto))
        assertEquals(H264Stream.MAX_BPS, DynamicBitrate.ceiling(H264Stream.defaultBitrate(3840, 2160, 60)))
    }

    @Test
    fun `climbs while the link keeps up and the encoder uses its budget`() {
        val d = DynamicBitrate(auto)
        repeat(8) { d.feed(0, 7.5, 7.5) }
        assertTrue(d.bitrate > auto)
    }

    @Test
    fun `a single slow sample isn't a full link`() {
        val d = DynamicBitrate(auto)
        repeat(4) { d.feed(0, 7.5, 7.5) }
        val before = d.bitrate
        assertNull(d.feed(100, 5.0, 7.5))  // a keyframe, say
        assertEquals(before, d.bitrate)
    }

    @Test
    fun `a queue that stays cuts to under what got through`() {
        val d = DynamicBitrate(auto)
        repeat(4) { d.feed(0, 7.5, 7.5) }
        d.feed(100, 4.0, 7.5)
        val cut = d.feed(150, 4.0, 7.5)!!
        assertTrue(cut in 3_000_000..4_500_000, "$cut")
    }

    @Test
    fun `a stall doesn't cut, and neither does the backlog draining after it`() {
        val d = DynamicBitrate(auto)
        repeat(4) { d.feed(0, 7.5, 7.5) }
        repeat(4) { assertNull(d.feed(300, 0.0, 7.5)) }  // a second with nothing going out
        assertNull(d.feed(400, 20.0, 7.5))               // back: the backlog goes out fast
        assertNull(d.feed(100, 15.0, 7.5))
        assertEquals(auto, d.bitrate)
    }

    @Test
    fun `a long stall counts as the worst link`() {
        val d = DynamicBitrate(auto)
        repeat(4) { d.feed(0, 7.5, 7.5) }
        repeat(8) { d.feed(500, 0.0, 7.5) }  // two seconds
        assertEquals(H264Stream.MIN_BPS, d.bitrate)
    }

    @Test
    fun `a viewer coming back after a while isn't a stall`() {
        val d = DynamicBitrate(auto)
        repeat(4) { d.feed(0, 7.5, 7.5) }
        now += 5_000  // the computer reconnected: the first sample covers the time nobody watched
        assertNull(d.feed(100, 0.2, 7.5))
        assertNull(d.feed(0, 7.5, 7.5))
        assertEquals(auto, d.bitrate)
    }

    @Test
    fun `a still scene doesn't climb`() {
        val d = DynamicBitrate(auto)
        repeat(40) { d.feed(0, 1.5, 1.5) }
        assertEquals(auto, d.bitrate)
    }

    @Test
    fun `a smaller size keeps the bitrate under its own ceiling`() {
        val d = DynamicBitrate(auto)
        repeat(80) { d.feed(0, d.bitrate / 1e6 * 0.9, d.bitrate / 1e6 * 0.9) }
        assertEquals(DynamicBitrate.ceiling(auto), d.bitrate)
        val small = H264Stream.defaultBitrate(1280, 720, 30)
        d.rebound(small)
        assertEquals(DynamicBitrate.ceiling(small), d.bitrate)
    }
}
