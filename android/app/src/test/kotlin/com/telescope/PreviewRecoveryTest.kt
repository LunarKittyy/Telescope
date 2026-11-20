package com.telescope

import org.junit.jupiter.api.Assertions.assertEquals
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
    @Test
    fun `recovery schedules and checks the same uptime deadline without extending it`() {
        val recovery = PreviewRecovery()
        recovery.start()
        val uptime = 5_000L
        val deadline = recovery.resume(uptime)!!
        assertEquals(uptime + PreviewRecovery.MAX_MS, deadline)
        assertEquals(deadline, recovery.resume(uptime + 300))
        assertFalse(recovery.timeout(deadline - 1))
        assertTrue(recovery.timeout(deadline))
        assertTrue(recovery.keep(1))
    }

    @Test
    fun `a rebuilt session gets a fresh deadline`() {
        val recovery = PreviewRecovery()
        recovery.start()
        recovery.resume(0)
        recovery.restartDeadline()
        assertEquals(10_000 + PreviewRecovery.MAX_MS, recovery.resume(10_000))
    }

    @Test
    fun `missing keyframes reach a fixed rebuild deadline even with repeated sync requests`() {
        val recovery = PreviewRecovery()
        recovery.start()
        recovery.resume(0)
        recovery.timeout(PreviewRecovery.MAX_MS)
        assertTrue(recovery.requestKeyframe(1_500))
        val deadline = recovery.keyframeDeadline()!!
        assertTrue(recovery.requestKeyframe(2_000))
        assertEquals(deadline, recovery.keyframeDeadline())
        assertFalse(recovery.keep(null, key = false))
        assertFalse(recovery.keyframeTimeout(deadline - 1))
        assertTrue(recovery.keyframeTimeout(deadline))
        recovery.reset()
        assertTrue(recovery.keep(null, key = false))
    }

    @Test
    fun `a keyframe or a new photo cancels an older keyframe timeout`() {
        val recovery = PreviewRecovery()
        recovery.start()
        recovery.resume(0)
        recovery.timeout(PreviewRecovery.MAX_MS)
        recovery.requestKeyframe(1_500)
        assertTrue(recovery.keep(null, key = true))
        assertFalse(recovery.keyframeTimeout(3_000))
        recovery.start()
        recovery.resume(3_000)
        recovery.timeout(4_500)
        recovery.requestKeyframe(4_500)
        recovery.start()
        assertFalse(recovery.keyframeTimeout(6_000))
    }

    @Test
    fun `ordinary photos only restore repeating after an abort or a preview hold`() {
        val recovery = PreviewRecovery()
        assertFalse(recovery.restoreRequired(aborted = false))
        assertTrue(recovery.restoreRequired(aborted = true))
        recovery.start()
        assertTrue(recovery.restoreRequired(aborted = false))
        recovery.reset()
        assertFalse(recovery.restoreRequired(aborted = false))
    }

    @Test
    fun `focus settle targets exclude the phone preview throughout a manual burst`() {
        val recovery = PreviewRecovery()
        assertEquals(listOf("stream", "phone"), recovery.targets(listOf("stream", "phone"), "phone"))
        recovery.start()
        repeat(3) { assertEquals(listOf("stream"), recovery.targets(listOf("stream", "phone"), "phone")) }
        recovery.resume(0)
        assertEquals(listOf("stream"), recovery.targets(listOf("stream", "phone"), "phone"))
        recovery.timeout(PreviewRecovery.MAX_MS)
        assertEquals(listOf("stream", "phone"), recovery.targets(listOf("stream", "phone"), "phone"))
    }
}
