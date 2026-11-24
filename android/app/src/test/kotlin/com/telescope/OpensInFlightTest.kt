package com.telescope

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

class OpensInFlightTest {
    @Test
    fun `a stop with nothing opening goes ahead at once`() {
        var stopped = false
        OpensInFlight().whenSettled { stopped = true }
        assertTrue(stopped)
    }

    @Test
    fun `a stop waits for a camera still opening, then goes ahead once it answers`() {
        val opens = OpensInFlight()
        val open = opens.begin()
        var stopped = false
        opens.whenSettled { stopped = true }
        assertFalse(stopped)
        open.end()
        assertTrue(stopped)
    }

    @Test
    fun `a stop waits for every open, and answering one twice counts once`() {
        val opens = OpensInFlight()
        val first = opens.begin()
        val second = opens.begin()
        var stops = 0
        opens.whenSettled { stops++ }
        first.end()
        first.end()
        assertEquals(0, stops)
        second.end()
        assertEquals(1, stops)
    }

    @Test
    fun `a camera service that never answers is given up on, once`() {
        val opens = OpensInFlight()
        val open = opens.begin()
        var stops = 0
        opens.whenSettled { stops++ }
        opens.giveUp()
        assertEquals(1, stops)
        opens.giveUp()
        open.end()
        assertEquals(1, stops)
    }

    @Test
    fun `two waiting stops both run when the open settles`() {
        val opens = OpensInFlight()
        val open = opens.begin()
        var stops = 0
        opens.whenSettled { stops++ }
        opens.whenSettled { stops++ }
        open.end()
        assertEquals(2, stops)
    }
}
