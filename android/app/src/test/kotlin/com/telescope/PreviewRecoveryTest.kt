package com.telescope

import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

class PreviewRecoveryTest {
    @Test
    fun `preview results between still frames cannot release the hold`() {
        val recovery = PreviewRecovery()
        recovery.start()
        assertFalse(recovery.result(10, ready = true, tag = recovery.requestTag()))
        assertFalse(recovery.timeout(100_000))
        assertFalse(recovery.keep(10, key = true))
        recovery.resume(100_000)
        assertFalse(recovery.result(20, ready = false, tag = recovery.requestTag()))
        assertFalse(recovery.keep(20))
        assertTrue(recovery.result(30, ready = true, tag = recovery.requestTag()))
        assertFalse(recovery.keep(20))
        assertTrue(recovery.keep(30))
    }

    @Test
    fun `searching or missing AE metadata cannot freeze preview past the cap`() {
        val recovery = PreviewRecovery()
        recovery.start()
        recovery.resume(5_000)
        assertFalse(recovery.result(40, ready = false, tag = recovery.requestTag()))
        assertFalse(recovery.timeout(5_000 + PreviewRecovery.MAX_MS - 1))
        assertTrue(recovery.timeout(5_000 + PreviewRecovery.MAX_MS))
        assertFalse(recovery.keep(39))
        assertTrue(recovery.keep(40))
        assertFalse(recovery.timeout(10_000))
    }

    @Test
    fun `an absent capture callback still releases at the cap`() {
        val recovery = PreviewRecovery()
        recovery.start()
        recovery.resume(0)
        assertTrue(recovery.timeout(PreviewRecovery.MAX_MS))
        assertTrue(recovery.keep(1))
    }

    @Test
    fun `H264 resumes at a fresh keyframe and keeps dependent frames afterwards`() {
        val recovery = PreviewRecovery()
        recovery.start()
        recovery.resume(0)
        assertTrue(recovery.result(100, ready = true, tag = recovery.requestTag()))
        assertTrue(recovery.requestKeyframe(0))
        assertFalse(recovery.keep(99, key = true))
        assertFalse(recovery.requestKeyframe(249))
        assertTrue(recovery.requestKeyframe(250))
        assertFalse(recovery.keep(100, key = false))
        assertTrue(recovery.keep(101, key = true))
        assertTrue(recovery.keep(102, key = false))
        assertFalse(recovery.requestKeyframe(500))
        recovery.start()
        recovery.resume(0)
        recovery.timeout(PreviewRecovery.MAX_MS)
        assertFalse(recovery.keep(null, key = false))
        assertTrue(recovery.keep(null, key = true))
    }

    @Test
    fun `an earlier recovery timeout cannot release the next shot`() {
        val recovery = PreviewRecovery()
        recovery.start()
        recovery.resume(0)
        recovery.start()
        assertFalse(recovery.timeout(PreviewRecovery.MAX_MS))
        recovery.resume(2_000)
        assertFalse(recovery.timeout(PreviewRecovery.MAX_MS))
        assertTrue(recovery.timeout(2_000 + PreviewRecovery.MAX_MS))
    }

    @Test
    fun `only the restored preview request can report convergence`() {
        val recovery = PreviewRecovery()
        recovery.start()
        val beforeRestore = recovery.requestTag()
        recovery.resume(0)
        assertFalse(recovery.result(10, ready = true, tag = beforeRestore))
        val previousShot = recovery.requestTag()
        recovery.start()
        recovery.resume(10)
        assertFalse(recovery.result(20, ready = true, tag = previousShot))
        assertTrue(recovery.result(30, ready = true, tag = recovery.requestTag()))
    }

    @Test
    fun `camera changes clear the hold and keyframe requirement`() {
        val recovery = PreviewRecovery()
        recovery.start()
        recovery.reset()
        assertFalse(recovery.holding)
        assertTrue(recovery.keep(1, key = false))
        assertFalse(recovery.timeout(100_000))
    }
}
