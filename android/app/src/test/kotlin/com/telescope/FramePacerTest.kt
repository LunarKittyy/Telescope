package com.telescope

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import kotlin.random.Random

class FramePacerTest {
    // A camera at 30 fps for ten seconds, its timestamps wobbling by up to 2 ms; returns the kept timestamps
    private fun kept(cap: Int?, wobbleMs: Int = 2, seed: Int = 1): List<Long> {
        val rnd = Random(seed)
        val pacer = FramePacer()
        return (0 until 300).map { it * 33_333_333L + rnd.nextLong(-wobbleMs * 1_000_000L, wobbleMs * 1_000_000L + 1) }
            .filter { pacer.keep(it, cap) }
    }

    @Test
    fun `keeps the rate asked for, evenly`() {
        for (cap in listOf(24, 20, 15)) {
            val ts = kept(cap)
            assertTrue(ts.size in cap * 10 - 2..cap * 10 + 2, "$cap fps kept ${ts.size} in 10 s")
            val gaps = ts.zipWithNext { a, b -> (b - a) / 1_000_000 }
            assertTrue(gaps.all { it <= 70 }, "$cap fps: no gap over two camera frames, got ${gaps.max()} ms")
        }
    }

    @Test
    fun `no cap, or one at the camera's rate, keeps every frame`() {
        assertEquals(300, kept(null).size)
        assertEquals(300, kept(30).size)
    }

    @Test
    fun `starts over after a gap instead of passing a burst`() {
        val pacer = FramePacer()
        var t = 0L
        repeat(30) { pacer.keep(t, 15); t += 33_333_333L }
        t += 2_000_000_000L  // the camera paused
        val after = (0 until 6).count { pacer.keep(t + it * 33_333_333L, 15) }
        assertEquals(3, after)
    }
}
