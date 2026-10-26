package com.telescope

import org.junit.jupiter.api.AfterEach
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.BeforeEach
import org.junit.jupiter.api.Test

class NotesTest {
    private fun parse(vararg pairs: Pair<String, String>) = NotesCommand.parse(mapOf(*pairs))

    @Test
    fun `camera controls aren't notes`() {
        assertNull(parse("action" to "zoom", "ratio" to "2"))
        assertNull(parse())
    }

    @Test
    fun `notes take their text as sent, with Windows line ends made plain`() {
        assertEquals(NotesCommand.Text("Hi\nthere\n"), parse("action" to "notes", "text" to "Hi\r\nthere\r"))
        assertEquals(NotesCommand.Text(""), parse("action" to "notes", "text" to ""))
        assertTrue(parse("action" to "notes") is NotesCommand.Refused)
        assertTrue(parse("action" to "notes", "text" to "x".repeat(NotesCommand.TEXT_MAX_CHARS + 1)) is NotesCommand.Refused)
    }

    @Test
    fun `style is fitted to what the screen can show and leaves out what wasn't sent`() {
        assertEquals(NotesCommand.Style(120, 0, true), parse("action" to "notes_style", "size" to "500", "speed" to "-3", "playing" to "1"))
        assertEquals(NotesCommand.Style(null, 7, null), parse("action" to "notes_style", "speed" to "7"))
        assertTrue(parse("action" to "notes_style", "size" to "big") is NotesCommand.Refused)
        assertTrue(parse("action" to "notes_style", "playing" to "yes") is NotesCommand.Refused)
    }

    @Test
    fun `scrolling goes by typed lines or back to the top`() {
        assertEquals(NotesCommand.Scroll(1, false), parse("action" to "notes_scroll", "by" to "1"))
        assertEquals(NotesCommand.Scroll(-2, false), parse("action" to "notes_scroll", "by" to "-2"))
        assertEquals(NotesCommand.Scroll(0, true), parse("action" to "notes_scroll", "to" to "top"))
        assertTrue(parse("action" to "notes_scroll") is NotesCommand.Refused)
        assertTrue(parse("action" to "notes_scroll", "by" to "lots") is NotesCommand.Refused)
    }

    @Test
    fun `a style change keeps the text and the rest of the style`() {
        val state = NotesState(text = "Hello", sizeSp = 40, speed = 3, playing = false)
        val after = state.after(NotesCommand.Style(null, null, true))
        assertEquals(NotesState(text = "Hello", sizeSp = 40, speed = 3, playing = true), after)
        assertTrue(after.scrolls)
        assertFalse(after.copy(speed = 0).scrolls)
        assertEquals("Bye", after.after(NotesCommand.Text("Bye")).text)
    }

    @Test
    fun `typed lines start where a line has something on it`() {
        assertEquals(listOf(0, 7, 14), NotesLines.starts("Hello\n\nthere\n\nfriend\n   \n"))
        assertEquals(emptyList<Int>(), NotesLines.starts("\n\n"))
    }

    @Test
    fun `next and previous move from one typed line to the next`() {
        val tops = listOf(0, 120, 400, 520)
        assertEquals(120, NotesLines.target(tops, 0, 1))
        assertEquals(400, NotesLines.target(tops, 130, 1))  // halfway through a line: next is the one after it
        assertEquals(520, NotesLines.target(tops, 0, 10))
        assertEquals(520, NotesLines.target(tops, 520, 1))  // the last one stays put
        assertEquals(120, NotesLines.target(tops, 130, -1))  // back to the start of the line being read
        assertEquals(0, NotesLines.target(tops, 120, -1))
        assertEquals(0, NotesLines.target(tops, 400, -5))
        assertEquals(77, NotesLines.target(emptyList(), 77, 1))
    }
}

class NotesBoardTest {
    private val heard = mutableListOf<String>()
    private val listener = object : NotesBoard.Listener {
        override fun onNotes(state: NotesState) { heard.add("notes:${state.text}") }
        override fun onScroll(by: Int, toTop: Boolean) { heard.add("scroll:$by:$toTop") }
    }

    @BeforeEach
    fun setUp() {
        NotesBoard.post = { it() }
        NotesBoard.clear()
        NotesBoard.addListener(listener)
        heard.clear()
    }

    @AfterEach
    fun tearDown() {
        NotesBoard.removeListener(listener)
        NotesBoard.clear()
        NotesBoard.setShown(false)
    }

    @Test
    fun `notes held back while Recovering or under Preview open once the stream is back`() {
        NotesBoard.apply(NotesCommand.Text("Line one"))
        assertFalse(NotesBoard.opensNow(streaming = false, previewUp = false, inFront = true))
        assertFalse(NotesBoard.opensNow(streaming = true, previewUp = true, inFront = true))
        assertFalse(NotesBoard.opensNow(streaming = true, previewUp = false, inFront = false))
        assertTrue(NotesBoard.opensNow(streaming = true, previewUp = false, inFront = true))
        NotesBoard.setShown(true)
        assertFalse(NotesBoard.opensNow(streaming = true, previewUp = false, inFront = true))  // already up
        NotesBoard.setShown(false)
        NotesBoard.dismissed = true
        assertFalse(NotesBoard.opensNow(streaming = true, previewUp = false, inFront = true))  // closed with Back
    }

    @Test
    fun `new text goes on screen and clears a closed screen`() {
        NotesBoard.dismissed = true
        assertTrue(NotesBoard.apply(NotesCommand.Text("Line one")))
        assertFalse(NotesBoard.dismissed)
        assertTrue(NotesBoard.wantsScreen)
        assertFalse(NotesBoard.apply(NotesCommand.Style(60, null, null)))  // only text opens the screen
        assertEquals(60, NotesBoard.state.sizeSp)
        assertEquals(listOf("notes:Line one", "notes:Line one"), heard)
    }

    @Test
    fun `empty text clears the notes`() {
        NotesBoard.apply(NotesCommand.Text("Line one"))
        assertFalse(NotesBoard.apply(NotesCommand.Text("  ")))
        assertFalse(NotesBoard.hasNotes)
        assertFalse(NotesBoard.wantsScreen)
    }

    @Test
    fun `scrolling reaches the screen without changing the notes`() {
        NotesBoard.apply(NotesCommand.Text("A\nB"))
        heard.clear()
        NotesBoard.apply(NotesCommand.Scroll(1, false))
        assertEquals(listOf("scroll:1:false"), heard)
        assertEquals("A\nB", NotesBoard.state.text)
    }

    @Test
    fun `the stream ending takes the notes but keeps the style`() {
        NotesBoard.apply(NotesCommand.Text("A"))
        NotesBoard.apply(NotesCommand.Style(80, 5, true))
        NotesBoard.clear()
        assertFalse(NotesBoard.hasNotes)
        assertEquals(80, NotesBoard.state.sizeSp)
        assertEquals("notes:", heard.last())
    }
}
