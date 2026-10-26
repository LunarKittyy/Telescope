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
    fun `finished requests past the size limit go, oldest first`() {
        val jobs = jobs()
        val big = (StillJobs.MAX_BYTES / 2 + 1).toInt()
        val first = jobs.start(1, 0)!!.also { id -> java.io.RandomAccessFile(jobs.newFile(id, "dng")!!, "rw").use { it.setLength(big.toLong()) } }
        // newFile's part file is renamed by fileDone
        jobs.fileDone(first, dir.listFiles()!!.single { it.name.endsWith(".part") })
        jobs.finish(first)
        val second = jobs.start(1, 0)!!
        java.io.RandomAccessFile(jobs.newFile(second, "dng")!!, "rw").use { it.setLength(big.toLong()) }
        jobs.fileDone(second, dir.listFiles()!!.single { it.name.endsWith(".part") })
        jobs.finish(second)
        assertNull(jobs.status(first))
        assertNotNull(jobs.status(second))
    }
}
