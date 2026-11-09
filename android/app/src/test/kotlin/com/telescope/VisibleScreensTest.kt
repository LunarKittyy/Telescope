package com.telescope

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

class VisibleScreensTest {
    private var ups = 0
    private var downs = 0
    private val screens = VisibleScreens({ ups++ }, { downs++ })

    @Test
    fun movingFromMainToSettingsKeepsTheHold() {
        screens.started()  // main
        screens.started()  // settings starts before main stops
        screens.stopped()
        assertEquals(1, ups)
        assertEquals(0, downs)
        assertTrue(screens.any())
    }

    @Test
    fun lastScreenGoneReleases() {
        screens.started()
        screens.started()
        screens.stopped()
        screens.stopped()
        assertEquals(1, downs)
        assertFalse(screens.any())
    }

    @Test
    fun strayStopDoesNothing() {
        screens.stopped()
        assertEquals(0, downs)
    }
}
