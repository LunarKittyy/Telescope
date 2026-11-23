package com.telescope

import kotlin.math.max
import kotlin.math.min
import kotlin.math.round

// The size of the phone's own preview picture: the stream's, or a smaller one when the screen couldn't show more of it
object PreviewSize {
    // A smaller size may fall this far short of what the screen shows (a 1080p picture on a 1440 px wide screen)
    private const val LEAST_SHARE = 0.75
    private const val ASPECT_SLACK = 0.02

    /** Sizes are (width, height). [offered] are the camera's sizes for a preview surface, the screen is [screenW] by [screenH] pixels in either turn. */
    fun pick(stream: Pair<Int, Int>, offered: List<Pair<Int, Int>>, screenW: Int, screenH: Int): Pair<Int, Int> {
        if (screenW <= 0 || screenH <= 0) return stream
        val longSide = max(stream.first, stream.second)
        val shortSide = min(stream.first, stream.second)
        // The whole picture fits the screen, so the smaller of the two fits is what shows
        val fit = min(max(screenW, screenH).toDouble() / longSide, min(screenW, screenH).toDouble() / shortSide)
        if (fit >= 1.0) return stream
        val needLong = round(longSide * fit)
        val aspect = stream.first.toDouble() / stream.second
        val smaller = offered
            .filter { it.first.toLong() * it.second < stream.first.toLong() * stream.second }
            .filter { Math.abs(it.first.toDouble() / it.second - aspect) <= ASPECT_SLACK * aspect }
            .sortedBy { it.first.toLong() * it.second }
        // The smallest that shows all the screen can, else the biggest that comes close
        return smaller.firstOrNull { max(it.first, it.second) >= needLong }
            ?: smaller.lastOrNull { max(it.first, it.second) >= needLong * LEAST_SHARE }
            ?: stream
    }
}
