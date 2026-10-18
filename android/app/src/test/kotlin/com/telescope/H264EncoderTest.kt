package com.telescope

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

class H264EncoderTest {
    private val all = H264Encoder.Extra.values().toSet()

    @Test
    fun `High comes first, then Baseline, then fewer extras, down to whatever the encoder picks`() {
        val attempts = H264Encoder.ATTEMPTS
        assertEquals(H264Encoder.Attempt(all, H264Encoder.Profile.HIGH), attempts.first())
        assertEquals(H264Encoder.Attempt(emptySet(), H264Encoder.Profile.ANY), attempts.last())
        attempts.zipWithNext().forEach { (a, b) -> assertTrue(a.extras.containsAll(b.extras) && a != b) }
    }

    @Test
    fun `every attempt with High has a Baseline one right after it`() {
        val attempts = H264Encoder.ATTEMPTS
        attempts.forEachIndexed { i, a ->
            if (a.profile == H264Encoder.Profile.HIGH) {
                assertEquals(H264Encoder.Attempt(a.extras, H264Encoder.Profile.BASELINE), attempts[i + 1])
            }
        }
    }
}
