package com.telescope

// Where the notes go on screen: a column right by the front camera, with the line being read as close to the lens as
// it fits. Plain numbers in screen pixels, so it can be tested without a device.
object NotesPlacement {
    data class Box(val left: Int, val top: Int, val right: Int, val bottom: Int) {
        val centreX: Int get() = (left + right) / 2
        val centreY: Int get() = (top + bottom) / 2
    }

    enum class Edge { TOP, LEFT, RIGHT, BOTTOM }

    // column: where the text is drawn (and cut off); readingTop: where the top of the line being read sits
    data class Placement(val column: Box, val readingTop: Int, val lensEdge: Edge)

    // rotation: Surface.ROTATION_* as 0..3. Without a cutout the lens is in the bezel above the screen's natural top.
    fun lens(width: Int, height: Int, cutouts: List<Box>, rotation: Int): Box {
        cutouts.minByOrNull { distanceToEdge(it, width, height) }?.let { return it }
        return when (rotation) {
            1 -> Box(0, height / 2, 0, height / 2)  // turned left: the top is now on the left
            2 -> Box(width / 2, height, width / 2, height)
            3 -> Box(width, height / 2, width, height / 2)
            else -> Box(width / 2, 0, width / 2, 0)
        }
    }

    fun edge(lens: Box, width: Int, height: Int): Edge {
        val gaps = listOf(Edge.TOP to lens.top, Edge.LEFT to lens.left, Edge.RIGHT to width - lens.right,
                          Edge.BOTTOM to height - lens.bottom)
        return gaps.minByOrNull { it.second }!!.first
    }

    fun place(width: Int, height: Int, lens: Box, margin: Int, lineHeight: Int): Placement {
        val edge = edge(lens, width, height)
        val narrow = minOf(width, height) - 2 * margin  // as wide as the screen is in portrait, wherever the lens is
        return when (edge) {
            Edge.TOP -> {
                val w = minOf(narrow, width - 2 * margin)
                val left = (lens.centreX - w / 2).coerceIn(margin, width - margin - w)
                val top = maxOf(lens.bottom, 0) + margin
                Placement(Box(left, top, left + w, height - margin), top, edge)
            }
            Edge.BOTTOM -> {
                val w = minOf(narrow, width - 2 * margin)
                val left = (lens.centreX - w / 2).coerceIn(margin, width - margin - w)
                val bottom = minOf(lens.top, height) - margin
                val reading = (bottom - 3 * lineHeight).coerceAtLeast(margin)
                Placement(Box(left, margin, left + w, bottom), reading, edge)
            }
            Edge.LEFT, Edge.RIGHT -> {
                val near = if (edge == Edge.LEFT) maxOf(lens.right, 0) + margin else minOf(lens.left, width) - margin
                val w = minOf(narrow, if (edge == Edge.LEFT) width - margin - near else near - margin)
                val left = if (edge == Edge.LEFT) near else near - w
                val reading = (lens.centreY - lineHeight / 2).coerceIn(margin, maxOf(margin, height - margin - lineHeight))
                Placement(Box(left, margin, left + w, height - margin), reading, edge)
            }
        }
    }

    private fun distanceToEdge(box: Box, width: Int, height: Int): Int =
        minOf(box.top, box.left, width - box.right, height - box.bottom)
}
