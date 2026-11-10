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

    @Test
    fun `a stream on its own notification leaves the waiting one to refresh too`() {
        assertEquals(true, Look.waitingNotificationNeedsRefresh(streaming = true, streamCovered = false))
    }

    @Test
    fun `a stream under waiting refreshes the one shared notification`() {
        assertEquals(false, Look.waitingNotificationNeedsRefresh(streaming = true, streamCovered = true))
    }

    @Test
    fun `waiting alone refreshes its notification`() {
        assertEquals(true, Look.waitingNotificationNeedsRefresh(streaming = false, streamCovered = false))
    }
}
