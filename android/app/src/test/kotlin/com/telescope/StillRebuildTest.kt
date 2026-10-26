package com.telescope

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

class StillRebuildTest {
    @Test
    fun `a rebuilt session of the same camera doesn't end a photo`() {
        assertFalse(StillRebuild.interrupts(StillRebuild.Cause.SESSION))
    }

    @Test
    fun `a new lens, a reopen or the camera stopping ends a photo`() {
        assertTrue(StillRebuild.interrupts(StillRebuild.Cause.LENS))
        assertTrue(StillRebuild.interrupts(StillRebuild.Cause.REOPEN))
        assertTrue(StillRebuild.interrupts(StillRebuild.Cause.OFF))
    }

    @Test
    fun `the text says what ended the photo`() {
        assertEquals("The lens changed during the photo", StillRebuild.text(StillRebuild.Cause.LENS))
        assertEquals("The stream restarted during the photo", StillRebuild.text(StillRebuild.Cause.REOPEN))
        assertEquals("The camera stopped during the photo", StillRebuild.text(StillRebuild.Cause.OFF))
    }

    @Test
    fun `captures a rebuild dropped are asked again once in the new session`() {
        assertEquals(StillRebuild.Lost.RETRY, StillRebuild.afterLostSession(retried = false, outputsInSession = true, sessionUp = true))
        assertEquals(StillRebuild.Lost.FAIL, StillRebuild.afterLostSession(retried = true, outputsInSession = true, sessionUp = true))
    }

    @Test
    fun `a new session without the photo outputs takes the photo in its own session`() {
        assertEquals(StillRebuild.Lost.ALONE, StillRebuild.afterLostSession(retried = false, outputsInSession = false, sessionUp = true))
    }

    @Test
    fun `no stream session to go back to fails the photo`() {
        assertEquals(StillRebuild.Lost.FAIL, StillRebuild.afterLostSession(retried = false, outputsInSession = true, sessionUp = false))
    }

    @Test
    fun `without a photo nothing waits`() {
        val hold = PhotoHold<String>()
        assertFalse(hold.hold("frameRate", "cap 24"))
        assertTrue(hold.end().isEmpty())
    }

    @Test
    fun `automatic changes during a photo wait until it ends`() {
        val hold = PhotoHold<String>()
        hold.start()
        assertTrue(hold.hold("frameRate", "cap 24"))
        assertTrue(hold.hold("thermal", "severe"))
        assertTrue(hold.busy)
        assertEquals(listOf("cap 24", "severe"), hold.end())
        assertFalse(hold.busy)
        assertFalse(hold.hold("frameRate", "cap 30"))
    }

    @Test
    fun `only the newest change of a kind runs after the photo`() {
        val hold = PhotoHold<String>()
        hold.start()
        hold.hold("frameRate", "cap 24")
        hold.hold("capCheck", "check")
        hold.hold("frameRate", "cap 15")
        assertEquals(listOf("check", "cap 15"), hold.end())
    }

    @Test
    fun `a photo's own hitch can't cascade into rebuilds`() {
        // Dynamic steps several times while the first photo is set up and taken: one rebuild afterwards, not one each
        val hold = PhotoHold<() -> Unit>()
        var rebuilds = 0
        hold.start()
        repeat(5) { if (!hold.hold("frameRate") { rebuilds++ }) rebuilds++ }
        assertEquals(0, rebuilds)
        hold.end().forEach { it() }
        assertEquals(1, rebuilds)
    }
}
