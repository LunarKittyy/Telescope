package com.telescope

import java.util.ArrayDeque
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
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

/** What [MicLoop] records from: AudioRecord on the phone, a fake in tests. */
interface MicSource {
    /** Bytes read, or a negative AudioRecord error. */
    fun read(buf: ByteArray, off: Int, len: Int): Int
    fun stop()
    fun release()
}

// Hands 10 ms chunks to onChunk while active, reopening a recording that fails or goes STALL_MS without audio
class MicLoop(
    private val open: () -> MicSource?,
    private val onChunk: (ByteArray) -> Unit,
    private val active: AtomicBoolean,
    private val clockMs: () -> Long = { System.nanoTime() / 1_000_000 },
    private val sleepMs: (Long) -> Unit = { Thread.sleep(it) },
) {
    companion object {
        const val STALL_MS = 500L
        const val FIRST_RETRY_MS = 100L
        const val MAX_RETRY_MS = 2_000L
    }

    @Volatile private var current: MicSource? = null
    @Volatile private var lastAudioMs = 0L
    @Volatile private var stalled = false
    @Volatile var reopens = 0
        private set

    fun run(first: MicSource) {
        var source: MicSource? = first
        var retryMs = FIRST_RETRY_MS
        while (active.get()) {
            if (source == null) {
                sleepMs(retryMs)
                retryMs = minOf(retryMs * 2, MAX_RETRY_MS)
                if (!active.get()) break
                source = open() ?: continue
                reopens++
            }
            if (pump(source)) retryMs = FIRST_RETRY_MS  // it did record for a while: the next failure retries quickly
            current = null
            runCatching { source.stop() }
            runCatching { source.release() }
            source = null
        }
    }

    // True when some audio came through before it ended.
    private fun pump(source: MicSource): Boolean {
        stalled = false
        lastAudioMs = clockMs()
        current = source
        var got = false
        while (active.get() && !stalled) {
            val chunk = ByteArray(AudioStream.CHUNK_BYTES)
            var n = 0
            while (n < chunk.size && active.get() && !stalled) {
                val r = try { source.read(chunk, n, chunk.size - n) } catch (_: Exception) { -1 }
                if (r < 0) return got
                if (r > 0) lastAudioMs = clockMs()
                n += r
            }
            if (n == chunk.size) { onChunk(chunk); got = true }
        }
        return got
    }

    // Stopping: a read() waiting forever on a dead recording would otherwise keep the mic held
    fun halt() {
        current?.let { runCatching { it.stop() } }
    }

    /** Called every so often from another thread: stops a recording that's gone quiet so [run] opens a new one. */
    fun checkStall(): Boolean {
        val source = current ?: return false
        if (clockMs() - lastAudioMs < STALL_MS) return false
        stalled = true
        runCatching { source.stop() }  // unblocks a read() that's waiting forever
        return true
    }
}
