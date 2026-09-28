package com.telescope

import android.content.Context
import java.io.File

// The diagnostics of the last few streams, kept in a small file so Copy diagnostics can still show why a
// stream stopped or dropped after the service (or the whole app) is gone.
object RecentRuns {
    const val KEEP = 3
    private const val FILE = "recent_streams.txt"
    private const val SEPARATOR = "\n--- previous stream ---\n"

    // Newest first, at most KEEP.
    fun add(runs: List<String>, report: String, keep: Int = KEEP): List<String> =
        (listOf(report.trimEnd()) + runs).take(keep)

    fun encode(runs: List<String>): String = runs.joinToString(SEPARATOR)

    fun decode(text: String): List<String> =
        text.split(SEPARATOR).map { it.trimEnd() }.filter { it.isNotBlank() }

    // Copy diagnostics: the live report, then the previous runs; a camera list the same as the one above it is one line.
    fun report(live: String, previous: List<String>): String {
        if (previous.isEmpty()) return live
        var above = cameraBlock(live)
        val runs = previous.map { run ->
            val block = cameraBlock(run)
            val shown = if (block != null && block == above) run.replace(block, "Cameras: same as above") else run
            above = block ?: above
            shown
        }
        return live + "\nPrevious streams, newest first:\n\n" + runs.joinToString("\n\n") + "\n"
    }

    private fun cameraBlock(report: String): String? {
        val lines = report.lines()
        val start = lines.indexOf("Cameras:")
        if (start < 0) return null
        val end = (start + 1 until lines.size).firstOrNull { !lines[it].startsWith("  ") } ?: lines.size
        return lines.subList(start, end).joinToString("\n")
    }

    // A crash on any thread lands in Copy diagnostics too; Android's own handler still runs after, so nothing is hidden.
    fun recordCrashes(context: Context) {
        val app = context.applicationContext
        val previous = Thread.getDefaultUncaughtExceptionHandler()
        if (previous is CrashRecorder) return
        Thread.setDefaultUncaughtExceptionHandler(CrashRecorder(app, previous))
    }

    private class CrashRecorder(
        private val context: Context,
        private val previous: Thread.UncaughtExceptionHandler?,
    ) : Thread.UncaughtExceptionHandler {
        override fun uncaughtException(t: Thread, e: Throwable) {
            runCatching { save(context, "Crashed on ${t.name}: ${e.javaClass.simpleName}: ${e.message}") }
            previous?.uncaughtException(t, e)
        }
    }

    @Synchronized
    fun load(context: Context): List<String> =
        runCatching { decode(File(context.filesDir, FILE).readText()) }.getOrDefault(emptyList())

    @Synchronized
    fun save(context: Context, report: String) {
        runCatching { File(context.filesDir, FILE).writeText(encode(add(load(context), report))) }
            .onFailure { android.util.Log.w("RecentRuns", "Could not save the stream report", it) }
    }
}
