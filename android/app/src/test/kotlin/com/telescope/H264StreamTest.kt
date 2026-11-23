package com.telescope

import org.junit.jupiter.api.Assertions.assertArrayEquals
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import java.nio.ByteBuffer

class H264StreamTest {

    private fun p(n: Int) = byteArrayOf(n.toByte())

    @Test
    fun `default bitrate is about 8 Mbps at 1080p30 and clamped`() {
        assertEquals(8_087_040, H264Stream.defaultBitrate(1920, 1080, 30))
        assertEquals(1_000_000, H264Stream.defaultBitrate(320, 240, 5))
        assertEquals(30_000_000, H264Stream.defaultBitrate(3840, 2160, 60))  // Auto stays at most 30
    }

    @Test
    fun `a requested bitrate wins over the default and is clamped`() {
        assertEquals(5_000_000, H264Stream.bitrateFor(5_000_000, 1920, 1080, 30))
        assertEquals(1_000_000, H264Stream.bitrateFor(10, 1920, 1080, 30))
        assertEquals(80_000_000, H264Stream.bitrateFor(80_000_000, 3840, 2160, 30))
        assertEquals(100_000_000, H264Stream.bitrateFor(500_000_000, 3840, 2160, 30))
        assertEquals(H264Stream.defaultBitrate(1280, 720, 30), H264Stream.bitrateFor(0, 1280, 720, 30))
    }

    @Test
    fun `each frame is followed by a delimiter`() {
        assertArrayEquals(byteArrayOf(7, 0, 0, 0, 1, 0x09, 0xF0.toByte()), H264Stream.packet(ByteBuffer.wrap(p(7)), null, 0))
    }

    @Test
    fun `a new viewer starts at a keyframe with the config in front`() {
        val q = H264ClientQueue()
        q.offerConfig(p(9))
        assertFalse(q.offer(p(1), key = false))
        assertEquals(0, q.size())
        q.offer(p(2), key = true)
        q.offer(p(3), key = false)
        assertArrayEquals(p(9), q.poll(0))
        assertArrayEquals(p(2), q.poll(0))
        assertArrayEquals(p(3), q.poll(0))
        assertNull(q.poll(0))
    }

    @Test
    fun `an overflowing backlog is dropped and asks for a keyframe`() {
        val q = H264ClientQueue(capacity = 3)
        q.offer(p(1), key = true)
        q.offer(p(2), key = false)
        q.offer(p(3), key = false)
        assertTrue(q.offer(p(4), key = false))
        assertEquals(0, q.size())
        assertFalse(q.offer(p(5), key = false))
        q.offer(p(6), key = true)
        assertArrayEquals(p(6), q.poll(0))
    }

    @Test
    fun `an overflowing keyframe is kept, with the config in front`() {
        val q = H264ClientQueue(capacity = 3)
        q.offerConfig(p(9))
        q.offer(p(1), key = true)
        q.offer(p(2), key = false)
        q.offer(p(3), key = false)
        assertFalse(q.offer(p(4), key = true))
        assertArrayEquals(p(9), q.poll(0))
        assertArrayEquals(p(4), q.poll(0))
        assertNull(q.poll(0))
    }

    @Test
    fun `a new config replaces the backlog and waits for the next keyframe`() {
        val q = H264ClientQueue()
        q.offer(p(1), key = true)
        q.offerConfig(p(8))
        assertEquals(0, q.size())
        q.offer(p(2), key = false)
        q.offer(p(3), key = true)
        assertArrayEquals(p(8), q.poll(0))
        assertArrayEquals(p(3), q.poll(0))
    }

    @Test
    fun `the queue measures the shortest wait, not a burst that clears`() {
        var now = 0L
        val q = H264ClientQueue(clock = { now })
        q.offer(p(1), key = true)
        q.offer(p(2), key = false)
        now = 100
        q.poll(0); q.written(1, done = true)  // waited 100 ms: a keyframe's burst
        q.offer(p(3), key = false)
        now = 105
        q.poll(0); q.written(1, done = true)  // waited 105
        q.poll(0); q.written(1, done = true)  // waited 5: the queue emptied
        assertEquals(5, q.takeQueueMs())
        assertEquals(3, q.takeSentBytes())
        assertEquals(0, q.takeSentBytes())
    }

