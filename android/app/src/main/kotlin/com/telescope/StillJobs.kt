package com.telescope

import java.io.File
import java.security.SecureRandom
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale

// Photo requests and their files in the app's cache until fetched and forgotten or too old; one shoots at a time
class StillJobs(private val dir: File, private val clock: () -> Long = System::currentTimeMillis) {
    companion object {
        const val KEEP_MS = 10 * 60_000L  // files a computer never came back for go after this
        const val MAX_KEPT = 4             // and only the newest few are kept at all
        const val MAX_BYTES = 1_000_000_000L  // and no more than this of finished requests' files a computer already fetched
        fun type(name: String): String = when (name.substringAfterLast('.').lowercase()) {
            "dng" -> "image/x-adobe-dng"
            "heic" -> "image/heic"
            else -> "image/jpeg"
        }
    }

    private class Job(val id: String, val shots: Int, val startedAt: Long, var shootAt: Long, val deadline: Long) {
        var state = "waiting"
        var taken = 0
        var paused = false
        var error: String? = null
        var code: String? = null
        val files = mutableListOf<File>()
        var endedAt = 0L
        var named = 0  // files handed out so far
        val fetched = HashSet<Int>()  // files a computer asked for
        fun allFetched() = files.indices.all { it in fetched }
    }

    private val jobs = LinkedHashMap<String, Job>()
    private val random = SecureRandom()

    // A new request, or null while another one is still shooting; limitMs after its timer it fails however the camera is doing (0 = never)
    @Synchronized
    fun start(shots: Int, timerMs: Long, limitMs: Long = 0L): String? {
        expireOverdue()
        if (jobs.values.any { it.endedAt == 0L }) return null
        sweep()
        val id = ByteArray(8).also(random::nextBytes).joinToString("") { "%02x".format(it) }
        val now = clock()
        jobs[id] = Job(id, shots, now, now + timerMs, if (limitMs > 0L) now + timerMs + limitMs else 0L)
        return id
    }

    // The request still shooting past its time limit fails with timeout (a camera thread stuck in the phone's camera
    // service can't end it); its id, or null when nothing was overdue
    @Synchronized
    fun expireOverdue(): String? {
        val job = jobs.values.firstOrNull { it.endedAt == 0L && it.deadline != 0L && clock() >= it.deadline } ?: return null
        fail(job.id, "timeout", "The phone's camera stopped answering")
        return job.id
    }

    @Synchronized
    fun capturing(id: String, paused: Boolean) {
        val job = jobs[id]?.takeIf { it.endedAt == 0L } ?: return
        job.state = "capturing"
        job.paused = paused
    }

    @Synchronized
    fun shotTaken(id: String) { jobs[id]?.let { if (it.endedAt == 0L) it.taken++ } }

    // Where the next file of this request goes: IMG_<when it started>_<n>.<ext>, n counting from 01
    @Synchronized
    fun newFile(id: String, ext: String): File? {
        val job = jobs[id]?.takeIf { it.endedAt == 0L } ?: return null
        dir.mkdirs()
        val stamp = SimpleDateFormat("yyyyMMdd_HHmmss", Locale.ROOT).format(Date(job.startedAt))
        return File(dir, "IMG_${stamp}_${"%02d".format(++job.named)}_$id.$ext.part")
    }

    // A file newFile handed out is written: it can be fetched from now on
    @Synchronized
    fun fileDone(id: String, part: File) {
        val job = jobs[id]
        val done = File(part.path.removeSuffix(".part"))
        if (job == null || job.state == "failed" || !part.renameTo(done)) { part.delete(); return }
        job.files += done
    }

    @Synchronized
    fun finish(id: String) {
        val job = jobs[id]?.takeIf { it.endedAt == 0L } ?: return
        job.state = "done"
        job.taken = job.shots  // a picture can be saved before its capture result says it was taken
        job.paused = false
        job.endedAt = clock()
    }

    @Synchronized
    fun fail(id: String, code: String, text: String) {
        val job = jobs[id]?.takeIf { it.endedAt == 0L } ?: return
        job.state = "failed"
        job.paused = false
        job.error = text
        job.code = code
        job.endedAt = clock()
    }

    // The request still shooting, if any
    @Synchronized
    fun active(): String? {
        expireOverdue()
        return jobs.values.firstOrNull { it.endedAt == 0L }?.id
    }

    @Synchronized
    fun status(id: String): StillStatus? {
        expireOverdue()
        sweep()
        val job = jobs[id] ?: return null
        return StillStatus(
            id = job.id, state = job.state, shots = job.shots, taken = job.taken,
            files = job.files.map { StillFile(publicName(it), it.length(), type(it.name)) },
            countdown_ms = if (job.state == "waiting") (job.shootAt - clock()).coerceAtLeast(0L) else 0L,
            paused = job.paused, error = job.error, code = job.code,
        )
    }

    // The n-th file (from 0) of a request
    @Synchronized
    fun file(id: String, n: Int): File? {
        sweep()
        val job = jobs[id] ?: return null
        return job.files.getOrNull(n)?.takeIf { it.isFile }?.also { job.fetched += n }
    }

    // The computer has what it wanted: the files go
    @Synchronized
    fun forget(id: String) {
        val job = jobs[id]?.takeIf { it.endedAt != 0L } ?: return
        job.files.forEach { it.delete() }
        jobs.remove(id)
    }

    // Everything goes: the stream stopped
    @Synchronized
    fun clear() {
        jobs.clear()
        dir.listFiles()?.forEach { it.delete() }
    }

    // The name a computer saves it under, without the request id kept to tell requests apart here
    fun publicName(file: File): String = file.name.replace(Regex("_[0-9a-f]{16}(\\.[a-z]+)$"), "$1")

    // Requests over KEEP_MS old, past the newest MAX_KEPT - 1, or past MAX_BYTES of fetched ones bar the newest (oldest first) go
    @Synchronized
    fun sweep() {
        val now = clock()
        val ended = jobs.values.filter { it.endedAt != 0L }
        val old = (ended.filter { now - it.endedAt > KEEP_MS } + ended.dropLast(MAX_KEPT - 1)).toMutableSet()
        var bytes = 0L
        for (job in ended.reversed().drop(1)) {
            if (!job.allFetched()) continue
            bytes += job.files.sumOf { it.length() }
            if (bytes > MAX_BYTES) old += job
        }
        old.forEach { job -> job.files.forEach { it.delete() }; jobs.remove(job.id) }
    }
}
