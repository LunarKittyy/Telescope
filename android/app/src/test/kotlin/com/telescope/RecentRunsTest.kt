package com.telescope

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Test

class RecentRunsTest {
    @Test
    fun `keeps the newest three, newest first`() {
        var runs = emptyList<String>()
        for (i in 1..5) runs = RecentRuns.add(runs, "run $i\n")
        assertEquals(listOf("run 5", "run 4", "run 3"), runs)
    }

    @Test
    fun `survives a round trip through the file text`() {
        val runs = listOf("Stream ended: a\nCurrent state: Idle", "Stream ended: b\n  Idle -> Stopping")
        assertEquals(runs, RecentRuns.decode(RecentRuns.encode(runs)))
    }

    @Test
    fun `an empty or missing file is no runs`() {
        assertEquals(emptyList<String>(), RecentRuns.decode(""))
    }
}