    @Test
    fun `with nothing going out the wait is the oldest packet's or the stuck write's`() {
        var now = 0L
        val q = H264ClientQueue(clock = { now })
        assertEquals(0, q.takeQueueMs())  // idle
        q.offer(p(1), key = true)
        q.poll(0)
        q.written(1, done = false)  // part of it went out, then the write got stuck
        q.offer(p(2), key = false)
        now = 50
        q.takeQueueMs()             // the poll above counts once
        now = 300
        assertEquals(300, q.takeQueueMs())
        q.written(0, done = true)
        q.poll(0); q.written(1, done = true)
        now = 400
        assertEquals(300, q.takeQueueMs())  // p(2) waited 300 before going out
    }

    @Test
    fun `a backlog older than its limit overflows like a full one`() {
        var now = 0L
        val q = H264ClientQueue(clock = { now })
        q.offer(p(1), key = true)
        now = 1_500
        q.maxWaitMs = Long.MAX_VALUE
        assertFalse(q.offer(p(2), key = false))  // no limit: kept
        q.maxWaitMs = 1_000
        assertTrue(q.offer(p(3), key = false))   // over it: dropped, and a keyframe asked for
        assertEquals(0, q.size())
    }

    private class Catchup(val maxAgeMs: Long, val firstIsKey: Boolean, val keyAsks: Int, val delivered: Int)

    // 30 fps with a keyframe a second, a viewer that reads nothing for stallMs and then everything at once
    private fun catchUp(maxWaitMs: Long?, stallMs: Long): Catchup {
        var now = 0L
        val q = H264ClientQueue(clock = { now })
        if (maxWaitMs != null) q.maxWaitMs = maxWaitMs
        val offeredAt = HashMap<Int, Long>()
        val keys = HashSet<Int>()
        var wantKey = false
        var asks = 0
        var maxAge = 0L
        var first: Boolean? = null
        var delivered = 0
        var frame = 0
        while (now < stallMs + 2_000) {
            if (now % 33 == 0L) {
                val key = wantKey || frame % 30 == 0
                wantKey = false
                offeredAt[frame] = now
                if (key) keys.add(frame)
                if (q.offer(byteArrayOf((frame shr 8).toByte(), frame.toByte()), key)) { asks++; wantKey = true }
                frame++
            }
            if (now >= stallMs) {
                while (true) {
                    val packet = q.poll(0) ?: break
                    val n = ((packet[0].toInt() and 0xFF) shl 8) or (packet[1].toInt() and 0xFF)
                    if (first == null) first = n in keys
                    maxAge = maxOf(maxAge, now - offeredAt.getValue(n))
                    delivered++
                    q.written(packet.size, done = true)
                }
            }
            now++
        }
        return Catchup(maxAge, first == true, asks, delivered)
    }

    @Test
    fun `a viewer that stalls and catches up sees at most the latency budget of old video in every mode`() {
        val old = catchUp(Long.MAX_VALUE, 2_500)  // what Auto and a fixed bitrate did: no age limit, only 90 packets
        val now = catchUp(null, 2_500)
        println("catch-up after a 2.5 s stall: oldest frame shown %d ms before, %d ms now; keyframe asks %d -> %d; delivered %d -> %d"
            .format(old.maxAgeMs, now.maxAgeMs, old.keyAsks, now.keyAsks, old.delivered, now.delivered))
        assertTrue(old.maxAgeMs >= 2_400)
        assertTrue(now.maxAgeMs <= H264Stream.LATENCY_BUDGET_MS + 33, "oldest frame shown was ${now.maxAgeMs} ms old")
        assertTrue(now.firstIsKey, "recovery starts at a keyframe")
        assertTrue(now.keyAsks >= 1)
        assertTrue(now.delivered > 0)
    }

    @Test
    fun `a keyframe that takes a while to go out doesn't count as lag`() {
        var now = 0L
        val q = H264ClientQueue(clock = { now })
        q.maxWaitMs = 1_000
        q.offer(p(1), key = true)
        q.poll(0)                                // the keyframe starts going out
        q.offer(p(2), key = false)
        now = 1_500
        assertFalse(q.offer(p(3), key = false))  // still sending it: kept
        q.written(1, done = true)
        now = 2_400
        assertFalse(q.offer(p(4), key = false))  // 900 ms since it went: kept
        now = 2_600
        assertTrue(q.offer(p(5), key = false))   // the rest still waiting after that: real lag
    }

    private fun hex(vararg v: Int) = ByteArray(v.size) { v[it].toByte() }
    private val seiHead = hex(0, 0, 0, 1, 0x06, 0x05, 25, 0x54, 0x45, 0x4C, 0x45, 0x53, 0x43, 0x4F, 0x50, 0x45, 0x2D, 0x54, 0x49, 0x4D, 0x45, 0x31, 0x31)

