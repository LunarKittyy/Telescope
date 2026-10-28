package com.telescope

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

class BlackScreenStateTest {

    @Test
    fun `the first tap brings the hint back and the second wakes the screen`() {
        val state = BlackScreenState()
        state.enter()
        assertEquals(BlackScreenState.Tap.ShowHint, state.tap(1000))
        assertTrue(state.black)
        assertEquals(BlackScreenState.Tap.Wake, state.tap(1200))
        assertFalse(state.black)
    }

    @Test
    fun `a tap after the hint faded starts over`() {
        val state = BlackScreenState()
        state.enter()
        state.tap(1000)
        state.hintGone()
        assertEquals(BlackScreenState.Tap.ShowHint, state.tap(9000))
        assertTrue(state.black)
    }

    @Test
    fun `going black again forgets a tap from last time`() {
        val state = BlackScreenState()
        state.enter()
        state.tap(1000)
        state.leave()
        state.enter()
        assertEquals(BlackScreenState.Tap.ShowHint, state.tap(2000))
    }

    @Test
    fun `touches just after waking are guarded, later ones are not`() {
        val state = BlackScreenState()
        assertFalse(state.justWoke(0))
        state.enter()
        state.tap(1000)
        state.tap(1100)
        assertTrue(state.justWoke(1100 + BlackScreenState.WAKE_GUARD_MS - 1))
        assertFalse(state.justWoke(1100 + BlackScreenState.WAKE_GUARD_MS))
    }

    @Test
    fun `leaving by Back or a stopped stream doesn't guard the next touch`() {
        val state = BlackScreenState()
        state.enter()
        state.leave()
        assertFalse(state.justWoke(10))
    }

    @Test
    fun `only a stream that ended wakes the screen`() {
        val state = BlackScreenState()
        assertFalse(state.shouldLeave(streaming = false, busy = false))
        state.enter()
        assertFalse(state.shouldLeave(streaming = true, busy = false))
        assertFalse(state.shouldLeave(streaming = false, busy = true))
        assertTrue(state.shouldLeave(streaming = false, busy = false))
    }
}
