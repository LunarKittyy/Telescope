package com.telescope

import java.nio.ByteBuffer
import java.util.ArrayDeque
import java.util.concurrent.TimeUnit
import java.util.concurrent.locks.ReentrantLock
import kotlin.concurrent.withLock

// Pure pieces of the H.264 stream, kept free of Android so they run as JVM tests.
object H264Stream {
    const val CODEC_MJPEG = "mjpeg"
    const val CODEC_H264 = "h264"

    const val MIN_BPS = 1_000_000
    const val DYNAMIC_MIN_BPS = 300_000  // Dynamic on a link thinner than MIN_BPS: a softer picture beats skipped frames
    const val MAX_BPS = 100_000_000       // what the desktop's slider goes up to
    const val AUTO_MAX_BPS = 30_000_000   // Auto stays at most this, as it always has
    const val DYNAMIC = -1  // the bitrate control's value for DynamicBitrate
    const val LATENCY_BUDGET_MS = 1_000L  // the oldest a waiting packet may get, in every bitrate mode; past it the viewer skips ahead

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

    /** Most Dynamic sends for cleaner video: as much as the encoder takes, up to the slider's top. */
    fun cleanCeiling(encoderMax: Int = MAX_BPS): Int = minOf(MAX_BPS, encoderMax).coerceAtLeast(MIN_BPS)

    // An access unit delimiter. Sent after each frame's packet: a decoder only knows a frame has
    // ended when the next one starts, so without it every frame would wait for the one after.
    val AUD = byteArrayOf(0, 0, 0, 1, 0x09, 0xF0.toByte())

    // "TELESCOPE-TIME11": the user_data_unregistered UUID that marks the capture time SEI
    private val TIME_UUID = "TELESCOPE-TIME11".toByteArray(Charsets.US_ASCII)
    // "TELESCOPE-CROP11": the one that marks the crop the camera reported for the picture, a second message in the same SEI
    private val CROP_UUID = "TELESCOPE-CROP11".toByteArray(Charsets.US_ASCII)

    /**
     * An SEI NAL unit carrying the frame's capture time, to go in front of its packet. [realtime] says which clock
     * [timeNs] is on: true = elapsed realtime (clock byte 0), false = System.nanoTime (1); null sends none. [share] is the
     * crop the camera reported for the frame as the `zoom` control's ratio, crop, x and y; a computer finds the frame by its time, so it goes only with one.
     */
    fun captureTimeSei(realtime: Boolean?, timeNs: Long, share: FloatArray? = null): ByteArray {
        if (realtime == null) return ByteArray(0)
        val rbsp = java.io.ByteArrayOutputStream()
        // Payload type 5 (user data unregistered), size 25: the UUID, the clock byte and the time
        rbsp.write(5); rbsp.write(25); rbsp.write(TIME_UUID)
        rbsp.write(if (realtime) 0 else 1)
        for (i in 0 until 8) rbsp.write((timeNs ushr (56 - 8 * i)).toInt())
        if (share != null && share.size == 4) {
            rbsp.write(5); rbsp.write(32); rbsp.write(CROP_UUID)
            for (v in share) { val bits = v.toRawBits(); for (i in 0 until 4) rbsp.write(bits ushr (24 - 8 * i)) }
        }
        rbsp.write(0x80)
        // Emulation prevention: an 03 goes in after each 00 00 that a 00..03 follows, so nothing in it looks like a start code
        val escaped = java.io.ByteArrayOutputStream()
        escaped.write(byteArrayOf(0, 0, 0, 1, 0x06))
        var zeros = 0
        for (b in rbsp.toByteArray()) {
            if (zeros >= 2 && (b.toInt() and 0xFF) <= 3) { escaped.write(3); zeros = 0 }
            escaped.write(b.toInt())
            zeros = if (b.toInt() == 0) zeros + 1 else 0
        }
        return escaped.toByteArray()
    }

