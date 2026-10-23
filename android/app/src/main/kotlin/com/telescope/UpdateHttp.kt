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
    fun download(asset: ManifestAsset, dest: File, strings: Strings, progress: (Int) -> Unit): String? {
        val partial = File(dest.path + ".part")
        return try {
            val conn = open(asset.url)
            try {
                if (conn.responseCode < 0) return strings.get(R.string.update_error_offline)  // an answer that isn't HTTP
                if (conn.responseCode != 200) return strings.get(R.string.update_error_http, conn.responseCode)
                var done = 0L
                var lastPct = -1
                conn.inputStream.use { input ->
                    partial.outputStream().use { out ->
                        val buf = ByteArray(64 * 1024)
                        while (true) {
                            val n = input.read(buf)
                            if (n < 0) break
                            done += n
                            if (done > asset.size) return strings.get(R.string.update_error_too_big)
                            out.write(buf, 0, n)
                            val pct = (done * 100 / asset.size).toInt()
                            if (pct != lastPct) { lastPct = pct; progress(pct) }
                        }
                    }
                }
                if (done != asset.size) return strings.get(R.string.update_error_cut_short)
            } finally {
                conn.disconnect()
            }
            val hash = partial.inputStream().use { UpdateLogic.sha256Hex(it) }
            if (!hash.equals(asset.sha256, ignoreCase = true)) {
                return strings.get(R.string.update_error_checksum)
            }
            if (!partial.renameTo(dest)) return strings.get(R.string.update_error_save)
            null
        } catch (e: Exception) {  // not only IOException: this runs on a bare thread, where anything else would crash the app
            strings.get(R.string.update_error_offline)
        } finally {
            partial.delete()
        }
    }

    // Rolling nightly assets can change after the update card was shown; retry once with fresh checksums.
    fun downloadUpdate(manifest: UpdateManifest, dest: File, strings: Strings,
                       fetchLatest: () -> UpdateManifest?, progress: (UpdateManifest, Int) -> Unit): Pair<UpdateManifest, String?> {
        val asset = UpdateLogic.apkAsset(manifest) ?: return manifest to strings.get(R.string.update_error_not_an_app)
        val problem = download(asset, dest, strings) { progress(manifest, it) } ?: return manifest to null
        val latest = runCatching { fetchLatest() }.getOrNull() ?: return manifest to problem
        val replacement = UpdateLogic.apkAsset(latest)
        if (latest.channel != manifest.channel || latest.build <= manifest.build ||
            latest.android.versionCode <= manifest.android.versionCode || replacement == null) return manifest to problem
        progress(latest, 0)
        return latest to download(replacement, dest, strings) { progress(latest, it) }
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
    }
}
