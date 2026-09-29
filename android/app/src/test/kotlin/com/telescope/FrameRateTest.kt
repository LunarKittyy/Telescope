package com.telescope

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Test

class FrameRateTest {

    @Test
    fun `counts frames per second over the window`() {
        var now = 0L
        val rate = FrameRate(clock = { now })
        assertEquals(0.0, rate.fps())  // nothing yet
        repeat(141) { rate.tick(); now += 1000 / 47 }  // a camera giving 47 fps, say in a dim room at 60
        now -= 1000 / 47
        assertEquals(47.0, rate.fps(), 1.0)
    }

    @Test
    fun `says nothing until half a window has gone by, and forgets frames that stopped`() {
        var now = 0L
        val rate = FrameRate(clock = { now })
        repeat(10) { rate.tick(); now += 33 }
        assertEquals(0.0, rate.fps())  // 300 ms is too little to go by
        repeat(40) { rate.tick(); now += 33 }
        assertEquals(30.0, rate.fps(), 1.0)
        now += 5_000
        assertEquals(0.0, rate.fps())
    }
}
