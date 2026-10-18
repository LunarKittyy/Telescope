package com.telescope

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Test

class WifiLinkTest {
    @Test
    fun `a channel's frequency gives its band`() {
        assertEquals("2.4", WifiLink.band(2412))
        assertEquals("2.4", WifiLink.band(2484))
        assertEquals("5", WifiLink.band(5180))
        assertEquals("5", WifiLink.band(5825))
        assertEquals("6", WifiLink.band(5955))
        assertEquals("6", WifiLink.band(7115))
        assertEquals(null, WifiLink.band(0))  // not connected
        assertEquals(null, WifiLink.band(60_480))  // 60 GHz: not something to say anything about
    }
}
