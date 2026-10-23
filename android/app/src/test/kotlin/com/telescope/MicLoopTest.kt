package com.telescope

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.assertThrows
import java.util.concurrent.CountDownLatch
import java.util.concurrent.FutureTask
import java.util.concurrent.TimeUnit.SECONDS
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicLong

class MicLoopTest {
    // Gives `chunks` full chunks, then fails with `then` (a negative error) or blocks until stopped.
    private class Fake(private val chunks: Int, private val then: Int?) : MicSource {
        var given = 0
        @Volatile var stopped = false
        var released = false
        val blocked = CountDownLatch(1)
        private val stopSignal = CountDownLatch(1)
        override fun read(buf: ByteArray, off: Int, len: Int): Int {
            if (given < chunks) { given++; return len }
            if (then != null) return then
            blocked.countDown()
            stopSignal.await()
            return 0
        }
        override fun stop() { stopped = true; stopSignal.countDown() }
        override fun release() { released = true }
    }

    @Test
    fun `a recording that fails is opened again and keeps going`() {
        val active = AtomicBoolean(true)
        val chunks = mutableListOf<ByteArray>()
        val first = Fake(3, AudioRecordErrorDeadObject)
        val second = Fake(2, null)
        val opened = mutableListOf<Fake>()
        lateinit var loop: MicLoop
        loop = MicLoop(
            open = { second.also { opened += it } },
            onChunk = { chunks += it; if (chunks.size == 5) active.set(false) },
            active = active, sleepMs = {},
        )
        loop.run(first)
        assertEquals(5, chunks.size)
        assertEquals(listOf(second), opened)
        assertTrue(first.released && second.released)
        assertEquals(1, loop.reopens)
    }

    @Test
    fun `a mic that can't open yet is retried with a growing wait`() {
        val active = AtomicBoolean(true)
        val waits = mutableListOf<Long>()
        var tries = 0
        val loop = MicLoop(
            open = { tries++; if (tries == 4) active.set(false); null },
            onChunk = {}, active = active, sleepMs = { waits += it },
        )
        loop.run(Fake(0, -1))
        assertEquals(listOf(100L, 200L, 400L, 800L), waits)
    }

    @Test
    fun `a recording that goes quiet is stopped by the watchdog and reopened`() {
        val active = AtomicBoolean(true)
        val now = AtomicLong(0)
        val stuck = Fake(1, null)
        val next = Fake(1, null)
        val loop = MicLoop(open = { active.set(false); next }, onChunk = {}, active = active,
            clockMs = now::get, sleepMs = {})
        val task = FutureTask { loop.run(stuck) }
        val thread = Thread(task).apply { isDaemon = true; start() }
        try {
            assertTrue(stuck.blocked.await(2, SECONDS), "recording never reached the stalled read")
            now.set(MicLoop.STALL_MS - 1)
            assertFalse(loop.checkStall())
            now.incrementAndGet()
            assertTrue(loop.checkStall())
            task.get(2, SECONDS)
            assertTrue(stuck.stopped && stuck.released)
            assertEquals(1, loop.reopens)
        } finally {
            active.set(false)
            loop.halt()
            stuck.stop()
            next.stop()
            thread.join(2_000)
            assertFalse(thread.isAlive, "recording thread did not stop")
        }
    }

    @Test
    fun `stopping ends the loop without reopening`() {
        val active = AtomicBoolean(true)
        val loop = MicLoop(open = { error("shouldn't reopen") }, onChunk = { active.set(false) }, active = active,
            sleepMs = {})
        val source = Fake(5, null)
        loop.run(source)
        assertTrue(source.released)
        assertEquals(0, loop.reopens)
    }

    private companion object {
        const val AudioRecordErrorDeadObject = -6  // AudioRecord.ERROR_DEAD_OBJECT
    }

    @Test
    fun `stop before the recording thread starts still releases the first recording`() {
        val source = Fake(1, null)
        val loop = MicLoop(open = { error("shouldn't reopen") }, onChunk = {}, active = AtomicBoolean(false))
        loop.run(source)
        assertTrue(source.stopped && source.released)
    }

    @Test
    fun `a failed chunk consumer cannot hold the microphone`() {
        val source = Fake(1, null)
        val loop = MicLoop(open = { null }, onChunk = { error("consumer failed") }, active = AtomicBoolean(true))
        val error = assertThrows<IllegalStateException> { loop.run(source) }
        assertEquals("consumer failed", error.message)
        assertTrue(source.stopped && source.released)
    }
}
