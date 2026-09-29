package com.telescope

import java.util.ArrayDeque
import java.util.concurrent.TimeUnit
import java.util.concurrent.locks.ReentrantLock
import kotlin.concurrent.withLock

// Pure pieces of the H.264 stream, kept free of Android so they run as JVM tests.
object H264Stream {
    const val CODEC_MJPEG = "mjpeg"
    const val CODEC_H264 = "h264"

    const val MIN_BPS = 1_000_000
    const val MAX_BPS = 100_000_000       // what the desktop's slider goes up to
    const val AUTO_MAX_BPS = 30_000_000   // Auto stays at most this, as it always has
    const val DYNAMIC = -1  // the bitrate control's value for DynamicBitrate

    // Roughly 8 Mbps for 1080p30, scaled by pixels per second.
    private fun sized(width: Int, height: Int, fps: Int): Long = width.toLong() * height * fps.coerceAtLeast(1) * 13 / 100

    fun defaultBitrate(width: Int, height: Int, fps: Int): Int =
        sized(width, height, fps).coerceIn(MIN_BPS.toLong(), AUTO_MAX_BPS.toLong()).toInt()

    /** Most Dynamic sends at this size and rate: past about 2.5 times Auto's sizing the extra data doesn't show.
     *  [encoderMax] is what this phone's encoder takes. Whole 0.1 Mbps, like the bitrates Dynamic picks. */
    fun dynamicCeiling(width: Int, height: Int, fps: Int, encoderMax: Int = MAX_BPS): Int {
        val top = minOf(MAX_BPS, encoderMax).coerceAtLeast(MIN_BPS).toLong()
        return (sized(width, height, fps) * 5 / 2 / 100_000 * 100_000).coerceIn(MIN_BPS.toLong(), top).toInt()
    }

    // An access unit delimiter. Sent after each frame's packet: a decoder only knows a frame has
    // ended when the next one starts, so without it every frame would wait for the one after.
    val AUD = byteArrayOf(0, 0, 0, 1, 0x09, 0xF0.toByte())

    fun withDelimiter(packet: ByteArray): ByteArray = packet + AUD

    // 0 asks for the default; anything else is clamped to what the desktop's slider offers.
    fun bitrateFor(requested: Int, width: Int, height: Int, fps: Int): Int =
        if (requested <= 0) defaultBitrate(width, height, fps) else requested.coerceIn(MIN_BPS, MAX_BPS)
}

/**
 * One viewer's backlog of Annex-B packets. A decoder can only start at a keyframe, and after the
 * backlog overflows it has to start over at one too, so non-key packets are dropped until the next
 * keyframe arrives. [offer] returns true when the encoder should be asked for a keyframe.
 *
 * It also measures the link for [DynamicBitrate]: how long packets wait to go out ([takeQueueMs]) and how much
 * the socket took ([takeSentBytes]); the sender reports what it writes with [written].
 */
class H264ClientQueue(
    private val capacity: Int = 90,
    private val clock: () -> Long = System::currentTimeMillis,
) {
    private val lock = ReentrantLock()
    private val ready = lock.newCondition()
    private val packets = ArrayDeque<ByteArray>()
    private val queuedAt = ArrayDeque<Long>()
    private var config: ByteArray? = null
    private var waitingForKey = true

    /** A backlog whose oldest packet waited longer than this also overflows (Dynamic skips ahead rather than lag). */
    @Volatile var maxWaitMs: Long = Long.MAX_VALUE

    private var shortestWaitMs = -1L  // since the last takeQueueMs(); -1 = nothing went out
    private var writingSinceMs = -1L  // a packet handed to the sender and not yet written
    private var sentBytes = 0L

    /** Codec config (SPS/PPS): sent ahead of the next keyframe. A new one replaces what's queued. */
    fun offerConfig(bytes: ByteArray) = lock.withLock {
        config = bytes
        clear()
    }

    fun offer(packet: ByteArray, key: Boolean): Boolean = lock.withLock {
        val now = clock()
        if (waitingForKey) {
            if (!key) return false
            waitingForKey = false
            config?.let { add(it, now) }
        } else if (packets.size >= capacity || (queuedAt.isNotEmpty() && now - queuedAt.first() > maxWaitMs)) {
            clear()
            return true
        }
        add(packet, now)
        ready.signal()
        false
    }

    fun poll(timeoutMs: Long): ByteArray? = lock.withLock {
        var waitNs = TimeUnit.MILLISECONDS.toNanos(timeoutMs)
        while (packets.isEmpty()) {
            if (waitNs <= 0) return null
            waitNs = ready.awaitNanos(waitNs)
        }
        val now = clock()
        val waited = now - queuedAt.poll()
        if (shortestWaitMs < 0 || waited < shortestWaitMs) shortestWaitMs = waited
        writingSinceMs = now
        packets.poll()
    }

    /** [bytes] more of the packet poll() last returned went into the socket; [done] once all of it has. */
    fun written(bytes: Int, done: Boolean) = lock.withLock {
        if (done) writingSinceMs = -1
        sentBytes += bytes
    }

    /**
     * The shortest wait a packet had since the last call: a queue that stays, not a keyframe's burst that clears.
     * When nothing went out at all, how long the oldest is waiting, or the write in progress has taken.
     */
    fun takeQueueMs(): Long = lock.withLock {
        val now = clock()
        val shortest = shortestWaitMs
        shortestWaitMs = -1
        if (shortest >= 0) return shortest
        maxOf(queuedAt.peek()?.let { now - it } ?: 0L, if (writingSinceMs >= 0) now - writingSinceMs else 0L)
    }

    /** Bytes written since the last call. */
    fun takeSentBytes(): Long = lock.withLock { sentBytes.also { sentBytes = 0 } }

    fun size(): Int = lock.withLock { packets.size }

    private fun add(packet: ByteArray, now: Long) {
        packets.add(packet)
        queuedAt.add(now)
    }

    private fun clear() {
        packets.clear()
        queuedAt.clear()
        waitingForKey = true
    }
}
