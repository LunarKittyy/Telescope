package com.telescope

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNotNull
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.io.File

class StillJobsTest {
    @TempDir
    lateinit var dir: File

    private var now = 1_000_000L
    private fun jobs() = StillJobs(dir) { now }

    private fun writeFile(jobs: StillJobs, id: String, ext: String, bytes: Int): File {
        val part = jobs.newFile(id, ext)!!
        part.writeBytes(ByteArray(bytes))
        jobs.fileDone(id, part)
        return File(part.path.removeSuffix(".part"))
    }

    @Test
    fun `one request shoots at a time`() {
        val jobs = jobs()
        val id = jobs.start(1, 0)!!
        assertNull(jobs.start(1, 0))
        assertEquals(id, jobs.active())
        jobs.finish(id)
        assertNotNull(jobs.start(1, 0))
    }

    @Test
    fun `the status counts down, then lists the files as they're written`() {
        val jobs = jobs()
        val id = jobs.start(2, 5_000)!!
        now += 2_000
        assertEquals("waiting", jobs.status(id)!!.state)
        assertEquals(3_000L, jobs.status(id)!!.countdown_ms)

        jobs.capturing(id, paused = true)
        jobs.shotTaken(id)
        writeFile(jobs, id, "dng", 10)
        val status = jobs.status(id)!!
        assertEquals("capturing", status.state)
        assertTrue(status.paused)
        assertEquals(1, status.taken)
        assertEquals(listOf("image/x-adobe-dng"), status.files.map { it.type })
        assertEquals(10L, status.files.single().size)

        writeFile(jobs, id, "jpg", 20)
        jobs.finish(id)
        val done = jobs.status(id)!!
        assertEquals("done", done.state)
        assertFalse(done.paused)
        assertEquals(2, done.files.size)
    }

    @Test
    fun `files are named after when the request started, without the id`() {
        val jobs = jobs()
        val id = jobs.start(2, 0)!!
        writeFile(jobs, id, "jpg", 1)
        writeFile(jobs, id, "dng", 1)
        val names = jobs.status(id)!!.files.map { it.name }
        assertEquals(2, names.size)
        assertTrue(names[0].matches(Regex("IMG_\\d{8}_\\d{6}_01\\.jpg")), names[0])
        assertTrue(names[1].endsWith("_02.dng"), names[1])
        assertEquals(names[0], jobs.publicName(jobs.file(id, 0)!!))
        assertNull(jobs.file(id, 2))
    }

    @Test
    fun `a file still being written isn't offered`() {
        val jobs = jobs()
        val id = jobs.start(1, 0)!!
        val part = jobs.newFile(id, "jpg")!!
        part.writeBytes(ByteArray(5))
        assertTrue(jobs.status(id)!!.files.isEmpty())
        assertNull(jobs.file(id, 0))
    }

    @Test
    fun `a failed request says why and drops files that arrive after`() {
        val jobs = jobs()
        val id = jobs.start(2, 0)!!
        val part = jobs.newFile(id, "jpg")!!
        part.writeBytes(ByteArray(5))
        jobs.fail(id, "interrupted", "The camera was busy with something else")
        jobs.fileDone(id, part)
        val status = jobs.status(id)!!
        assertEquals("failed", status.state)
        assertEquals("interrupted", status.code)
        assertTrue(status.files.isEmpty())
        assertFalse(part.exists())
        // and the next one can start
        assertNotNull(jobs.start(1, 0))
    }

    @Test
    fun `forgetting a request deletes its files, but only once it's over`() {
        val jobs = jobs()
        val id = jobs.start(1, 0)!!
        val file = writeFile(jobs, id, "jpg", 3)
        jobs.forget(id)
        assertTrue(file.exists())  // still shooting
        jobs.finish(id)
        jobs.forget(id)
        assertFalse(file.exists())
        assertNull(jobs.status(id))
    }

    @Test
    fun `old and surplus requests are cleared when a new one starts`() {
        val jobs = jobs()
        val ids = (1..StillJobs.MAX_KEPT + 1).map { jobs.start(1, 0)!!.also { id -> writeFile(jobs, id, "jpg", 1); jobs.finish(id) } }
        // Each start keeps the newest MAX_KEPT - 1 that ended, plus itself
        assertNull(jobs.status(ids.first()))
        assertNotNull(jobs.status(ids.last()))
        now += StillJobs.KEEP_MS + 1
        jobs.start(1, 0)
        assertTrue(ids.all { jobs.status(it) == null })
    }

