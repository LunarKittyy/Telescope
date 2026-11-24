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
    // The check on a downloaded file; a check that throws is a problem to show, not a dead thread
    fun checked(verify: () -> String?, onError: () -> String): String? =
        try { verify() } catch (e: Exception) { onError() }

    const val REPO = "LunarKittyy/Telescope"
    const val STABLE = "stable"
    const val NIGHTLY = "nightly"

    private val json = Json { ignoreUnknownKeys = true }

    fun manifestUrl(channel: String): String =
        if (channel == NIGHTLY) "https://github.com/$REPO/releases/download/nightly/manifest.json"
        else "https://github.com/$REPO/releases/latest/download/manifest.json"

    // A check that began on one channel says nothing once the other has been picked.
    fun resultWanted(checkedChannel: String, currentChannel: String): Boolean = checkedChannel == currentChannel

    // A check's answer only goes out while its own check still holds the Checking state: never over a download or a later check.
    fun checkOwnsState(checking: Boolean, checkId: Long, currentCheckId: Long): Boolean = checking && checkId == currentCheckId

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

    // Once a day at most; a failed check doesn't count, so it retries on the next launch. A last check in the future
    // means the clock was set back since, and would otherwise hold the checks off until it caught up.
    fun checkDue(lastCheckMs: Long, nowMs: Long): Boolean = nowMs < lastCheckMs || nowMs - lastCheckMs >= DAY_MS

    const val DAY_MS = 24L * 60 * 60 * 1000

    // A second download over one in flight would write the same file twice; one waiting for the installer holds its state too
    fun downloadMayStart(downloading: Boolean, installing: Boolean): Boolean = !downloading && !installing

    // An install session that fails halfway is dropped, not left in the phone's installer until Android clears it
    fun <T> abandonOnFailure(abandon: () -> Unit, body: () -> T): T =
        try {
            body()
        } catch (e: Exception) {
            runCatching { abandon() }
            throw e
        }

    // The installer reports nothing if its confirm screen is left with Home; after this long the update is offered again
    fun installStuck(sinceMs: Long, nowMs: Long): Boolean = nowMs - sinceMs >= INSTALL_STUCK_MS

    const val INSTALL_STUCK_MS = 60_000L

    /** No session is expected: an installer result that names one is from an attempt already given up on. */
    const val NO_SESSION = -2

    // A result naming a session other than the live attempt's is from an earlier one; one naming none can't be told apart
    fun fromActiveSession(reported: Int, active: Int): Boolean = reported < 0 || reported == active
}
