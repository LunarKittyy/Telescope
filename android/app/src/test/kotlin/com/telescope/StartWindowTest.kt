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
    fun `a start that couldn't be made closes the window`() {
        window.begin()
        window.cancel()
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

    @Test
    fun `a stop while a start is queued cancels that start and closes the window`() {
        val ticket = window.begin()
        assertTrue(window.abort())
        assertFalse(window.open())
        assertTrue(window.aborted(ticket))
    }

    @Test
    fun `a start made after the stop is not cancelled`() {
        val before = window.begin()
        window.abort()
        val after = window.begin()
        assertTrue(window.aborted(before))
        assertFalse(window.aborted(after))
    }

    @Test
    fun `a stop with no start in flight cancels nothing`() {
        assertFalse(window.abort())
        assertFalse(window.aborted(window.begin()))
    }

    @Test
    fun `a start with no ticket is never cancelled`() {
        window.begin()
        window.abort()
        assertFalse(window.aborted(0L))
    }

    @Test
    fun `another computer's stop leaves a queued start alone`() {
        val ticket = window.begin("a")
        assertFalse(window.abort("b"))
        assertTrue(window.open())
        assertFalse(window.aborted(ticket))
        assertTrue(window.abort("a"))
        assertTrue(window.aborted(ticket))
    }

    @Test
    fun `a stop that names nobody still cancels a start`() {
        val ticket = window.begin("a")
        assertTrue(window.abort(null))
        assertTrue(window.aborted(ticket))
    }

    @Test
    fun `a start nobody owns is cancelled by any stop`() {
        val ticket = window.begin()
        assertTrue(window.abort("b"))
        assertTrue(window.aborted(ticket))
    }

    @Test
    fun `a stop only cancels the starts its own computer made`() {
        val a = window.begin("a")
        val b = window.begin("b")
        assertTrue(window.abort("b"))
        assertTrue(window.aborted(b))
        assertFalse(window.aborted(a))
        assertTrue(window.open())
        assertTrue(window.abort("a"))
        assertTrue(window.aborted(a))
        assertFalse(window.open())
    }

    @Test
    fun `a stop from a third computer cancels neither of two queued starts`() {
        val a = window.begin("a")
        val b = window.begin("b")
        assertFalse(window.abort("c"))
        assertFalse(window.aborted(a))
        assertFalse(window.aborted(b))
        assertTrue(window.open())
    }

    @Test
    fun `starts from an earlier window don't count as queued`() {
        window.begin("a")
        now += StartWindow.PENDING_MS
        val b = window.begin("b")
        assertTrue(window.abort("b"))
        assertTrue(window.aborted(b))
        assertFalse(window.open())
    }
}
