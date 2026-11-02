package com.telescope

import com.telescope.NotesPlacement.Box
import com.telescope.NotesPlacement.Edge
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

class NotesPlacementTest {
    private val margin = 40
    private val line = 150

    @Test
    fun `a punch hole at the top puts the reading line right under it`() {
        val hole = Box(500, 30, 580, 110)
        val lens = NotesPlacement.lens(1080, 2400, listOf(hole), 0)
        val p = NotesPlacement.place(1080, 2400, lens, margin, line)
        assertEquals(Edge.TOP, p.lensEdge)
        assertEquals(110 + margin, p.readingTop)
        assertEquals(p.readingTop, p.column.top)
        assertEquals(Box(margin, 150, 1080 - margin, 2400 - margin), p.column)
    }

    @Test
    fun `a hole in the corner pulls the column towards it as far as the screen allows`() {
        val lens = NotesPlacement.lens(1440, 3000, listOf(Box(1200, 20, 1300, 120)), 0)
        val narrow = NotesPlacement.place(1440, 3000, lens, margin, line)
        assertEquals(1440 - margin, narrow.column.right)
    }

    @Test
    fun `without a cutout the lens is taken to be in the bezel above the top of the screen`() {
        val p = NotesPlacement.place(1080, 1920, NotesPlacement.lens(1080, 1920, emptyList(), 0), margin, line)
        assertEquals(Edge.TOP, p.lensEdge)
        assertEquals(margin, p.readingTop)
    }

    @Test
    fun `turned sideways the column sits beside the lens with the reading line level with it`() {
        // Turned to the left (rotation 1): the top of the phone, and the hole, are on the left
        val hole = Box(30, 500, 110, 580)
        val p = NotesPlacement.place(2400, 1080, NotesPlacement.lens(2400, 1080, listOf(hole), 1), margin, line)
        assertEquals(Edge.LEFT, p.lensEdge)
        assertEquals(110 + margin, p.column.left)
        assertEquals(540 - line / 2, p.readingTop)
        assertTrue(p.column.right - p.column.left <= 1080 - 2 * margin)  // no wider than in portrait

        val right = NotesPlacement.place(2400, 1080, NotesPlacement.lens(2400, 1080, emptyList(), 3), margin, line)
        assertEquals(Edge.RIGHT, right.lensEdge)
        assertEquals(2400 - margin, right.column.right)
    }

    @Test
    fun `a corner hole near the bottom of a sideways screen keeps the reading line halfway up`() {
        val hole = Box(30, 950, 110, 1030)
        val p = NotesPlacement.place(2400, 1080, NotesPlacement.lens(2400, 1080, listOf(hole), 1), margin, line)
        assertEquals(Edge.LEFT, p.lensEdge)
        assertEquals(540, p.readingTop)
        assertTrue(p.column.bottom - p.readingTop >= 3 * line)  // a few lines to read ahead
    }

    @Test
    fun `upside down the reading line sits a few lines above the lens`() {
        val p = NotesPlacement.place(1080, 2400, NotesPlacement.lens(1080, 2400, emptyList(), 2), margin, line)
        assertEquals(Edge.BOTTOM, p.lensEdge)
        assertEquals(2400 - margin, p.column.bottom)
        assertEquals(2400 - margin - 3 * line, p.readingTop)
    }

    @Test
    fun `the cutout nearest an edge is the camera`() {
        val lens = NotesPlacement.lens(1080, 2400, listOf(Box(400, 900, 600, 1000), Box(500, 0, 580, 90)), 0)
        assertEquals(Box(500, 0, 580, 90), lens)
    }
}