    @Test
    fun `clear empties the folder`() {
        val jobs = jobs()
        val id = jobs.start(1, 0)!!
        writeFile(jobs, id, "jpg", 1)
        jobs.clear()
        assertNull(jobs.status(id))
        assertTrue(dir.listFiles()!!.isEmpty())
    }

    @Test
    fun `old requests go when anyone asks about photos, not only on the next start`() {
        val jobs = jobs()
        val id = jobs.start(1, 0)!!
        val file = writeFile(jobs, id, "jpg", 1)
        jobs.finish(id)
        val other = jobs.start(1, 0)!!
        jobs.finish(other)
        now += StillJobs.KEEP_MS + 1
        assertNull(jobs.status(other))
        assertNull(jobs.status(id))
        assertFalse(file.exists())
    }

    @Test
    fun `fetched requests past the size limit go, oldest first`() {
        val jobs = jobs()
        val big = StillJobs.MAX_BYTES / 2 + 1
        val first = bigRequest(jobs, big)
        assertNotNull(jobs.file(first, 0))
        val second = bigRequest(jobs, big)
        assertNotNull(jobs.file(second, 0))
        val third = bigRequest(jobs, 1)
        assertNull(jobs.status(first))
        assertNotNull(jobs.status(second))
        assertNotNull(jobs.status(third))
    }

    private fun bigRequest(jobs: StillJobs, bytes: Long): String {
        val id = jobs.start(1, 0)!!
        val part = jobs.newFile(id, "dng")!!
        java.io.RandomAccessFile(part, "rw").use { it.setLength(bytes) }
        jobs.fileDone(id, part)
        jobs.finish(id)
        return id
    }

    @Test
    fun `a finished request over the size limit stays until it's fetched and another one finishes`() {
        val jobs = jobs()
        val huge = bigRequest(jobs, StillJobs.MAX_BYTES + 1)
        assertNotNull(jobs.status(huge))
        assertNotNull(jobs.file(huge, 0))
        // Fetched, but still the newest
        assertNotNull(jobs.status(huge))
        val next = bigRequest(jobs, 1)
        assertNull(jobs.status(huge))
        assertNotNull(jobs.status(next))
    }

    @Test
    fun `requests not fetched yet don't count towards the size limit`() {
        val jobs = jobs()
        val first = bigRequest(jobs, StillJobs.MAX_BYTES + 1)
        val second = bigRequest(jobs, 1)
        assertNotNull(jobs.status(first))
        assertNotNull(jobs.status(second))
    }

    @Test
    fun `a photo nobody ends fails with timeout once its time limit after the timer is up`() {
        val jobs = jobs()
        val id = jobs.start(1, 3_000, 20_000)!!
        jobs.capturing(id, paused = true)
        now += 22_999
        assertNull(jobs.expireOverdue())
        assertEquals("capturing", jobs.status(id)!!.state)
        now += 1
        val status = jobs.status(id)!!
        assertEquals("failed", status.state)
        assertEquals("timeout", status.code)
        assertFalse(status.paused)
        // Another photo can start straight away
        assertNull(jobs.active())
        assertNotNull(jobs.start(1, 0, 20_000))
    }

    @Test
    fun `expiring says which photo ran out, once`() {
        val jobs = jobs()
        val id = jobs.start(1, 0, 1_000)!!
        now += 1_000
        assertEquals(id, jobs.expireOverdue())
        assertNull(jobs.expireOverdue())
    }

    @Test
    fun `a photo that ended in time isn't touched by its limit, and no limit means none`() {
        val jobs = jobs()
        val done = jobs.start(1, 0, 1_000)!!
        jobs.finish(done)
        now += 5_000
        assertEquals("done", jobs.status(done)!!.state)
        val open = jobs.start(1, 0)!!
        now += 60 * 60_000L
        assertNull(jobs.expireOverdue())
        assertEquals(open, jobs.active())
    }
}