    @Test
    fun `capture time SEI has the exact bytes for a plain value`() {
        val sei = H264Stream.captureTimeSei(false, 0x0102030405060708L)
        assertArrayEquals(seiHead + hex(1, 1, 2, 3, 4, 5, 6, 7, 8, 0x80), sei)
    }

    @Test
    fun `capture time SEI escapes zero runs in the payload`() {
        val sei = H264Stream.captureTimeSei(true, 0x0000000300000000L)
        assertArrayEquals(seiHead + hex(0, 0, 3, 0, 0, 3, 3, 0, 0, 3, 0, 0, 0x80), sei)
        // Removing emulation prevention gives the clock byte and time back
        val body = sei.copyOfRange(seiHead.size, sei.size - 1)
        val raw = ArrayList<Byte>()
        var zeros = 0
        for (b in body) {
            if (zeros >= 2 && b.toInt() == 3) { zeros = 0; continue }
            raw.add(b)
            zeros = if (b.toInt() == 0) zeros + 1 else 0
        }
        assertArrayEquals(hex(0, 0, 0, 0, 3, 0, 0, 0, 0), raw.toByteArray())
        // No start-code lookalike inside the escaped payload
        for (i in 0 until body.size - 2) assertFalse(body[i].toInt() == 0 && body[i + 1].toInt() == 0 && body[i + 2].toInt() in 0..2)
    }

    @Test
    fun `building a packet matches the old capture time then delimiter layout`() {
        val codec = hex(0, 0, 0, 1, 0x65, 0x88, 0x84, 0, 0, 1, 9)
        for (realtime in listOf(true, false, null)) {
            for (timeNs in listOf(0L, 5_000L, 0x0102030405060708L)) {
                val old = if (realtime == null) codec else H264Stream.captureTimeSei(realtime, timeNs) + codec
                val expected = old + H264Stream.AUD
                val array = ByteBuffer.wrap(codec)
                assertArrayEquals(expected, H264Stream.packet(array, realtime, timeNs))
                assertEquals(array.limit(), array.position())
                val direct = ByteBuffer.allocateDirect(codec.size + 8)
                direct.position(3)
                direct.put(codec)
                direct.limit(3 + codec.size)
                direct.position(3)
                assertArrayEquals(expected, H264Stream.packet(direct, realtime, timeNs))
                assertEquals(direct.limit(), direct.position())
            }
        }
        assertEquals(0, H264Stream.captureTimeSei(null, 1).size)
    }

    @Test
    fun `a crop goes after the time in the same SEI, escaped, and only with a time`() {
        val sei = H264Stream.captureTimeSei(false, 0x0102030405060708L, floatArrayOf(1f, 2f, 0.5f, 0f))
        val crop = hex(0x05, 32) + "TELESCOPE-CROP11".toByteArray(Charsets.US_ASCII) +
            // 1.0, 2.0, 0.5 and 0.0 as floats, with an 03 after each 00 00 that a 00 follows
            hex(0x3F, 0x80, 0, 0, 0x40, 0, 0, 3, 0, 0x3F, 0, 0, 3, 0, 0, 3, 0, 0, 3, 0, 0x80)
        assertArrayEquals(seiHead + hex(1, 1, 2, 3, 4, 5, 6, 7, 8) + crop, sei)
        assertEquals(0, H264Stream.captureTimeSei(null, 1, floatArrayOf(1f, 1f, 0.5f, 0.5f)).size)
        assertArrayEquals(H264Stream.captureTimeSei(true, 5), H264Stream.captureTimeSei(true, 5, null))
    }

    @Test
    fun `the crop log hands each frame its own crop and waits a little for a late one`() {
        val log = CropLog(capacity = 3)
        log.put(10, floatArrayOf(1f, 1f, 0.5f, 0.5f))
        log.put(20, floatArrayOf(1f, 2f, 0.5f, 0.5f))
        assertArrayEquals(floatArrayOf(1f, 2f, 0.5f, 0.5f), log.take(20, 0))
        assertNull(log.take(20, 0))
        Thread { Thread.sleep(5); log.put(30, floatArrayOf(1f, 3f, 0.5f, 0.5f)) }.start()
        assertArrayEquals(floatArrayOf(1f, 3f, 0.5f, 0.5f), log.take(30, 2_000))
        for (t in 40L..50L) log.put(t, floatArrayOf(1f, 1f, 0.5f, 0.5f))
        assertNull(log.take(10, 0), "the oldest go once it's full")
        assertTrue(log.take(50, 0) != null)
    }
}
