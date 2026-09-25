package com.telescope

import java.util.ArrayDeque
import java.util.concurrent.TimeUnit
import java.util.concurrent.locks.ReentrantLock
import kotlin.concurrent.withLock

// Pure pieces of the H.264 stream, kept free of Android so they run as JVM tests.
object H264Stream {
    const val CODEC_MJPEG = "mjpeg"
    const val CODEC_H264 = "h264"

    private const val MIN_BPS = 1_000_000
    private const val MAX_BPS = 30_000_000

    // Roughly 8 Mbps for 1080p30, scaled by pixels per second.
    fun defaultBitrate(width: Int, height: Int, fps: Int): Int {
        val bps = width.toLong() * height * fps.coerceAtLeast(1) * 13 / 100
        return bps.coerceIn(MIN_BPS.toLong(), MAX_BPS.toLong()).toInt()
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
 */
class H264ClientQueue(private val capacity: Int = 90) {
    private val lock = ReentrantLock()
    private val ready = lock.newCondition()
    private val packets = ArrayDeque<ByteArray>()
    private var config: ByteArray? = null
    private var waitingForKey = true

    /** Codec config (SPS/PPS): sent ahead of the next keyframe. A new one replaces what's queued. */
    fun offerConfig(bytes: ByteArray) = lock.withLock {
        config = bytes
        packets.clear()
        waitingForKey = true
    }

    fun offer(packet: ByteArray, key: Boolean): Boolean = lock.withLock {
        if (waitingForKey) {
            if (!key) return false
            waitingForKey = false
            config?.let { packets.add(it) }
        } else if (packets.size >= capacity) {
            packets.clear()
            waitingForKey = true
            return true
        }
        packets.add(packet)
        ready.signal()
        false
    }

    fun poll(timeoutMs: Long): ByteArray? = lock.withLock {
        var waitNs = TimeUnit.MILLISECONDS.toNanos(timeoutMs)
        while (packets.isEmpty()) {
            if (waitNs <= 0) return null
            waitNs = ready.awaitNanos(waitNs)
        }
        packets.poll()
    }

    fun size(): Int = lock.withLock { packets.size }
}
