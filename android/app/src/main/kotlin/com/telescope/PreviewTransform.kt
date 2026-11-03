package com.telescope

// The scale and turn the preview's TextureView needs, so the picture stands upright and undistorted
object PreviewTransform {
    /** [scaleX] and [scaleY] stretch the view's own rectangle, then it turns by [rotateDegrees], all about its centre. */
    data class Result(val scaleX: Float, val scaleY: Float, val rotateDegrees: Float)

    // Camera2 hands over the picture upright for the phone's natural orientation, so a turned display needs a turn back
    fun rotationFor(displayDegrees: Int): Float = when (((displayDegrees % 360) + 360) % 360) {
        90 -> -90f
        180 -> 180f
        270 -> 90f
        else -> 0f
    }

    /** [cover] fills the view and crops (the standalone crop box); otherwise the whole picture shows (a running stream). */
    fun compute(viewW: Float, viewH: Float, bufW: Float, bufH: Float,
                sensorOrientation: Int, displayDegrees: Int, cover: Boolean): Result {
        val swapped = sensorOrientation == 90 || sensorOrientation == 270
        val picW = if (swapped) bufH else bufW
        val picH = if (swapped) bufW else bufH
        val turned = rotationFor(displayDegrees).let { it == 90f || it == -90f }
        val shownW = if (turned) picH else picW
        val shownH = if (turned) picW else picH
        val fitW = viewW / shownW
        val fitH = viewH / shownH
        val scale = if (cover) maxOf(fitW, fitH) else minOf(fitW, fitH)
        return Result(picW * scale / viewW, picH * scale / viewH, rotationFor(displayDegrees))
    }
}
