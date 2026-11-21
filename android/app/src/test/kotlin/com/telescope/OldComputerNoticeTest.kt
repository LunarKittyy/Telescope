package com.telescope

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Test

class OldComputerNoticeTest {
    @Test
    fun `nothing is shown until a computer knocks`() {
        assertNull(OldComputerNotice { 0L }.current())
    }

    @Test
    fun `a knock shows its name, or an empty one when it is not known`() {
        val notice = OldComputerNotice { 0L }
        notice.knocked("Desk")
        assertEquals("Desk", notice.current())
        notice.knocked(null)
        assertEquals("", notice.current())
    }

    @Test
    fun `it goes away when the computer stops knocking`() {
        var now = 0L
        val notice = OldComputerNotice { now }
        notice.knocked("Desk")
        now = OldComputerNotice.SHOW_MS
        assertEquals("Desk", notice.current())
        now += 1
        assertNull(notice.current())
    }
}
