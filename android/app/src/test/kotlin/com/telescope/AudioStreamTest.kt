package com.telescope

import org.junit.jupiter.api.Assertions.assertArrayEquals
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Test

class AudioStreamTest {

    @Test
    fun `a chunk is 10 ms of 48 kHz mono 16-bit`() {
        assertEquals(960, AudioStream.CHUNK_BYTES)
    }

    @Test
    fun `a slow listener loses its oldest audio, not the newest`() {
        val q = PcmClientQueue(capacity = 3)
        (1..5).forEach { q.offer(byteArrayOf(it.toByte())) }
        assertEquals(3, q.size())
        assertArrayEquals(byteArrayOf(3), q.poll(0))
        assertArrayEquals(byteArrayOf(4), q.poll(0))
        assertArrayEquals(byteArrayOf(5), q.poll(0))
        assertNull(q.poll(0))
    }
}
