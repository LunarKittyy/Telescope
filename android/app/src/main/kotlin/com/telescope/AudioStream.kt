package com.telescope

import java.util.ArrayDeque
import java.util.concurrent.TimeUnit
import java.util.concurrent.locks.ReentrantLock
import kotlin.concurrent.withLock

// The phone microphone's wire format, and the pure parts of serving it (JVM-tested).
object AudioStream {
    const val SAMPLE_RATE = 48_000
    const val CHUNK_MS = 10
    const val CHUNK_BYTES = SAMPLE_RATE / 1000 * CHUNK_MS * 2  // mono s16le
    const val CONTENT_TYPE = "audio/pcm; rate=48000; channels=1; format=s16le"
}

/**
 * One listener's backlog of PCM chunks. A listener that falls behind loses its oldest audio,
 * so it never drifts further than [capacity] chunks behind live.
 */
class PcmClientQueue(private val capacity: Int = 20) {
    private val lock = ReentrantLock()
    private val ready = lock.newCondition()
    private val chunks = ArrayDeque<ByteArray>()

    fun offer(chunk: ByteArray) = lock.withLock {
        while (chunks.size >= capacity) chunks.poll()
        chunks.add(chunk)
        ready.signal()
    }

    fun poll(timeoutMs: Long): ByteArray? = lock.withLock {
        var waitNs = TimeUnit.MILLISECONDS.toNanos(timeoutMs)
        while (chunks.isEmpty()) {
            if (waitNs <= 0) return null
            waitNs = ready.awaitNanos(waitNs)
        }
        chunks.poll()
    }

    fun size(): Int = lock.withLock { chunks.size }
}
