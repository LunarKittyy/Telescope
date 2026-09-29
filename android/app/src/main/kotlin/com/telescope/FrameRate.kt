package com.telescope

/**
 * Frames per second over the last [windowMs], from a tick per frame. The phone reports what it actually makes, so the
 * computer can tell a camera giving fewer frames (a dim room at 60 fps) from a link losing them.
 */
class FrameRate(private val windowMs: Long = 2_000L, private val clock: () -> Long = System::currentTimeMillis) {
    private val times = ArrayDeque<Long>()

    @Synchronized
    fun tick() {
        val now = clock()
        times.addLast(now)
        trim(now)
    }

    /** 0 until there's a window's worth to go by, or after frames stopped. */
    @Synchronized
    fun fps(): Double {
        val now = clock()
        trim(now)
        if (times.size < 2 || now - times.first() < windowMs / 2) return 0.0
        return (times.size - 1) * 1000.0 / (times.last() - times.first()).coerceAtLeast(1)
    }

    private fun trim(now: Long) {
        while (times.isNotEmpty() && now - times.first() > windowMs) times.removeFirst()
    }
}
