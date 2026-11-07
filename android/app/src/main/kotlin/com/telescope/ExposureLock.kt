package com.telescope

// Holding auto exposure and auto white balance where they are (Camera2's AE and AWB locks), kept apart from Camera2 so it can be tested
class ExposureLock {
    enum class Phase { OFF, LOCKING, LOCKED }

    @Volatile var phase = Phase.OFF
        private set

    // Counts the locks asked for. It goes in the request as its tag, so a result of an earlier lock's request can't confirm a later one.
    @Volatile var generation = 0
        private set

    // When the desktop last asked (the clock the caller passes in); the lock lapses if it stops asking
    @Volatile private var askedAt = 0L

    // What goes in the request: both locks stay set from the ask until it is let go
    val asked: Boolean get() = phase != Phase.OFF

    // The phone's wording for the state
    val wire: String get() = when (phase) {
        Phase.OFF -> "off"
        Phase.LOCKING -> "locking"
        Phase.LOCKED -> "locked"
    }

    // false: this lens can't lock both, and nothing changes. Asking again while held renews it.
    fun request(available: Boolean, now: Long): Boolean {
        if (!available) return false
        askedAt = now
        if (phase == Phase.OFF) {
            generation++
            phase = Phase.LOCKING
        }
        return true
    }

    fun release() { phase = Phase.OFF }

    // A held lock the desktop stopped renewing (it crashed, or the link went) lets go by itself. True when it just did.
    fun lapse(now: Long): Boolean {
        if (asked && now - askedAt > LEASE_MS) {
            release()
            return true
        }
        return false
    }

    // How long until a held lock is due to lapse (at least 1), or null when nothing is held; for a check that doesn't wait on capture results
    fun lapseIn(now: Long): Long? = if (asked) maxOf(1L, askedAt + LEASE_MS + 1 - now) else null

    // A capture result: confirmed only by one of the request of this lock (its tag is the generation), with each of auto
    // exposure and auto white balance holding, or set by hand already (where a lock means nothing)
    fun onResult(requestGeneration: Int?, aeLocked: Boolean, awbLocked: Boolean) {
        if (phase == Phase.LOCKING && requestGeneration == generation && aeLocked && awbLocked) phase = Phase.LOCKED
    }

    // The camera closed or changed lens: the next one starts from a fresh metering, so the lock goes. A rebuilt session on
    // the same camera keeps it.
    fun onRebuild(cause: StillRebuild.Cause) {
        if (cause != StillRebuild.Cause.SESSION) release()
    }

    companion object {
        // The desktop renews every few seconds while it holds a lock
        const val LEASE_MS = 20_000L

        // Both have to be there: a lock of one alone would still let the other drift
        fun available(aeLockAvailable: Boolean, awbLockAvailable: Boolean) = aeLockAvailable && awbLockAvailable

        private const val LOCKED = 3  // CONTROL_AE_STATE_LOCKED and CONTROL_AWB_STATE_LOCKED

        // Auto exposure's state says it holds; metadata the camera has to give once it can lock
        fun aeHeld(byHand: Boolean, state: Int?) = byHand || state == LOCKED

        // A camera that leaves the white balance state out (optional on LEGACY ones) is taken at its word that AWB_LOCK was set
        fun awbHeld(byHand: Boolean, state: Int?) = byHand || state == null || state == LOCKED
    }
}
