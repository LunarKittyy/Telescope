package com.telescope

import android.animation.ValueAnimator
import android.view.View
import kotlin.math.abs
import kotlin.math.min

// The kit's motion on the phone (rust/crates/ui-kit/ui/widgets/motion.slint): each motion is a spring, and a spring given a new target keeps its speed
object Motion {
    // Android's animator duration scale is 0 (Settings > Developer options, or Remove animations): nothing moves and every colour snaps
    fun on(): Boolean = ValueAnimator.areAnimatorsEnabled()

    const val TONE_UP_MS = 90
    const val TONE_DOWN_MS = 140
    const val FLICK_MS = 260
    const val WORM_LEAD_MS = 110
    const val WORM_TRAIL_MS = 280
    const val FADE_IN_MS = 120
    const val FADE_OUT_MS = 100
}

// A mass on a spring towards `target`. `omega` is how stiff it is, `zeta` how damped (1 never passes the target, below 1 passes it a little).
class Spring(var value: Float, private var omega: Float, private val zeta: Float) {
    var target = value
    private var velocity = 0f

    val atRest get() = abs(value - target) < 0.0005f && abs(velocity) < 0.005f

    fun snap(to: Float) {
        value = to
        target = to
        velocity = 0f
    }

    // Takes `ms` to settle from here on, without overshoot (a colour going up is quicker than going down)
    fun settleIn(ms: Int) {
        omega = 5.83f / (ms / 1000f)
    }

    fun step(seconds: Float) {
        var left = seconds
        while (left > 0f) {
            val h = min(left, 0.004f)
            velocity += (-omega * omega * (value - target) - 2f * zeta * omega * velocity) * h
            value += velocity * h
            left -= h
        }
        if (atRest) snap(target)
    }

    companion object {
        // A spring that never overshoots and is within 2% of its target after `ms`, the way a Slint spring(0.0) takes its duration
        fun settling(ms: Int, start: Float = 0f) = Spring(start, 5.83f / (ms / 1000f), 1f)

        // A spring with a little overshoot whose period is `ms`: it is within 2% of its target after about one period
        fun bouncing(ms: Int, bounce: Float, start: Float = 0f): Spring {
            val zeta = 1f - bounce
            return Spring(start, 4f / (zeta * ms / 1000f), zeta)
        }
    }
}

// Calls `onFrame(seconds since the last frame)` once a frame while it returns true, and stops when it returns false
class Frames(private val view: View, private val onFrame: (Float) -> Boolean) {
    private var running = false
    private var last = 0L

    fun start() {
        if (running) return
        running = true
        last = 0L
        view.postOnAnimation(tick)
    }

    private val tick = object : Runnable {
        override fun run() {
            val now = System.nanoTime()
            val dt = if (last == 0L) 1f / 60f else min((now - last) / 1e9f, 0.05f)
            last = now
            if (onFrame(dt)) view.postOnAnimation(this) else running = false
        }
    }
}
