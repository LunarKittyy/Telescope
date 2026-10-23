package com.telescope

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Test

class LensLabelTest {
    @Test
    fun `the label sent over the wire is the one older desktops read`() {
        assertEquals("Back ~24mm OIS [auto]", LensLabel.english("back", 24, true, "auto"))
        assertEquals("Front ~25mm", LensLabel.english("front", 25, false, ""))
        assertEquals("Back ~13mm [phys]", LensLabel.english("back", 13, false, "physical"))
        assertEquals("Ext ?", LensLabel.english("external", 0, false, ""))
    }

    @Test
    fun `the label on the phone comes from its strings`() {
        assertEquals("Back ~24mm OIS [auto]", LensLabel.local(EnglishStrings, "back", 24, true, "auto"))
        assertEquals("Front ~25mm", LensLabel.local(EnglishStrings, "front", 25, false, ""))
        assertEquals("Back ~13mm", LensLabel.local(EnglishStrings, "back", 13, false, "physical"))
    }

    @Test
    fun `a codec failure keeps its English text and says whether trying again could help`() {
        val cant = CodecFailure(CodecFailure.CAMERA_CANT_FEED, 3840, 2160)
        assertEquals("The camera can't feed H.264 at 3840x2160 on this phone", cant.text)
        assertEquals(true, cant.unsupported)
        val stopped = CodecFailure(CodecFailure.ENCODER_STOPPED, 1920, 1080)
        assertEquals("The phone's H.264 encoder stopped", stopped.text)
        assertEquals(false, stopped.unsupported)
    }
}
