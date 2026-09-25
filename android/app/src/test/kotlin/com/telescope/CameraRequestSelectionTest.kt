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

class MeteringRectTest {
    private val array = SensorBox(0, 0, 4000, 3000)  // 4:3 sensor

    private fun rect(x: Float, y: Float, w: Int, h: Int, size: Float = 0.1f, a: SensorBox = array) =
        CameraRequestSelection.meteringRect(x, y, size, a, w, h).toList()

    @org.junit.jupiter.api.Test
    fun `a same-aspect stream maps straight onto the array`() {
        // centre, 10% of 3000 = 300 px square
        org.junit.jupiter.api.Assertions.assertEquals(listOf(1850, 1350, 300, 300), rect(0.5f, 0.5f, 1440, 1080))
        org.junit.jupiter.api.Assertions.assertEquals(listOf(850, 600, 300, 300), rect(0.25f, 0.25f, 1440, 1080))
    }

    @org.junit.jupiter.api.Test
    fun `a wider stream is the array's centre band, so y goes through the crop`() {
        // 16:9 of a 4000-wide array is 2250 tall, starting 375 down
        val top = rect(0.5f, 0f, 1920, 1080)
        org.junit.jupiter.api.Assertions.assertEquals(375, top[1])  // clamped into the visible band, not just the array
        val mid = rect(0.5f, 0.5f, 1920, 1080)
        org.junit.jupiter.api.Assertions.assertTrue(kotlin.math.abs(mid[1] + mid[3] / 2 - 1500) <= 1)
        org.junit.jupiter.api.Assertions.assertEquals(225, mid[2])  // 10% of the visible 2250
    }

    @org.junit.jupiter.api.Test
    fun `a narrower stream crops the sides`() {
        // 1:1 of a 4000x3000 array is 3000x3000 starting 500 in
        val left = rect(0f, 0.5f, 1080, 1080)
        org.junit.jupiter.api.Assertions.assertEquals(500, left[0])
    }

    @org.junit.jupiter.api.Test
    fun `corners stay inside the array and its offset is kept`() {
        val offset = SensorBox(8, 8, 4000, 3000)
        val r = rect(1f, 1f, 1440, 1080, a = offset)
        org.junit.jupiter.api.Assertions.assertEquals(8 + 4000 - 300, r[0])
        org.junit.jupiter.api.Assertions.assertEquals(8 + 3000 - 300, r[1])
        val out = rect(-1f, 2f, 1440, 1080, a = offset)
        org.junit.jupiter.api.Assertions.assertEquals(8, out[0])
    }

    @org.junit.jupiter.api.Test
    fun `metering inside a zoom crop maps through the crop`() {
        val crop = CameraRequestSelection.cropRect(array, 2f, 0.5f, 0.5f, 1440, 1080)
        org.junit.jupiter.api.Assertions.assertEquals(listOf(1925, 1425, 150, 150), rect(0.5f, 0.5f, 1440, 1080, a = crop))
    }
}

class CropRectTest {
    private val array = SensorBox(0, 0, 4000, 3000)  // 4:3 sensor

    private fun crop(zoom: Float, x: Float, y: Float, w: Int = 1920, h: Int = 1080) =
        CameraRequestSelection.cropRect(array, zoom, x, y, w, h)

    @org.junit.jupiter.api.Test
    fun `no zoom is the stream-shaped centre of the array`() {
        org.junit.jupiter.api.Assertions.assertEquals(SensorBox(0, 375, 4000, 2250), crop(1f, 0.5f, 0.5f))
    }

    @org.junit.jupiter.api.Test
    fun `a centred zoom halves each side around the middle`() {
        org.junit.jupiter.api.Assertions.assertEquals(SensorBox(1000, 937, 2000, 1125), crop(2f, 0.5f, 0.5f))
    }

    @org.junit.jupiter.api.Test
    fun `an off-centre crop moves and stays inside the visible frame`() {
        org.junit.jupiter.api.Assertions.assertEquals(SensorBox(2000, 375, 2000, 1125), crop(2f, 1f, 0f))
        org.junit.jupiter.api.Assertions.assertEquals(SensorBox(0, 1500, 2000, 1125), crop(2f, -3f, 5f))
    }
}
