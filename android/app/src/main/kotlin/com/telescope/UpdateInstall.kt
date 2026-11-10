package com.telescope

import android.content.Intent
import android.net.Uri
import android.provider.Settings
import androidx.appcompat.app.AppCompatActivity

// A held update installs once the screen is back from "Allow from this source", but never over a stream that started meanwhile
fun shouldResumePendingUpdate(hasPending: Boolean, canInstall: Boolean, streaming: Boolean): Boolean =
    hasPending && canInstall && !streaming

fun AppCompatActivity.canInstallUpdates(): Boolean = packageManager.canRequestPackageInstalls()

// Installs the update, or first asks for "Allow from this source", which Android asks once per app. `waitForAllow` holds the update until the screen is back from that setting.
fun AppCompatActivity.installUpdate(manifest: UpdateManifest, waitForAllow: (UpdateManifest?) -> Unit) {
    if (canInstallUpdates()) {
        Updater.downloadAndInstall(this, manifest)
        return
    }
    waitForAllow(manifest)
    lookDialog()
        .setTitle(R.string.allow_installs_title)
        .setMessage(R.string.allow_installs_message)
        .setNegativeButton(R.string.cancel) { _, _ -> waitForAllow(null) }
        .setPositiveButton(R.string.open_settings) { _, _ ->
            startActivity(Intent(Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES, Uri.parse("package:$packageName")))
        }
        .showLook()
}
