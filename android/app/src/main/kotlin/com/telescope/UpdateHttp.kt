package com.telescope

import java.io.File
import java.io.IOException
import java.io.InputStream
import java.net.HttpURLConnection
import java.net.URL

// Updater's network half, plain JVM so tests can point it at a network with no internet behind it
// (a wifi that only reaches the computer is a normal way to use Telescope).
internal class UpdateHttp(private val userAgent: String, private val timeoutMs: Int = 15_000) {

    // Null when the channel has no release yet (404). Throws IOException when there's no reachable,
    // readable manifest, so the check reads as failed and runs again, not as up to date.
    fun fetchManifest(url: String): UpdateManifest? {
        val conn = open(url)
        try {
            if (conn.responseCode == 404) return null
            if (conn.responseCode != 200) throw IOException("HTTP ${conn.responseCode}")
            val bytes = conn.inputStream.use { it.readUpTo(MAX_MANIFEST_BYTES + 1) }
            if (bytes.size > MAX_MANIFEST_BYTES) throw IOException("manifest too big")
            return UpdateLogic.parse(String(bytes, Charsets.UTF_8)) ?: throw IOException("not a manifest")
        } finally {
            conn.disconnect()
        }
    }

    // Null once dest holds the verified file, or the message to show. Leaves nothing behind on failure.
    fun download(asset: ManifestAsset, dest: File, progress: (Int) -> Unit): String? {
        val partial = File(dest.path + ".part")
        return try {
            val conn = open(asset.url)
            try {
                if (conn.responseCode < 0) return OFFLINE  // an answer that isn't HTTP
                if (conn.responseCode != 200) return "The download failed (HTTP ${conn.responseCode})."
                var done = 0L
                var lastPct = -1
                conn.inputStream.use { input ->
                    partial.outputStream().use { out ->
                        val buf = ByteArray(64 * 1024)
                        while (true) {
                            val n = input.read(buf)
                            if (n < 0) break
                            done += n
                            if (done > asset.size) return "The download is bigger than it should be."
                            out.write(buf, 0, n)
                            val pct = (done * 100 / asset.size).toInt()
                            if (pct != lastPct) { lastPct = pct; progress(pct) }
                        }
                    }
                }
                if (done != asset.size) return "The download was cut short. Check the internet connection and try again."
            } finally {
                conn.disconnect()
            }
            val hash = partial.inputStream().use { UpdateLogic.sha256Hex(it) }
            if (!hash.equals(asset.sha256, ignoreCase = true)) {
                return "The download didn't match its checksum, so it wasn't installed."
            }
            if (!partial.renameTo(dest)) return "Couldn't save the download."
            null
        } catch (e: Exception) {  // not only IOException: this runs on a bare thread, where anything else would crash the app
            OFFLINE
        } finally {
            partial.delete()
        }
    }

    private fun open(url: String): HttpURLConnection =
        (URL(url).openConnection() as HttpURLConnection).apply {
            connectTimeout = timeoutMs
            readTimeout = timeoutMs
            instanceFollowRedirects = true  // GitHub release downloads redirect to its file host
            setRequestProperty("User-Agent", userAgent)
        }

    private fun InputStream.readUpTo(limit: Int): ByteArray {
        val out = java.io.ByteArrayOutputStream()
        val buf = ByteArray(8 * 1024)
        while (out.size() < limit) {
            val n = read(buf, 0, minOf(buf.size, limit - out.size()))
            if (n < 0) break
            out.write(buf, 0, n)
        }
        return out.toByteArray()
    }

    companion object {
        const val MAX_MANIFEST_BYTES = 256 * 1024
        private const val OFFLINE = "The download failed. Check the internet connection and try again."
    }
}
