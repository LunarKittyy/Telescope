package com.telescope

import android.hardware.camera2.CaptureRequest
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Test

class CameraRequestSelectionTest {

    @Test
    fun `empty AE ranges omit the request key`() {
        assertNull(CameraRequestSelection.pickAeFpsRange(emptyList(), 30))
    }

    @Test
    fun `continuous AF prefers video then picture then auto`() {
        val all = setOf(
            CaptureRequest.CONTROL_AF_MODE_CONTINUOUS_VIDEO,
            CaptureRequest.CONTROL_AF_MODE_CONTINUOUS_PICTURE,
            CaptureRequest.CONTROL_AF_MODE_AUTO,
        )
        assertEquals(
            CaptureRequest.CONTROL_AF_MODE_CONTINUOUS_VIDEO,
            CameraRequestSelection.pickAfMode(all, wantContinuousVideo = true),
        )

        assertEquals(
            CaptureRequest.CONTROL_AF_MODE_CONTINUOUS_PICTURE,
            CameraRequestSelection.pickAfMode(
                setOf(CaptureRequest.CONTROL_AF_MODE_CONTINUOUS_PICTURE,
                      CaptureRequest.CONTROL_AF_MODE_AUTO),
                wantContinuousVideo = true,
            ),
        )
        assertEquals(
            CaptureRequest.CONTROL_AF_MODE_AUTO,
            CameraRequestSelection.pickAfMode(
                setOf(CaptureRequest.CONTROL_AF_MODE_AUTO),
                wantContinuousVideo = true,
            ),
        )
    }

    @Test
    fun `manual focus selection never chooses continuous video`() {
        assertEquals(
            CaptureRequest.CONTROL_AF_MODE_CONTINUOUS_PICTURE,
            CameraRequestSelection.pickAfMode(
                setOf(
                    CaptureRequest.CONTROL_AF_MODE_CONTINUOUS_VIDEO,
                    CaptureRequest.CONTROL_AF_MODE_CONTINUOUS_PICTURE,
                ),
                wantContinuousVideo = false,
            ),
        )
        assertEquals(
            CaptureRequest.CONTROL_AF_MODE_OFF,
            CameraRequestSelection.pickAfMode(
                setOf(CaptureRequest.CONTROL_AF_MODE_CONTINUOUS_VIDEO),
                wantContinuousVideo = false,
            ),
        )
    }

    @Test
    fun `noise reduction keeps supported request and uses safe fallbacks`() {
        val available = setOf(
            CaptureRequest.NOISE_REDUCTION_MODE_OFF,
            CaptureRequest.NOISE_REDUCTION_MODE_FAST,
            CaptureRequest.NOISE_REDUCTION_MODE_HIGH_QUALITY,
        )
        assertEquals(
            CaptureRequest.NOISE_REDUCTION_MODE_HIGH_QUALITY,
            CameraRequestSelection.pickNrMode(
                available,
                CaptureRequest.NOISE_REDUCTION_MODE_HIGH_QUALITY,
            ),
        )
        assertEquals(
            CaptureRequest.NOISE_REDUCTION_MODE_FAST,
            CameraRequestSelection.pickNrMode(available, 999),
        )
        assertEquals(
            CaptureRequest.NOISE_REDUCTION_MODE_OFF,
            CameraRequestSelection.pickNrMode(
                setOf(CaptureRequest.NOISE_REDUCTION_MODE_OFF),
                999,
            ),
        )
        assertNull(CameraRequestSelection.pickNrMode(emptySet(), 999))
        assertNull(CameraRequestSelection.pickNrMode(setOf(777), 999))
    }

    @Test
    fun `edge processing keeps supported request and uses safe fallbacks`() {
        val available = setOf(
            CaptureRequest.EDGE_MODE_OFF,
            CaptureRequest.EDGE_MODE_FAST,
            CaptureRequest.EDGE_MODE_HIGH_QUALITY,
        )
        assertEquals(
            CaptureRequest.EDGE_MODE_HIGH_QUALITY,
            CameraRequestSelection.pickEdgeMode(
                available,
                CaptureRequest.EDGE_MODE_HIGH_QUALITY,
            ),
        )
        assertEquals(
            CaptureRequest.EDGE_MODE_FAST,
            CameraRequestSelection.pickEdgeMode(available, 999),
        )
        assertEquals(
            CaptureRequest.EDGE_MODE_OFF,
            CameraRequestSelection.pickEdgeMode(setOf(CaptureRequest.EDGE_MODE_OFF), 999),
        )
        assertNull(CameraRequestSelection.pickEdgeMode(emptySet(), 999))
        assertNull(CameraRequestSelection.pickEdgeMode(setOf(777), 999))
    }

    @Test
    fun `integer clamps handle bounds and inverted ranges`() {
        assertEquals(10, CameraRequestSelection.clamp(5, 10, 20))
        assertEquals(15, CameraRequestSelection.clamp(15, 10, 20))
        assertEquals(20, CameraRequestSelection.clamp(25, 10, 20))
        assertEquals(25, CameraRequestSelection.clamp(25, 20, 10))
    }

    @Test
    fun `long clamps handle bounds and inverted ranges`() {
        assertEquals(10L, CameraRequestSelection.clamp(5L, 10L, 20L))
        assertEquals(15L, CameraRequestSelection.clamp(15L, 10L, 20L))
        assertEquals(20L, CameraRequestSelection.clamp(25L, 10L, 20L))
        assertEquals(25L, CameraRequestSelection.clamp(25L, 20L, 10L))
    }

    @Test
    fun `float clamps handle bounds and inverted ranges`() {
        assertEquals(10f, CameraRequestSelection.clamp(5f, 10f, 20f))
        assertEquals(15f, CameraRequestSelection.clamp(15f, 10f, 20f))
        assertEquals(20f, CameraRequestSelection.clamp(25f, 10f, 20f))
        assertEquals(25f, CameraRequestSelection.clamp(25f, 20f, 10f))
    }
}
