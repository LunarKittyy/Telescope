package com.telescope

import java.io.InputStream
import java.security.MessageDigest
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json

// The release's manifest.json (written by .github/write_manifest.py). Only what the phone needs.
@Serializable
data class ManifestAsset(val name: String, val url: String, val sha256: String, val size: Long)

@Serializable
data class ManifestAndroid(val versionCode: Int, val versionName: String, val asset: String)

@Serializable
data class UpdateManifest(
    val version: String,
    val build: Int,
    val channel: String,
    val versionName: String = "",
    val notes: String = "",
    val assets: List<ManifestAsset>,
    val android: ManifestAndroid,
)

// Pure update rules, JVM-tested; Updater does the Android side (network, files, PackageInstaller).
object UpdateLogic {
    const val REPO = "LunarKittyy/Telescope"
    const val STABLE = "stable"
    const val NIGHTLY = "nightly"

    private val json = Json { ignoreUnknownKeys = true }

    fun manifestUrl(channel: String): String =
        if (channel == NIGHTLY) "https://github.com/$REPO/releases/download/nightly/manifest.json"
        else "https://github.com/$REPO/releases/latest/download/manifest.json"

    // A nightly build follows nightly; stable and local builds follow stable.
    fun defaultChannel(buildChannel: String): String = if (buildChannel == NIGHTLY) NIGHTLY else STABLE

    // Null when the text isn't a manifest this app can trust: not https, or a checksum that isn't SHA-256.
    fun parse(text: String): UpdateManifest? {
        val manifest = runCatching { json.decodeFromString(UpdateManifest.serializer(), text) }.getOrNull()
            ?: return null
        val apk = apkAsset(manifest) ?: return null
        if (!apk.url.startsWith("https://") || apk.sha256.length != 64 || apk.size <= 0) return null
        return manifest
    }

    fun apkAsset(manifest: UpdateManifest): ManifestAsset? =
        manifest.assets.firstOrNull { it.name == manifest.android.asset }

    // Build numbers only grow on master, so this also stops a channel switch from downgrading.
    fun isNewer(manifest: UpdateManifest, currentVersionCode: Int): Boolean =
        manifest.android.versionCode > currentVersionCode

    fun displayVersion(manifest: UpdateManifest): String =
        if (manifest.channel == STABLE) manifest.version else "${manifest.version} ${manifest.channel} ${manifest.build}"

    fun sha256Hex(input: InputStream): String {
        val digest = MessageDigest.getInstance("SHA-256")
        val buf = ByteArray(64 * 1024)
        while (true) {
            val n = input.read(buf)
            if (n < 0) break
            digest.update(buf, 0, n)
        }
        return digest.digest().joinToString("") { "%02x".format(it) }
    }

    // Once a day at most; a failed check doesn't count, so it retries on the next launch.
    fun checkDue(lastCheckMs: Long, nowMs: Long): Boolean = nowMs - lastCheckMs >= DAY_MS

    const val DAY_MS = 24L * 60 * 60 * 1000
}
