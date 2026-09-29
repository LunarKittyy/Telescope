package com.telescope

/**
 * The Dynamic bitrate: as much as the link carries, without video queueing up on the phone.
 *
 * The signal is how long video waits to go out: the shortest wait over a sample, so a keyframe's burst that clears
 * right away doesn't count, only a queue that stays. That's near zero while the link keeps up and grows as soon as
 * it can't, before the picture on the computer lags. When a queue stays, the bitrate drops to just under what got
 * through and waits for the backlog to drain. Then it climbs back quickly to just under where the link filled up,
 * settles there, and only later looks higher, a little at a time, less often each time it finds the same wall.
 *
 * A stall (nothing going out for a moment: Wi-Fi roaming, a microwave) isn't a slower link, so it doesn't count;
 * the server skips ahead if one lasts. And it only climbs while the encoder uses its budget: a still scene sends
 * little, which says nothing about the link. Tuned against simulated links in DynamicBitrateSimTest.
 *
 * [update] runs on the encoder's thread, [rebound] on whichever changes the size or rate; [bitrate] reads anywhere.
 */
class DynamicBitrate(defaultBps: Int) {

    /** What happened on the link since the last update. */
    data class Sample(
        val nowMs: Long,
        val queueMs: Long,       // the shortest wait to go out since the last sample, for the slowest viewer
        val sentBytes: Long,     // bytes the slowest viewer's socket took since the last sample
        val encodedBytes: Long,  // bytes the encoder produced since the last sample
    )

    companion object {
        const val CONGESTED_MS = 60L     // a queue that stays this long means the link is full
        const val SEVERE_MS = 400L       // this long means it's far over: straight to what gets through
        const val CLEAR_MS = 25L         // under this it keeps up
        const val CUT_GAP_MS = 1_000L    // a cut needs this long to show before the next one
        const val HOLD_MS = 3_000L       // no climbing this soon after a cut
        const val STEP_MS = 1_000L       // at most one step up a second, after a second that kept up
        const val MEMORY_MS = 30_000L    // how long where the link filled up is remembered
        const val WINDOW_MS = 1_000L     // rates are measured over this long
        const val GAP_MS = 2_000L        // samples further apart than this (no viewer meanwhile) start over
        const val RECENT_MS = 500L       // the link's speed right now is measured over this long
        const val STALL_MS = 1_500L      // a stall this long counts as the worst link there is
        const val STALL_GRACE_MS = 1_000L  // after a stall the backlog drains first: no cuts meanwhile
        const val PROBE_MS = 10_000L     // after a cut, stays under where it filled up this long before looking higher,
        const val PROBE_MAX_MS = 20_000L // and longer each time looking higher only found the same wall
        private const val MIN_CHANGE = 0.05  // smaller changes aren't worth reconfiguring the encoder

        /** Most a stream of this size and rate gets: past this the extra data doesn't show. */
        fun ceiling(defaultBps: Int): Int =
            (defaultBps * 5L / 2 / 100_000 * 100_000).coerceIn(H264Stream.MIN_BPS.toLong(), H264Stream.MAX_BPS.toLong()).toInt()
    }

    private var floor = H264Stream.MIN_BPS
    private var ceiling = ceiling(defaultBps)

    /** The bitrate the encoder should run at. */
    @Volatile var bitrate: Int = defaultBps.coerceIn(floor, ceiling)
        private set

    private var target = bitrate.toDouble()
    private var capacity = 0.0          // what got through when the link last filled up; 0 = not known
    private var capacityAtMs = 0L
    private var lastCutMs = Long.MIN_VALUE / 2
    private var lastStepMs = Long.MIN_VALUE / 2
    private var clearSinceMs = -1L
    private var stallSinceMs = -1L
    private var climbs = 0  // steps up in a row since the last cut
    private var lastStallMs = Long.MIN_VALUE / 2
    private var probeMs = PROBE_MS
    private var probing = false  // looking past the wall
    private val window = ArrayDeque<Sample>()
    private val stalled = ArrayDeque<Boolean>()  // alongside window: nothing went out in that sample

    /** A new size or frame rate: same link, other bounds. */
    @Synchronized
    fun rebound(defaultBps: Int) {
        val newCeiling = ceiling(defaultBps)
        if (newCeiling == ceiling) return
        ceiling = newCeiling
        target = target.coerceIn(floor.toDouble(), ceiling.toDouble())
        bitrate = rounded(target)
    }

