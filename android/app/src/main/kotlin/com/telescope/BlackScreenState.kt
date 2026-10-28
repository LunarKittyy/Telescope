package com.telescope

// Black screen's taps, kept apart from the views so it runs on the JVM. Same rules as Browser camera's page
// (desktop/telescope/web/app.js): the first tap brings the hint back, a second one while it shows wakes the screen.
class BlackScreenState {
    var black = false
        private set
    private var tapped = false
    private var wokeAtMs = Long.MIN_VALUE / 2

    enum class Tap { ShowHint, Wake }

    fun enter() {
        black = true
        tapped = false
    }

    fun tap(nowMs: Long): Tap {
        if (!black) return Tap.Wake
        if (!tapped) {
            tapped = true
            return Tap.ShowHint
        }
        wokeAtMs = nowMs
        leave()
        return Tap.Wake
    }

    // The hint faded: the next tap starts over
    fun hintGone() {
        tapped = false
    }

    fun leave() {
        black = false
        tapped = false
    }

    // A fast double tap's third tap lands on whatever is under the black screen once it's gone
    fun justWoke(nowMs: Long) = nowMs - wokeAtMs < WAKE_GUARD_MS

    // Only a stream that really ended wakes the screen; Recovering and other in-between states stay black
    fun shouldLeave(streaming: Boolean, busy: Boolean) = black && !streaming && !busy

    companion object {
        const val HINT_MS = 4000L
        const val WAKE_GUARD_MS = 500L
    }
}
