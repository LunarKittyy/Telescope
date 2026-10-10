package com.telescope

/**
 * Dynamic's second step: when even the lowest bitrate the link carries is too thin for the frame rate asked for,
 * fewer frames each get more of it. Fewer sharp frames beat many smeared ones, and it's what the "can't keep up"
 * note told people to do by hand.
 *
 * The signal is the bitrate [DynamicBitrate] settled on, which tracks what the link carries, against Auto's bitrate
 * for each rate. Well under it for a while steps down a rung; comfortably over the next rung's for longer steps back
 * up. Each step restarts the camera's session (the rate is part of its setup), so it waits between steps and never
 * goes under [MIN_FPS].
 *
 * Only rates the camera can be held to are rungs ([canCapAt]): one whose ranges all go past a rate would keep
 * running faster than it, so stepping there would rebuild the session for nothing. A stall isn't a slower link
 * ([DynamicBitrate] drops to its floor for one and climbs back after), so the time after one doesn't count.
 *
 * Pure, so it runs as a JVM test; [update] runs on the encoder's thread like [DynamicBitrate.update].
 */
class DynamicFrameRate(private val askedFps: Int, canCapAt: (Int) -> Boolean = { true }) {

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

    /** Feed what Dynamic's bitrate is now and when the link last stalled; returns the new rate when it should
     *  change, else null. */
    @Synchronized
    fun update(nowMs: Long, bitrate: Int, width: Int, height: Int, lastStallMs: Long = Long.MIN_VALUE / 2): Int? {
        val tooThin = rung < rungs.size - 1 && nowMs - lastStallMs >= AFTER_STALL_MS &&
            bitrate < H264Stream.defaultBitrate(width, height, fps) * LOW
        val room = rung > 0 && bitrate >= H264Stream.defaultBitrate(width, height, rungs[rung - 1]) * HIGH
        lowSinceMs = if (tooThin) (if (lowSinceMs < 0) nowMs else lowSinceMs) else -1
        roomSinceMs = if (room) (if (roomSinceMs < 0) nowMs else roomSinceMs) else -1
        if (nowMs - lastStepMs < SETTLE_MS) return null
        val next = when {
            lowSinceMs >= 0 && nowMs - lowSinceMs >= DOWN_MS -> rung + 1
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
