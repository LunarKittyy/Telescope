package com.telescope

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Test

// The desktop writes no look until one is picked and reads a missing or unknown one as Dark; the phone follows that rule
class LookTest {
    @Test
    fun `no saved look is the default Dark`() {
        assertEquals(Look.DARK, Look.of(null))
        assertEquals(Look.DARK, Look.DEFAULT)
    }

    @Test
    fun `an unknown saved look falls back to Dark`() {
        assertEquals(Look.DARK, Look.of("blue"))
    }

    @Test
    fun `a saved look comes back as it was picked`() {
        Look.entries.forEach { assertEquals(it, Look.of(it.key)) }
    }
}
