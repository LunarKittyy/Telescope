package com.telescope

import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

class StartWindowTest {

    private var now = 1_000L
    private val window = StartWindow { now }

    @Test
    fun `closed until a start is accepted`() {
        assertFalse(window.open())
    }

    @Test
    fun `stays open while the service hasn't reported`() {
        window.begin()
        now += StartWindow.PENDING_MS - 1
        assertTrue(window.open())
        now += 1
        assertFalse(window.open())
    }

    @Test
    fun `a start that fails at once still shows as busy for a poll or two`() {
        window.begin()
        window.settle()
        now += StartWindow.SETTLE_MS - 1
        assertTrue(window.open())
        now += 1
        assertFalse(window.open())
    }

    @Test
    fun `settling never makes the window longer`() {
        window.begin()
        now += StartWindow.PENDING_MS - 100
        window.settle()
        now += 100
        assertFalse(window.open())
    }

    @Test
    fun `settling without a start keeps it closed`() {
        window.settle()
        assertFalse(window.open())
    }
}
