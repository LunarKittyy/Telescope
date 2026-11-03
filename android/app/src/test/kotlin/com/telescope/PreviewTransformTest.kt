package com.telescope

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Test

class PreviewTransformTest {
    // What the preview did before the display's turn counted: the sensor's axes only
    private fun before(vw: Float, vh: Float, bw: Float, bh: Float, sensor: Int, cover: Boolean): Pair<Float, Float> {
        val swapped = sensor == 90 || sensor == 270
        val sx = if (swapped) vw / bh else vw / bw
        val sy = if (swapped) vh / bw else vh / bh
        val f = if (cover) maxOf(sx, sy) else minOf(sx, sy)
        return f / sx to f / sy
    }

    @Test
    fun `a portrait phone gets the same numbers as before`() {
        for (cover in listOf(true, false)) for (sensor in listOf(0, 90, 270)) {
            val r = PreviewTransform.compute(1080f, 1920f, 1920f, 1080f, sensor, 0, cover)
            val (x, y) = before(1080f, 1920f, 1920f, 1080f, sensor, cover)
            assertEquals(x, r.scaleX, 1e-6f)
            assertEquals(y, r.scaleY, 1e-6f)
            assertEquals(0f, r.rotateDegrees, 0f)
        }
    }

    @Test
    fun `a sideways display turns the picture back and fits it`() {
        // Upright the picture is 1080x1440, shown 1440x1080 in a 2400x1600 view: the height is the limit
        val r = PreviewTransform.compute(2400f, 1600f, 1440f, 1080f, 90, 90, false)
        assertEquals(-90f, r.rotateDegrees, 0f)
        assertEquals(1600f / 2400f, r.scaleX, 1e-5f)
        assertEquals(2133.3333f / 1600f, r.scaleY, 1e-5f)
        assertEquals(90f, PreviewTransform.compute(2400f, 1600f, 1440f, 1080f, 90, 270, false).rotateDegrees, 0f)
    }

    @Test
    fun `cover fills the view where fit leaves bars`() {
        val fit = PreviewTransform.compute(2400f, 1600f, 1440f, 1080f, 90, 90, false)
        val cover = PreviewTransform.compute(2400f, 1600f, 1440f, 1080f, 90, 90, true)
        assertEquals(true, cover.scaleX > fit.scaleX && cover.scaleY > fit.scaleY)
        assertEquals(2400f / 1440f * 1080f / 2400f, cover.scaleX, 1e-5f)
    }

    @Test
    fun `an upside-down display turns half way without changing the fit`() {
        val up = PreviewTransform.compute(1080f, 1920f, 1920f, 1080f, 90, 0, false)
        val down = PreviewTransform.compute(1080f, 1920f, 1920f, 1080f, 90, 180, false)
        assertEquals(180f, down.rotateDegrees, 0f)
        assertEquals(up.scaleX, down.scaleX, 0f)
        assertEquals(up.scaleY, down.scaleY, 0f)
    }
}
