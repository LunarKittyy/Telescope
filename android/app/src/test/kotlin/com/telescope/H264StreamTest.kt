package com.telescope

import org.junit.jupiter.api.Assertions.assertArrayEquals
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

class H264StreamTest {

    private fun p(n: Int) = byteArrayOf(n.toByte())

    @Test
    fun `default bitrate is about 8 Mbps at 1080p30 and clamped`() {
        assertEquals(8_087_040, H264Stream.defaultBitrate(1920, 1080, 30))
        assertEquals(1_000_000, H264Stream.defaultBitrate(320, 240, 5))
        assertEquals(30_000_000, H264Stream.defaultBitrate(3840, 2160, 60))
    }

    @Test
    fun `a requested bitrate wins over the default and is clamped`() {
        assertEquals(5_000_000, H264Stream.bitrateFor(5_000_000, 1920, 1080, 30))
        assertEquals(1_000_000, H264Stream.bitrateFor(10, 1920, 1080, 30))
        assertEquals(H264Stream.defaultBitrate(1280, 720, 30), H264Stream.bitrateFor(0, 1280, 720, 30))
    }

    @Test
    fun `each frame is followed by a delimiter`() {
        assertArrayEquals(byteArrayOf(7, 0, 0, 0, 1, 0x09, 0xF0.toByte()), H264Stream.withDelimiter(p(7)))
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
}
