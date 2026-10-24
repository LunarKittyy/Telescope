package com.telescope

/**
 * Dynamic's second step: when even the lowest bitrate the link carries is too thin for the frame rate asked for,
 * fewer frames each get more of it. Fewer sharp frames beat many smeared ones, and it's what the "can't keep up"
 * note told people to do by hand.
 *
 * The signal is the bitrate [DynamicBitrate] settled on, which tracks what the link carries, against Auto's bitrate
 * for each rate. Well under it for a while steps down, straight to the first rung it's enough for; comfortably over
 * the next rung's for longer steps back up. A step has the encoder drop frames where it can (Android 10+), else it
 * restarts the camera's session (the rate is part of its setup), so it waits between steps and never goes under
 * [MIN_FPS].
 *
 * Only rates the camera can be held to are rungs ([canCapAt]): one whose ranges all go past a rate would keep
 * running faster than it, so stepping there would rebuild the session for nothing. A stall isn't a slower link
 * ([DynamicBitrate] drops to its floor for one and climbs back after), so the time after one doesn't count.
 *
 * Pure, so it runs as a JVM test; [update] runs on the encoder's thread like [DynamicBitrate.update].
 */
// lens: whatever the rungs were worked out for (the controller passes the camera), so a caller can tell they're stale
class DynamicFrameRate(val askedFps: Int, val lens: Any? = null, canCapAt: (Int) -> Boolean = { true }) {

    companion object {
        const val MIN_FPS = 15
        private val RUNGS = listOf(30, 24, 20, MIN_FPS)
        const val LOW = 0.35       // under this share of Auto's bitrate at the current rate: too thin
        const val HIGH = 0.6       // at least this share of Auto's at the next rate up: room to go back
        const val DOWN_MS = 4_000L   // too thin for this long steps down
        const val UP_MS = 15_000L    // room for this long steps up
        const val SETTLE_MS = 10_000L  // after a step, the link gets this long before the next one
        const val AFTER_STALL_MS = 15_000L  // a stall's cut and the climb back aren't the link being slow
    }

    /** The rates it can run at, fastest (the one asked for) first. */
    val rungs: List<Int> = (listOf(askedFps) + RUNGS.filter { it < askedFps && canCapAt(it) }).distinct()

    @Volatile private var rung = 0  // read from the camera thread too
    private var lowSinceMs = -1L
    private var roomSinceMs = -1L
    private var lastStepMs = Long.MIN_VALUE / 2

    /** The rate to run at; the one asked for until the link says otherwise. */
    val fps: Int get() = rungs[rung]

    /** The cap in force, or null at the rate asked for. */
    val cap: Int? get() = if (rung == 0) null else fps

    /** Feed what Dynamic's bitrate is now and when a stall last cut it; returns the new rate when it should
     *  change, else null. */
    @Synchronized
    fun update(nowMs: Long, bitrate: Int, width: Int, height: Int, lastStallCutMs: Long = Long.MIN_VALUE / 2): Int? {
        val tooThin = rung < rungs.size - 1 && nowMs - lastStallCutMs >= AFTER_STALL_MS &&
            bitrate < H264Stream.defaultBitrate(width, height, fps) * LOW
        val room = rung > 0 && bitrate >= H264Stream.defaultBitrate(width, height, rungs[rung - 1]) * HIGH
        lowSinceMs = if (tooThin) (if (lowSinceMs < 0) nowMs else lowSinceMs) else -1
        roomSinceMs = if (room) (if (roomSinceMs < 0) nowMs else roomSinceMs) else -1
        if (nowMs - lastStepMs < SETTLE_MS) return null
        val next = when {
            // Far too thin goes straight to the rung it fits, rather than one every SETTLE_MS
            lowSinceMs >= 0 && nowMs - lowSinceMs >= DOWN_MS ->
                (rung + 1 until rungs.size).firstOrNull { bitrate >= H264Stream.defaultBitrate(width, height, rungs[it]) * LOW }
                    ?: (rungs.size - 1)
            roomSinceMs >= 0 && nowMs - roomSinceMs >= UP_MS -> rung - 1
            else -> return null
        }
        rung = next
        lastStepMs = nowMs
        lowSinceMs = -1
        roomSinceMs = -1
        return fps
    }
}

// Android's own thermal status (10 and newer) caps the rate before the phone gets hot enough for an alert to stop it
object HeatStep {
    const val SEVERE = 3    // PowerManager.THERMAL_STATUS_SEVERE
    const val CRITICAL = 4  // PowerManager.THERMAL_STATUS_CRITICAL
    private val RUNGS = listOf(24, 20, DynamicFrameRate.MIN_FPS)

    /** The rate to cap at for this status, or null when it needn't (or the camera can't be held under the rate asked for). */
    fun cap(status: Int, askedFps: Int, canCapAt: (Int) -> Boolean = { true }): Int? {
        val limit = when {
            status >= CRITICAL -> DynamicFrameRate.MIN_FPS
            status >= SEVERE -> 24
            else -> return null
        }
        if (askedFps <= limit) return null
        return RUNGS.filter { it <= limit && canCapAt(it) }.maxOrNull()
    }

    // At critical the picture drops to this (its short side), on top of the frame rate
    const val CRITICAL_SHORT_SIDE = 720

    /** The size to stream at for this status instead of [asked], or null when it needn't change: at critical, the largest
     *  of [sizes] with the same shape whose short side is at most [CRITICAL_SHORT_SIDE] (and [fits], for Light). */
    fun size(status: Int, asked: Pair<Int, Int>, sizes: List<Pair<Int, Int>>,
             fits: (Int, Int) -> Boolean = { _, _ -> true }): Pair<Int, Int>? {
        if (status < CRITICAL || minOf(asked.first, asked.second) <= CRITICAL_SHORT_SIDE) return null
        val shape = asked.first.toFloat() / asked.second
        return sizes.filter { (w, h) ->
            minOf(w, h) <= CRITICAL_SHORT_SIDE && kotlin.math.abs(w.toFloat() / h - shape) < 0.01f && fits(w, h)
        }.maxByOrNull { (w, h) -> w * h }
    }

    const val FORECAST_S = 30  // how far ahead to ask Android about (Android 11 and newer)
    const val FORECAST_EVERY_MS = 10_000L  // Android wants at most one ask a second; this is plenty

    /** The status to act on now: Android's, or severe already when its forecast says that's [FORECAST_S] away
     *  (headroom 1.0 is where severe throttling starts), so the rate eases off before the phone gets that hot. */
    fun status(current: Int, headroom: Float): Int =
        if (current < SEVERE && headroom.isFinite() && headroom >= 1f) SEVERE else current
}
