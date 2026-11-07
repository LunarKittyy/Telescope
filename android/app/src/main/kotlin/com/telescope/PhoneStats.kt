package com.telescope

import kotlinx.serialization.Serializable

// What the phone measures for the desktop's Stats card. Nothing is measured unless the desktop asked in the last LEASE_MS
// (control "stats" value=1, sent again every few seconds), so a closed card or a vanished desktop costs the phone nothing.
// The lease and the sample window run on a monotonic clock, so setting the phone's wall clock can't stretch or cut them.
class PhoneStats(private val clock: () -> Long = ::monotonicMs) {
    companion object {
        const val LEASE_MS = 10_000L
        const val WINDOW_MS = 2_000L
        // A camera timestamp further from now than this isn't on the clock we compare it with
        private const val MAX_DELAY_MS = 2_000.0
        val shared = PhoneStats()

        fun monotonicMs(): Long = System.nanoTime() / 1_000_000

        /** Milliseconds from a frame's camera timestamp to [nowNs] on the clock the timestamp is from, or null when it isn't believable. */
        fun delayMs(timestampUs: Long, nowNs: Long): Double? {
            if (timestampUs <= 0) return null
            val ms = (nowNs - timestampUs * 1000) / 1_000_000.0
            return ms.takeIf { it in 0.0..MAX_DELAY_MS }
        }

        /**
         * The camera hands the encoder timestamps on the monotonic clock (System.nanoTime) or on the elapsed-realtime
         * one, which also counts time asleep; its SENSOR_INFO_TIMESTAMP_SOURCE says which ([realtime]). Both clocks read
         * alike until the phone has slept, so guessing is only right by luck: when the camera says nothing, the one that
         * gives a believable delay is taken, and a reading on neither is dropped rather than guessed.
         */
        fun delayMs(timestampUs: Long, monotonicNs: Long, realtimeNs: Long, realtime: Boolean? = null): Double? = when (realtime) {
            true -> delayMs(timestampUs, realtimeNs)
            false -> delayMs(timestampUs, monotonicNs)
            null -> delayMs(timestampUs, monotonicNs) ?: delayMs(timestampUs, realtimeNs)
        }
    }

    private class Sample(val atMs: Long, val value: Double)

    /** Whether the open camera stamps frames on the elapsed-realtime clock (true), the monotonic one (false) or doesn't say (null). */
    @Volatile var realtimeStamps: Boolean? = null

    private val encode = ArrayDeque<Sample>()
    private val queue = ArrayDeque<Sample>()
    @Volatile private var leaseUntilMs = 0L

    /** The desktop asked (true) or let go (false). */
    fun ask(on: Boolean) {
        leaseUntilMs = if (on) clock() + LEASE_MS else 0L
        if (!on) synchronized(this) { encode.clear(); queue.clear() }
    }

    fun active(): Boolean = clock() < leaseUntilMs

    /** A frame took this long from the camera to come out of the encoder. */
    fun addEncode(ms: Double) {
        if (active()) add(encode, ms)
    }

    /** A packet waited this long before it went to the socket. */
    fun addQueue(ms: Long) {
        if (active()) add(queue, ms.toDouble())
    }

    @Synchronized
    private fun add(list: ArrayDeque<Sample>, value: Double) {
        val now = clock()
        list.addLast(Sample(now, value))
        while (list.isNotEmpty() && now - list.first().atMs > WINDOW_MS) list.removeFirst()
    }

    /** The last couple of seconds, or null while nobody asks. Fields without a reading are left null. */
    @Synchronized
    fun report(encoderFps: Double?, thermalStatus: Int?, thermalHeadroom: Float?): PhoneStatsReport? {
        if (!active()) return null
        val now = clock()
        val e = encode.filter { now - it.atMs <= WINDOW_MS }
        val q = queue.filter { now - it.atMs <= WINDOW_MS }
        return PhoneStatsReport(
            encode_ms = e.takeIf { it.isNotEmpty() }?.let { round1(it.sumOf { s -> s.value } / it.size) },
            encode_max_ms = e.maxOfOrNull { it.value }?.let(::round1),
            queue_ms = q.maxOfOrNull { it.value }?.let(::round1),
            encoder_fps = encoderFps?.takeIf { it > 0 }?.let(::round1),
            thermal = thermalStatus,
            thermal_headroom = thermalHeadroom?.takeIf { it.isFinite() }?.let { Math.round(it * 100) / 100f },
        )
    }

    private fun round1(v: Double) = Math.round(v * 10) / 10.0
}

// /v1/state "stats": only while the desktop asks, each field only when the phone could measure it
@Serializable
data class PhoneStatsReport(
    val encode_ms: Double? = null,  // a frame's average time from the camera to out of the encoder, over the last two seconds
    val encode_max_ms: Double? = null,  // the slowest of them
    val queue_ms: Double? = null,  // the longest a packet waited to go to the socket (TCP viewers)
    val encoder_fps: Double? = null,  // frames per second the encoder put out
    val thermal: Int? = null,  // PowerManager.THERMAL_STATUS_*, 0 none to 6 shutdown (Android 10 and newer)
    val thermal_headroom: Float? = null,  // Android's forecast of how close to throttling, 1.0 = at it (Android 11 and newer)
)
