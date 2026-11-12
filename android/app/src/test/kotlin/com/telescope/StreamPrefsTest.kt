package com.telescope

import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

class StreamPrefsTest {
    @Test
    fun waitingSwitchFollowsOnlyItsOwnKey() {
        assertTrue(StreamPrefs.isWaitForComputerKey("wait_for_computer"))
        assertFalse(StreamPrefs.isWaitForComputerKey("local_only"))
        assertFalse(StreamPrefs.isWaitForComputerKey(null))
    }
}