    /** Feed one sample; returns the new bitrate when the encoder should change, else null. */
    @Synchronized
    fun update(s: Sample): Int? {
        var prev = window.lastOrNull()
        if (prev != null && s.nowMs <= prev.nowMs) return null
        if (prev != null && s.nowMs - prev.nowMs > GAP_MS) {  // nobody watched for a while: measure afresh
            window.clear(); stalled.clear(); stallSinceMs = -1; clearSinceMs = -1; prev = null
        }
        // Video waiting but nothing going out: a stall (Wi-Fi roaming, a microwave), not a slower link. Cutting
        // wouldn't help, and what got through says nothing about the link. Only a long one counts, as the worst link.
        val stall = prev != null && s.queueMs > CLEAR_MS && s.sentBytes * 8_000.0 / (s.nowMs - prev.nowMs) < target * 0.05
        window.addLast(s)
        stalled.addLast(stall)
        while (window.size > 1 && s.nowMs - window.first().nowMs > WINDOW_MS) { window.removeFirst(); stalled.removeFirst() }
        if (prev == null) return null
        // What the encoder actually spends of its budget (a VBR encoder rarely spends all of it).
        val usage = (rate { it.encodedBytes } / target).coerceIn(0.3, 1.2)

        if (stall) {
            clearSinceMs = -1
            lastStallMs = s.nowMs
            if (stallSinceMs < 0) stallSinceMs = prev.nowMs
            if (s.nowMs - stallSinceMs < STALL_MS || s.nowMs - lastCutMs < CUT_GAP_MS) return null
            lastCutMs = s.nowMs
            return set(floor.toDouble(), force = true)
        }
        stallSinceMs = -1
        if (capacity > 0 && s.nowMs - capacityAtMs > MEMORY_MS) capacity = 0.0  // the link may be better now
        // A backlog draining shows the link carries at least that much, e.g. right after a stall that looked slow
        // (the kernel's buffer can make a half second look up to a fifth faster than it was).
        if (capacity > 0) capacity = maxOf(capacity, sentRate(s.nowMs, RECENT_MS) * 0.8)

        if (s.queueMs >= CONGESTED_MS) {
            clearSinceMs = -1
            // One sample is noisy (a stall starting halfway through it, a keyframe): a queue has to stay for two.
            val lasting = prev.queueMs >= CONGESTED_MS && !stalled[stalled.size - 2]
            if (!lasting && s.queueMs < SEVERE_MS) return null
            val sent = sentRate(s.nowMs, RECENT_MS)
            if (sent <= 0 || s.nowMs - lastStallMs < STALL_GRACE_MS) return null  // let a stall's backlog drain first
            if (s.queueMs < prev.queueMs * 0.8) return null  // shrinking: the last cut is working, let it drain
            // Going out faster than the encoder fills it: a backlog from a stall draining, not a full link.
            if (sent > target * usage * 1.1 || s.nowMs - lastCutMs < CUT_GAP_MS) return null
            // What got through is what the link carries right now: aim the encoder's output a little under it, so the
            // backlog drains. Far over, or slower for the whole last second, goes straight there: the link is slower.
            // A short dip may be over already, so that's at most 30% at a time.
            val toSent = sent * 0.9 / usage
            val slower = s.queueMs >= SEVERE_MS || sentRate(s.nowMs, WINDOW_MS) < target * usage * 0.8
            val next = if (slower) minOf(target * 0.9, toSent) else maxOf(target * 0.7, minOf(target * 0.9, toSent))
            // Where the wall is: what got through on a slower link, or after a dip, about where the output ran into it.
            capacity = if (slower) sent else maxOf(sent, target * usage * 0.9)
            capacityAtMs = s.nowMs
            lastCutMs = s.nowMs
            climbs = 0
            if (probing) probeMs = minOf(PROBE_MAX_MS, probeMs * 3 / 2)
            probing = false
            return set(next, force = true)
        }
        if (s.queueMs > CLEAR_MS) { clearSinceMs = -1; return null }
        if (clearSinceMs < 0) clearSinceMs = s.nowMs
        val calm = s.nowMs - clearSinceMs >= STEP_MS && s.nowMs - lastCutMs >= HOLD_MS && s.nowMs - lastStepMs >= STEP_MS
        // A still scene uses a fraction of the budget, and climbing on that would find the wall only once it moves.
        if (!calm || usage < 0.5) return null
        val output = target * usage
        if (capacity > 0 && output > capacity * 1.1) {  // well past where it filled up: the link got better
            capacity = 0.0
            probeMs = PROBE_MS
            probing = false
        }
        // Nothing known: climbs faster while it goes well. Well under the wall: quickly up to just under it, and
        // settles there. Then looks higher, gently at first: past the wall, the link got better and it's forgotten.
        var next = when {
            capacity <= 0 -> target * (1 + minOf(0.15, 0.06 + 0.02 * climbs++))
            output < capacity * 0.85 -> minOf(target * (if (output < capacity * 0.5) 1.25 else 1.08), capacity * 0.9 / usage)
            s.nowMs - lastCutMs >= probeMs -> { probing = true; target * (1 + minOf(0.15, 0.03 + 0.02 * climbs++)) }
            else -> return null
        }
        next = maxOf(next, target)
        lastStepMs = s.nowMs
        return set(next, force = false)
    }

    // What got through over the last spanMs, stalls left out: the link's speed.
    private fun sentRate(nowMs: Long, spanMs: Long): Double {
        var bytes = 0L
        var ms = 0L
        for (i in window.indices.reversed()) {
            if (i == 0 || nowMs - window[i - 1].nowMs > spanMs) break
            if (stalled[i]) continue
            bytes += window[i].sentBytes
            ms += window[i].nowMs - window[i - 1].nowMs
        }
        return if (ms > 0) bytes * 8_000.0 / ms else 0.0
    }

    // Bits per second over the window, from a byte count per sample (the first sample only marks the start).
    private inline fun rate(bytes: (Sample) -> Long): Double {
        if (window.size < 2) return 0.0
        val ms = window.last().nowMs - window.first().nowMs
        if (ms <= 0) return 0.0
        var total = 0L
        for (i in 1 until window.size) total += bytes(window[i])
        return total * 8_000.0 / ms
    }

    private fun set(next: Double, force: Boolean): Int? {
        target = next.coerceIn(floor.toDouble(), ceiling.toDouble())
        val bps = rounded(target)
        val small = kotlin.math.abs(bps - bitrate) < bitrate * MIN_CHANGE && bps != ceiling && bps != floor
        if (bps == bitrate || (!force && small)) return null
        bitrate = bps
        return bps
    }

    private fun rounded(bps: Double): Int = ((bps / 100_000).toLong() * 100_000).toInt().coerceIn(floor, ceiling)
}
