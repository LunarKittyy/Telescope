package com.telescope

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Test

class SetupStepsTest {

    @Test
    fun `a permission is asked for until Android stops showing its prompt`() {
        assertEquals(SetupSteps.Action.NONE, SetupSteps.action(granted = true, askedBefore = true, showRationale = false))
        assertEquals(SetupSteps.Action.ASK, SetupSteps.action(granted = false, askedBefore = false, showRationale = false))
        assertEquals(SetupSteps.Action.ASK, SetupSteps.action(granted = false, askedBefore = true, showRationale = true))
        assertEquals(SetupSteps.Action.SETTINGS, SetupSteps.action(granted = false, askedBefore = true, showRationale = false))
    }

    @Test
    fun `the first step not done is the current one`() {
        assertEquals(0, SetupSteps.current(listOf(false, false, false)))
        assertEquals(1, SetupSteps.current(listOf(true, false, false)))
        assertEquals(2, SetupSteps.current(listOf(true, true, false)))
        assertEquals(-1, SetupSteps.current(listOf(true, true, true)))
    }
}
