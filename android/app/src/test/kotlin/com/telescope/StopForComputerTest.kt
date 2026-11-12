package com.telescope

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

class StopForComputerTest {

    private val a = PairedComputer("a", "Desk", "t", 0L)
    private val b = PairedComputer("b", "Laptop", "t", 0L)

    private class Service(var owner: PairedComputer?) : StopTarget {
        val stops = mutableListOf<String>()
        override fun otherOwner(caller: PairedComputer?) = owner?.takeIf { caller != null && it.id != caller.id }
        override fun stop(op: String) { stops += op }
    }

    @Test
    fun `a stop reaches a stream nobody else has`() {
        val service = Service(owner = null)
        assertTrue(stopForComputer(service, b))
        assertEquals(listOf("remoteStop"), service.stops)
    }

    @Test
    fun `a stop reaches the stream of the computer that sent it`() {
        val service = Service(owner = b)
        assertTrue(stopForComputer(service, b))
        assertEquals(listOf("remoteStop"), service.stops)
    }

    @Test
    fun `a start queued for another computer that has reached the service is left alone`() {
        // B's stop was answered while the service was idle, then A's queued start ran before B's stop did
        val service = Service(owner = null)
        service.owner = a
        assertFalse(stopForComputer(service, b))
        assertTrue(service.stops.isEmpty())
    }

    @Test
    fun `no service means nothing to stop`() {
        assertFalse(stopForComputer(null, b))
    }
}
