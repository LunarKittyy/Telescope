package com.telescope

// Holding auto exposure and auto white balance where they are (Camera2's AE and AWB locks), kept apart from Camera2 so it can be tested
class ExposureLock {
    enum class Phase { OFF, LOCKING, LOCKED }

    @Volatile var phase = Phase.OFF
        private set

    // What goes in the request: both locks stay set from the ask until it is let go
    val asked: Boolean get() = phase != Phase.OFF

    // The phone's wording for the state
    val wire: String get() = when (phase) {
        Phase.OFF -> "off"
        Phase.LOCKING -> "locking"
        Phase.LOCKED -> "locked"
    }

    // false: this lens can't lock both, and nothing changes
    fun request(available: Boolean): Boolean {
        if (!available) return false
        if (phase == Phase.OFF) phase = Phase.LOCKING
        return true
    }

    fun release() { phase = Phase.OFF }

    // A capture result: confirmed only by one of a request that carried the locks, with each of auto exposure and auto
    // white balance reporting LOCKED, or held by hand already (manual exposure, manual gains, where a lock means nothing)
    fun onResult(requestHadLocks: Boolean, aeLocked: Boolean, awbLocked: Boolean) {
        if (phase == Phase.LOCKING && requestHadLocks && aeLocked && awbLocked) phase = Phase.LOCKED
    }

    // The camera closed or changed lens: the next one starts from a fresh metering, so the lock goes. A rebuilt session on
    // the same camera keeps it.
    fun onRebuild(cause: StillRebuild.Cause) {
        if (cause != StillRebuild.Cause.SESSION) release()
    }

    companion object {
        // Both have to be there: a lock of one alone would still let the other drift
        fun available(aeLockAvailable: Boolean, awbLockAvailable: Boolean) = aeLockAvailable && awbLockAvailable
    }
}
