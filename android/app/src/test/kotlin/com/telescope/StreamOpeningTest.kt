package com.telescope

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Test

class StreamOpeningTest {
    @Test
    fun `an older computer's start asks for nothing`() {
        assertNull(StreamOpening.from(mapOf("action" to "start")))
    }

    @Test
    fun `size and rate come through`() {
        assertEquals(StreamOpening(1280, 720, 30),
            StreamOpening.from(mapOf("width" to "1280", "height" to "720", "fps" to "30")))
    }

    @Test
    fun `half a size or a nonsense one is left out, the rate kept`() {
        assertEquals(StreamOpening(null, null, 60), StreamOpening.from(mapOf("width" to "1280", "fps" to "60")))
        assertEquals(StreamOpening(null, null, 60),
            StreamOpening.from(mapOf("width" to "0", "height" to "720", "fps" to "60")))
        assertEquals(StreamOpening(null, null, 60),
            StreamOpening.from(mapOf("width" to "99999", "height" to "720", "fps" to "60")))
    }

    @Test
    fun `a rate is kept to what the phone takes`() {
        assertEquals(StreamOpening(1280, 720, 120),
            StreamOpening.from(mapOf("width" to "1280", "height" to "720", "fps" to "500")))
        assertEquals(StreamOpening(1280, 720, null),
            StreamOpening.from(mapOf("width" to "1280", "height" to "720", "fps" to "0")))
    }
}
