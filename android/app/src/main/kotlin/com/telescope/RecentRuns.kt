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

    @Synchronized
    fun load(context: Context): List<String> =
        runCatching { decode(File(context.filesDir, FILE).readText()) }.getOrDefault(emptyList())

    @Synchronized
    fun save(context: Context, report: String) {
        runCatching { File(context.filesDir, FILE).writeText(encode(add(load(context), report))) }
            .onFailure { android.util.Log.w("RecentRuns", "Could not save the stream report", it) }
    }
}
