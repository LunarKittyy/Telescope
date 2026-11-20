package com.telescope

import android.content.Context
import java.io.File

// Stores only the latest Camera2 probe report in the app cache directory and never touches stream preferences.
object CameraProbeCache {
    const val FILE_NAME = "camera2_probe.txt"

    fun cacheFile(context: Context): File = File(context.cacheDir, FILE_NAME)

    // Writes through a sibling temporary file first so a partial crash never leaves a half-written cache file.
    fun write(file: File, report: String): Boolean {
        return try {
            val parent = file.parentFile
            if (parent != null && !parent.exists()) parent.mkdirs()
            val tmp = File(parent, "$FILE_NAME.tmp")
            tmp.writeText(report, Charsets.UTF_8)
            if (tmp.renameTo(file)) {
                true
            } else {
                // Some filesystems refuse rename over an existing file; write directly and remove the temp copy.
                file.writeText(report, Charsets.UTF_8)
                tmp.delete()
                true
            }
        } catch (_: Exception) {
            false
        }
    }

    fun read(file: File): String? {
        return try {
            if (!file.isFile) null else file.readText(Charsets.UTF_8)
        } catch (_: Exception) {
            null
        }
    }
}
