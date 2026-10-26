package com.telescope

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

class UdpLinkTest {
    private fun hex(s: String) = s.chunked(2).map { it.toInt(16).toByte() }.toByteArray()

    private fun report(received: Long, highest: Long, bytes: Long = 0, clockMs: Long = 0, frame: Int = -1,
                       sinceMs: Int = 0, gaveUp: Int = 0) = UdpReport(received, highest, bytes, clockMs, frame, sinceMs, gaveUp)

    @Test
    fun `reads the report exactly as the desktop packs it`() {
        // The same values are in the desktop's test_udp_reader.py
        assertEquals(report(5, 6, 40, 2250, 0, 250, 0), UdpReport.parse(hex("04000000050000000600000028000008ca0000000000fa0000")))
        val wrapped = UdpReport.parse(hex("04ffffffff" + "ffffffff" + "00000001" + "00000002" + "ffffffff" + "ffff" + "ffff"))!!
        assertEquals(0xFFFFFFFFL, wrapped.received)
        assertEquals(-1, wrapped.frame)
        assertEquals(65535, wrapped.gaveUp)
        assertTrue(UdpReport.parse(hex("04000000050000000600000028000008ca0000000000fa0000" + "abcd")) != null)  // added fields
        assertNull(UdpReport.parse(hex("04000000050000000600000028000008ca0000000000fa00")))  // short
        assertNull(UdpReport.parse(hex("03000000050000000600000028000008ca0000000000fa0000")))  // another kind
    }

    @Test
    fun `nothing until a desktop reports, and the first report only starts the count`() {
        val link = UdpLink()
        assertNull(link.take(0, 0))
        link.report(report(0, 0), 10)
        assertTrue(link.reporting)
        assertNull(link.take(250, 0))
        assertNull(link.take(500, 0))  // no report since
    }

    @Test
    fun `loss is what the counters skipped, wrapping at 32 bits`() {
        val link = UdpLink()
        link.report(report(0xFFFFFF00L, 0xFFFFFF00L), 0)
        link.take(0, 0)
        link.report(report(0xFFFFFF00L + 270 - 0x100000000L, 0xFFFFFF00L + 300 - 0x100000000L), 100)
        assertEquals(0.1, link.take(250, 0)!!.loss, 1e-9)
        link.report(report(0xFFFFFF00L + 10, 0xFFFFFF00L + 20), 300)  // an older one, arriving late
        link.report(report(0xFFFFFF00L + 570 - 0x100000000L, 0xFFFFFF00L + 600 - 0x100000000L), 400)
        assertEquals(0.0, link.take(500, 0)!!.loss, 1e-9)
    }

    @Test
    fun `a frame given up on counts as loss even when the counters say little`() {
        val link = UdpLink()
        link.report(report(0, 0), 0)
        link.take(0, 0)
        link.report(report(100, 100, gaveUp = 1), 100)
        assertEquals(UdpLink.GAVE_UP_LOSS, link.take(250, 0)!!.loss, 1e-9)
    }

    @Test
    fun `bytes that got through follow the desktop's clock`() {
        val link = UdpLink()
        link.report(report(0, 0, bytes = 0, clockMs = 5_000), 0)
        link.take(0, 0)
        // 100 KB over 200 ms of the desktop's time, taken 250 ms apart on the phone's: 125 KB went through
        link.report(report(100, 100, bytes = 100_000, clockMs = 5_200), 200)
        assertEquals(125_000L, link.take(250, 0)!!.sentBytes)
    }

    @Test
    fun `a trip longer than the quickest lately is a queue on the way`() {
        val link = UdpLink()
        link.sent(1, 0, 0)
        link.report(report(10, 10, frame = 1, sinceMs = 5), 25)  // 20 ms there and back: the quickest so far
        link.take(25, 0)
        link.sent(2, 30, 33)
        link.report(report(20, 20, frame = 2, sinceMs = 10), 133)  // 90 ms
        assertEquals(70L, link.take(250, 0)!!.queueMs)
        link.sent(3, 300, 300)
        link.report(report(30, 30, frame = 3), 400)
        link.sent(4, 400, 400)
        link.report(report(40, 40, frame = 4), 425)  // the shortest in a sample counts, like H264ClientQueue's
        assertEquals(5L, link.take(500, 0)!!.queueMs)
        link.sent(5, 20_000, 20_000)
        link.report(report(50, 50, frame = 5), 20_050)  // the quick trips were too long ago: this is the usual now
        assertEquals(0L, link.take(20_100, 0)!!.queueMs)
    }

    @Test
    fun `frames waiting on the phone are a queue too`() {
        val link = UdpLink()
        link.report(report(0, 0), 0)
        link.take(0, 0)
        link.sent(1, 100, 180)
        link.report(report(10, 10), 200)
        assertEquals(80L, link.take(250, 0)!!.queueMs)
        link.report(report(20, 20), 300)
        assertEquals(400L, link.take(500, waitingMs = 400)!!.queueMs)  // stuck: nothing went out at all
    }
}