    // Read the codec bytes straight into the packet, between its capture time and delimiter
    fun packet(codec: ByteBuffer, realtime: Boolean?, timeNs: Long, share: FloatArray? = null): ByteArray {
        val sei = captureTimeSei(realtime, timeNs, share)
        val size = codec.remaining()
        val packet = ByteArray(sei.size + size + AUD.size)
        sei.copyInto(packet)
        codec.get(packet, sei.size, size)
        AUD.copyInto(packet, sei.size + size)
        return packet
    }

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
    private val clock: () -> Long = PhoneStats::monotonicMs,
    private val onWaited: (Long) -> Unit = {},  // how long each packet waited, for the Stats card
) {
    private val lock = ReentrantLock()
    private val ready = lock.newCondition()
    private val packets = ArrayDeque<ByteArray>()
    private val queuedAt = ArrayDeque<Long>()
    private val isKey = ArrayDeque<Boolean>()
    private var config: ByteArray? = null
    private var waitingForKey = true

    /** A backlog whose oldest packet waited longer than this also overflows: every mode skips ahead rather than lag. */
    @Volatile var maxWaitMs: Long = H264Stream.LATENCY_BUDGET_MS

    private var shortestWaitMs = -1L  // since the last takeQueueMs(); -1 = nothing went out
    private var writingSinceMs = -1L  // a packet handed to the sender and not yet written
    private var sentBytes = 0L
    private var keyWriting = false  // a keyframe is going out: on a thin link it can take a second, which isn't lag
    private var keyDoneMs = Long.MIN_VALUE

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
            config?.let { add(it, now, true) }
        } else if (packets.size >= capacity || (queuedAt.isNotEmpty() && !keyWriting && now - maxOf(queuedAt.first(), keyDoneMs) > maxWaitMs)) {
            clear()
            if (!key) return true
            // The overflowing frame is itself a keyframe: restart from it rather than drop it and ask for another
            waitingForKey = false
            config?.let { add(it, now, true) }
        }
        add(packet, now, key)
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
        val waited = now - queuedAt.remove()
        onWaited(waited)
        if (shortestWaitMs < 0 || waited < shortestWaitMs) shortestWaitMs = waited
        writingSinceMs = now
        if (isKey.poll() == true) keyWriting = true
        packets.poll()
    }

    /** [bytes] more of the packet poll() last returned went into the socket; [done] once all of it has. */
    fun written(bytes: Int, done: Boolean) = lock.withLock {
        if (done) {
            writingSinceMs = -1
            if (keyWriting) { keyWriting = false; keyDoneMs = clock() }
        }
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

    private fun add(packet: ByteArray, now: Long, key: Boolean) {
        packets.add(packet)
        queuedAt.add(now)
        isKey.add(key)
    }

    private fun clear() {
        packets.clear()
        queuedAt.clear()
        isKey.clear()
        waitingForKey = true
    }
}

/**
 * The crop the camera reported for each recent frame, by its sensor time in microseconds, for the encoder to tag the frame
 * with. A frame gets the crop whose time is nearest its own within half a frame, so a clock offset sampled a little off or
 * a time cut to microseconds still finds it. The camera's result usually comes a few ms before the encoded frame, now and
 * then a few ms after it, so [take] waits a little, but only while that frame's result is the next one due.
 */
class CropLog(private val capacity: Int = 64) {
    private val lock = ReentrantLock()
    private val arrived = lock.newCondition()
    private val shares = java.util.TreeMap<Long, FloatArray>()

    fun put(timeUs: Long, share: FloatArray) = lock.withLock {
        shares[timeUs] = share
        while (shares.size > capacity) shares.pollFirstEntry()
        arrived.signalAll()
    }

    /**
     * The crop of the frame taken at the first of [timesUs] that has one, the clocks the encoder's time may be on with the
     * likeliest first; null when none came. Only the first waits, up to [waitMs], and only while no later frame's result
     * has come and its own is the next one due, so a time on the wrong clock never holds the caller.
     */
    fun take(timesUs: LongArray, waitMs: Long): FloatArray? = lock.withLock {
        val first = timesUs.firstOrNull() ?: return null
        val deadline = System.nanoTime() + TimeUnit.MILLISECONDS.toNanos(waitMs)
        while (true) {
            nearest(first)?.let { return it }
            val newest = if (shares.isEmpty()) null else shares.lastKey()
            val due = newest != null && (first > newest && first - newest <= 2 * interval())
            val left = deadline - System.nanoTime()
            if (!due || left <= 0) break
            arrived.awaitNanos(left)
        }
        for (i in 1 until timesUs.size) nearest(timesUs[i])?.let { return it }
        null
    }

    fun take(timeUs: Long, waitMs: Long): FloatArray? = take(longArrayOf(timeUs), waitMs)

    fun clear() = lock.withLock { shares.clear() }

    // The crop logged within half a frame of timeUs, the nearer of the two either side
    private fun nearest(timeUs: Long): FloatArray? {
        val tolerance = interval() / 2
        val below = shares.floorEntry(timeUs)
        val above = shares.ceilingEntry(timeUs)
        val best = listOfNotNull(below, above).minByOrNull { kotlin.math.abs(it.key - timeUs) } ?: return null
        return best.value.takeIf { kotlin.math.abs(best.key - timeUs) <= tolerance }
    }

    // The time between the last two frames logged (us), within reason; a default before there are two
    private fun interval(): Long {
        if (shares.size < 2) return DEFAULT_INTERVAL_US
        val last = shares.lastKey()
        return (last - shares.lowerKey(last)).coerceIn(MIN_INTERVAL_US, MAX_INTERVAL_US)
    }

    companion object {
        private const val DEFAULT_INTERVAL_US = 33_333L
        private const val MIN_INTERVAL_US = 4_000L
        private const val MAX_INTERVAL_US = 100_000L

        // One camera streams H.264 at a time: its session writes here and its encoder reads
        val shared = CropLog()
    }
}
