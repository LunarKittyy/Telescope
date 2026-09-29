package com.telescope

import org.junit.jupiter.api.Test
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertTrue

class H264EncoderTest {
    @Test
    fun `a refused setup is retried with fewer extras, down to none`() {
        val attempts = H264Encoder.ATTEMPTS
        assertEquals(H264Encoder.Extra.values().toSet(), attempts.first())
        assertTrue(attempts.last().isEmpty())
        attempts.zipWithNext().forEach { (a, b) -> assertTrue(a.containsAll(b) && a != b) }
    }
}
