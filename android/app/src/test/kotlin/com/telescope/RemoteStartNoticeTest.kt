package com.telescope

import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

class RemoteStartNoticeTest {

    @Test
    fun `a remote start is announced once however often the app opens`() {
        val notice = RemoteStartNotice()
        notice.started(remotely = true)
        assertTrue(notice.take(streaming = true))
        assertFalse(notice.take(streaming = true))
        assertFalse(notice.take(streaming = true))
    }

    @Test
    fun `nothing is announced before the stream is up or for a start on the phone`() {
        val notice = RemoteStartNotice()
        notice.started(remotely = true)
        assertFalse(notice.take(streaming = false))
        assertTrue(notice.take(streaming = true))
        notice.started(remotely = false)
        assertFalse(notice.take(streaming = true))
    }

    @Test
    fun `the next remote start is announced again`() {
        val notice = RemoteStartNotice()
        notice.started(remotely = true)
        notice.take(streaming = true)
        notice.started(remotely = true)
        assertTrue(notice.take(streaming = true))
    }
}
