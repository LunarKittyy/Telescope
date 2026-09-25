package com.telescope

import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.pm.PackageInstaller
import android.content.pm.PackageManager
import android.content.pm.Signature
import android.os.Build
import android.os.Handler
import android.os.Looper
import java.io.File
import java.io.IOException
import java.net.HttpURLConnection
import java.net.URL
import java.util.concurrent.CopyOnWriteArraySet

// The phone app's own updates: check the channel's manifest.json, download the APK, verify its checksum
// and signer, and hand it to Android's installer (which asks the user to confirm). One shared state, so
// the update card shows the same thing after the activity is recreated.
object Updater {

    sealed class State {
        object Idle : State()
        object Checking : State()
        object UpToDate : State()
        object CheckFailed : State()  // offline, or GitHub didn't answer
        data class Available(val manifest: UpdateManifest) : State()
        data class Downloading(val manifest: UpdateManifest, val percent: Int) : State()
        data class Installing(val manifest: UpdateManifest) : State()
        data class Failed(val message: String, val manifest: UpdateManifest?) : State()
    }

    private const val PREFS = "updates"
    private const val KEY_CHANNEL = "channel"
    private const val KEY_LAST_CHECK = "last_check_ms"
    private const val TIMEOUT_MS = 15_000
    private const val MAX_MANIFEST_BYTES = 256 * 1024

    @Volatile var state: State = State.Idle
        private set
    @Volatile private var checkedThisRun = false
    private val listeners = CopyOnWriteArraySet<() -> Unit>()
    private val main = Handler(Looper.getMainLooper())

    fun addListener(l: () -> Unit) { listeners.add(l) }
    fun removeListener(l: () -> Unit) { listeners.remove(l) }

    private fun set(newState: State) {
        state = newState
        main.post { listeners.forEach { it() } }
    }

    fun channel(context: Context): String =
        prefs(context).getString(KEY_CHANNEL, null) ?: UpdateLogic.defaultChannel(BuildConfig.CHANNEL)

    fun setChannel(context: Context, channel: String) {
        prefs(context).edit().putString(KEY_CHANNEL, channel).apply()
        check(context)
    }

    // Local builds ("dev") are signed with a debug key, so no release could install over them anyway.
    fun canUpdate(): Boolean = BuildConfig.CHANNEL != "dev"

    // Once each time the app starts, then daily while it stays open.
    fun maybeCheck(context: Context) {
        if (!canUpdate()) return
        if (state !is State.Idle && state !is State.UpToDate && state !is State.CheckFailed) return
        val last = prefs(context).getLong(KEY_LAST_CHECK, 0)
        if (!checkedThisRun || UpdateLogic.checkDue(last, System.currentTimeMillis())) check(context)
    }

    fun check(context: Context) {
        if (state is State.Checking || state is State.Downloading || state is State.Installing) return
        val app = context.applicationContext
        val channel = channel(app)
        checkedThisRun = true
        set(State.Checking)
        Thread {
            val manifest = try {
                fetchManifest(channel)
            } catch (e: Exception) {
                set(State.CheckFailed)  // offline is normal; the next launch or Check tries again
                return@Thread
            }
            prefs(app).edit().putLong(KEY_LAST_CHECK, System.currentTimeMillis()).apply()
            set(
                if (manifest != null && UpdateLogic.isNewer(manifest, BuildConfig.VERSION_CODE)) {
                    State.Available(manifest)
                } else {
                    State.UpToDate
                },
            )
        }.start()
    }

    // Null when the channel has no release yet (404) or the manifest isn't one to trust.
    private fun fetchManifest(channel: String): UpdateManifest? {
        val conn = open(UpdateLogic.manifestUrl(channel))
        try {
            if (conn.responseCode == 404) return null
            if (conn.responseCode != 200) throw IOException("HTTP ${conn.responseCode}")
            val bytes = conn.inputStream.use { it.readNBytesCompat(MAX_MANIFEST_BYTES + 1) }
            if (bytes.size > MAX_MANIFEST_BYTES) return null
            return UpdateLogic.parse(String(bytes, Charsets.UTF_8))
        } finally {
            conn.disconnect()
        }
    }

    // Downloads, verifies and installs. The caller has already made sure installs are allowed.
    fun downloadAndInstall(context: Context, manifest: UpdateManifest) {
        val app = context.applicationContext
        val asset = UpdateLogic.apkAsset(manifest) ?: return
        set(State.Downloading(manifest, 0))
        Thread {
            val apk = File(File(app.cacheDir, "updates").apply { mkdirs() }, "Telescope.apk")
            val problem = download(asset, apk) { pct -> set(State.Downloading(manifest, pct)) }
                ?: verifyApk(app, apk, manifest)
            if (problem != null) {
                apk.delete()
                set(State.Failed(problem, manifest))
                return@Thread
            }
            set(State.Installing(manifest))
            try {
                install(app, apk)
            } catch (e: Exception) {
                set(State.Failed("Android couldn't start the install.", manifest))
            }
        }.start()
    }

