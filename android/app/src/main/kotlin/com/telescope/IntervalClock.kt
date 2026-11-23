package com.telescope

import android.os.SystemClock

// Milliseconds for interval timers: keeps counting in deep sleep and ignores wall-clock changes (nanoTime stops asleep).
object IntervalClock {
    fun nowMs(): Long = try { SystemClock.elapsedRealtime() } catch (_: RuntimeException) { System.nanoTime() / 1_000_000 } // JVM unit tests only have android.jar stubs
}
