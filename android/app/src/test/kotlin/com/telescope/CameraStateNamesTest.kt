package com.telescope

import android.hardware.camera2.CaptureResult
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Test

class CameraStateNamesTest {
    @Test
    fun `focus and exposure states go to the desktop by name`() {
        assertEquals("scanning", afStateName(CaptureResult.CONTROL_AF_STATE_PASSIVE_SCAN))
        assertEquals("scanning", afStateName(CaptureResult.CONTROL_AF_STATE_ACTIVE_SCAN))
        assertEquals("focused", afStateName(CaptureResult.CONTROL_AF_STATE_PASSIVE_FOCUSED))
        assertEquals("locked_unfocused", afStateName(CaptureResult.CONTROL_AF_STATE_NOT_FOCUSED_LOCKED))
        assertEquals("converged", aeStateName(CaptureResult.CONTROL_AE_STATE_CONVERGED))
        assertEquals("flash_required", aeStateName(CaptureResult.CONTROL_AE_STATE_FLASH_REQUIRED))
    }

    @Test
    fun `a state the camera doesn't say is left out`() {
        assertNull(afStateName(null))
        assertNull(aeStateName(null))
        assertNull(afStateName(99))
    }

    @Test
    fun `capabilities go by their Camera2 names`() {
        assertEquals("RAW", cameraCapabilityName(3))
        assertEquals("DYNAMIC_RANGE_TEN_BIT", cameraCapabilityName(18))
        assertEquals("CAP_99", cameraCapabilityName(99))
    }
}
