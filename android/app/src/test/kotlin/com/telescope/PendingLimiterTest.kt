package com.telescope

import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

class PendingLimiterTest {

    @Test
    fun `one address can't take more than its share`() {
        val limiter = PendingLimiter(maxTotal = 10, maxPerAddress = 2)
        assertTrue(limiter.tryAcquire("10.0.0.5"))
        assertTrue(limiter.tryAcquire("10.0.0.5"))
        assertFalse(limiter.tryAcquire("10.0.0.5"))
        assertTrue(limiter.tryAcquire("10.0.0.6"))
    }

    @Test
    fun `the total cap holds across addresses`() {
        val limiter = PendingLimiter(maxTotal = 2, maxPerAddress = 2)
        assertTrue(limiter.tryAcquire("a"))
        assertTrue(limiter.tryAcquire("b"))
        assertFalse(limiter.tryAcquire("c"))
    }

    @Test
    fun `releasing frees the slot for the same address`() {
        val limiter = PendingLimiter(maxTotal = 10, maxPerAddress = 1)
        assertTrue(limiter.tryAcquire("a"))
        limiter.release("a")
        assertTrue(limiter.tryAcquire("a"))
    }

    @Test
    fun `releasing an address that holds nothing changes nothing`() {
        val limiter = PendingLimiter(maxTotal = 1, maxPerAddress = 1)
        limiter.release("ghost")
        assertTrue(limiter.tryAcquire("a"))
        assertFalse(limiter.tryAcquire("b"))
    }
}