    private fun download(asset: ManifestAsset, dest: File, progress: (Int) -> Unit): String? {
        val partial = File(dest.path + ".part")
        return try {
            val conn = open(asset.url)
            try {
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
                if (done != asset.size) return "The download was cut short. Try again."
            } finally {
                conn.disconnect()
            }
            val hash = partial.inputStream().use { UpdateLogic.sha256Hex(it) }
            if (!hash.equals(asset.sha256, ignoreCase = true)) {
                return "The download didn't match its checksum, so it wasn't installed."
            }
            if (!partial.renameTo(dest)) return "Couldn't save the download."
            null
        } catch (e: IOException) {
            "The download failed. Check the internet connection and try again."
        } finally {
            partial.delete()
        }
    }

    // A clear message instead of the installer's vague "App not installed" when the key differs.
    private fun verifyApk(context: Context, apk: File, manifest: UpdateManifest): String? {
        val pm = context.packageManager
        val flags = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
            PackageManager.GET_SIGNING_CERTIFICATES
        } else {
            @Suppress("DEPRECATION") PackageManager.GET_SIGNATURES
        }
        val archive = pm.getPackageArchiveInfo(apk.path, flags) ?: return "The download isn't a valid app."
        if (archive.packageName != context.packageName) return "The download isn't Telescope."
        val installed = pm.getPackageInfo(context.packageName, flags)
        val theirs = signers(archive)
        val ours = signers(installed)
        if (theirs.isNotEmpty() && ours.isNotEmpty() && theirs != ours) {
            return "This copy of Telescope was signed differently from the update. " +
                "Uninstall it and install ${UpdateLogic.displayVersion(manifest)} from the releases page."
        }
        return null
    }

    private fun signers(info: android.content.pm.PackageInfo): Set<Signature> =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
            info.signingInfo?.apkContentsSigners?.toSet().orEmpty()
        } else {
            @Suppress("DEPRECATION") info.signatures?.toSet().orEmpty()
        }

    private fun install(context: Context, apk: File) {
        val installer = context.packageManager.packageInstaller
        val params = PackageInstaller.SessionParams(PackageInstaller.SessionParams.MODE_FULL_INSTALL)
        params.setAppPackageName(context.packageName)
        val sessionId = installer.createSession(params)
        installer.openSession(sessionId).use { session ->
            session.openWrite("Telescope.apk", 0, apk.length()).use { out ->
                apk.inputStream().use { it.copyTo(out) }
                session.fsync(out)
            }
            val flags = PendingIntent.FLAG_UPDATE_CURRENT or
                (if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) PendingIntent.FLAG_MUTABLE else 0)
            val intent = Intent(context, UpdateInstallReceiver::class.java)
            val pending = PendingIntent.getBroadcast(context, sessionId, intent, flags)
            session.commit(pending.intentSender)
        }
    }

    // From UpdateInstallReceiver: the installer finished without replacing this app.
    internal fun installFinished(ok: Boolean, message: String?) {
        val manifest = (state as? State.Installing)?.manifest
        if (ok) { set(State.UpToDate); return }
        set(State.Failed(message?.takeIf { it.isNotBlank() }?.let { "The update wasn't installed: $it" }
            ?: "The update wasn't installed.", manifest))
    }

    internal fun installCancelled() {
        val manifest = (state as? State.Installing)?.manifest ?: return
        set(State.Available(manifest))
    }

    private fun open(url: String): HttpURLConnection =
        (URL(url).openConnection() as HttpURLConnection).apply {
            connectTimeout = TIMEOUT_MS
            readTimeout = TIMEOUT_MS
            instanceFollowRedirects = true  // GitHub release downloads redirect to its file host
            setRequestProperty("User-Agent", "Telescope-Android/${BuildConfig.VERSION_NAME}")
        }

    private fun prefs(context: Context) = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    private fun java.io.InputStream.readNBytesCompat(limit: Int): ByteArray {
        val out = java.io.ByteArrayOutputStream()
        val buf = ByteArray(8 * 1024)
        while (out.size() < limit) {
            val n = read(buf, 0, minOf(buf.size, limit - out.size()))
            if (n < 0) break
            out.write(buf, 0, n)
        }
        return out.toByteArray()
    }
}

// Where Android reports on the install session. Needs to launch the confirm screen itself.
class UpdateInstallReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        when (intent.getIntExtra(PackageInstaller.EXTRA_STATUS, PackageInstaller.STATUS_FAILURE)) {
            PackageInstaller.STATUS_PENDING_USER_ACTION -> {
                val confirm = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
                    intent.getParcelableExtra(Intent.EXTRA_INTENT, Intent::class.java)
                } else {
                    @Suppress("DEPRECATION") intent.getParcelableExtra(Intent.EXTRA_INTENT)
                }
                if (confirm == null) {
                    Updater.installFinished(false, null)
                    return
                }
                confirm.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
                runCatching { context.startActivity(confirm) }
                    .onFailure { Updater.installFinished(false, null) }
            }
            PackageInstaller.STATUS_SUCCESS -> Updater.installFinished(true, null)
            PackageInstaller.STATUS_FAILURE_ABORTED -> Updater.installCancelled()
            else -> Updater.installFinished(false, intent.getStringExtra(PackageInstaller.EXTRA_STATUS_MESSAGE))
        }
    }
}
