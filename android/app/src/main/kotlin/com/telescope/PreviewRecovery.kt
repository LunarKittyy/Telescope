package com.telescope

// Holds frames after a still until preview AE settles, then rejects older queued frames and H.264 deltas.
class PreviewRecovery {
    companion object { const val MAX_MS = 1_500L }

    private var serial = 0L
    private var capturing = false
    private var deadlineMs: Long? = null
    private var latestTimestampNs = Long.MIN_VALUE
    private var firstTimestampNs = Long.MIN_VALUE
    private var waitingForKey = false
    private var keyRequestedAtMs: Long? = null
    @Volatile var holding = false
        private set

    @Synchronized
    fun start() {
        serial++
        capturing = true
        holding = true
        deadlineMs = null
        latestTimestampNs = Long.MIN_VALUE
        firstTimestampNs = Long.MIN_VALUE
        waitingForKey = true
        keyRequestedAtMs = null
    }

    @Synchronized
    fun resume(nowMs: Long) {
        if (!holding) return
        capturing = false
        deadlineMs = nowMs + MAX_MS
    }

    @Synchronized
    fun recovering(): Boolean = holding && !capturing

    @Synchronized
    fun requestTag(): Long? = if (holding && !capturing) serial else null

    @Synchronized
    fun result(timestampNs: Long, ready: Boolean, tag: Long?): Boolean {
        if (!holding || capturing || tag != serial) return false
        latestTimestampNs = timestampNs
        return if (ready) release() else false
    }

    @Synchronized
    fun timeout(nowMs: Long): Boolean =
        if (holding && !capturing && deadlineMs?.let { nowMs >= it } == true) release() else false

    private fun release(): Boolean {
        holding = false
        firstTimestampNs = latestTimestampNs
        deadlineMs = null
        return true
    }

    @Synchronized
    fun keep(timestampNs: Long?, key: Boolean? = null): Boolean {
        if (holding || (timestampNs != null && timestampNs < firstTimestampNs)) return false
        if (key != null && waitingForKey) {
            if (!key) return false
            waitingForKey = false
        }
        return true
    }

    @Synchronized
    fun requestKeyframe(nowMs: Long): Boolean {
        if (holding || !waitingForKey || keyRequestedAtMs?.let { nowMs - it < 250L } == true) return false
        keyRequestedAtMs = nowMs
        return true
    }

    @Synchronized
    fun reset() {
        capturing = false
        holding = false
        deadlineMs = null
        firstTimestampNs = Long.MIN_VALUE
        waitingForKey = false
    }
}
