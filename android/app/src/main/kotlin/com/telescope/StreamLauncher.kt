package com.telescope

import android.Manifest
import android.content.Context
import android.content.Intent
import androidx.core.content.ContextCompat

/**
 * The single place [CameraStreamService] is started from, so the local path
 * ([MainActivity.startStream], driven by the spinners) and the remote path
 * ([SessionServer], driven by [StreamPrefs]) can't drift apart in what extras
 * they pass.
 *
 * Nothing here is allowed to assume an Activity: the remote path runs on a
 * socket thread inside whichever component happens to be holding the session
 * endpoint open.
 */
object StreamLauncher {

    sealed interface Result {
        object Started : Result
        object AlreadyStreaming : Result
        /** [reason] is a stable machine-readable token, not display text - the
         *  desktop maps it to a message. */
        data class Rejected(val reason: String) : Result
    }

    /**
     * Starts a stream using [selection], or - when it is null, i.e. nothing has
     * ever been started on this phone - with no camera/size extras at all, so
     * [CameraStreamService.onStartCommand]'s own defaults apply.
     *
     * Callers on the remote path must only reach this while [MainActivity] is
     * visible or the service is already running (both are what keeps
     * [SessionServer] bound at all). That is also what makes the
     * `startForegroundService` below legal: Android 12+ blocks starting a
     * `camera`-type foreground service from the background, and the
     * service-already-running case returns [Result.AlreadyStreaming] before
     * getting here.
     */
    fun start(
        context: Context,
        selection: StreamPrefs.Selection?,
        remote: Boolean = false,
    ): Result {
        if (CameraStreamService.instance?.isStreaming == true) return Result.AlreadyStreaming
        if (ContextCompat.checkSelfPermission(context, Manifest.permission.CAMERA)
            != android.content.pm.PackageManager.PERMISSION_GRANTED
        ) {
            return Result.Rejected("no_camera_permission")
        }

        val intent = Intent(context, CameraStreamService::class.java).apply {
            putExtra(CameraStreamService.EXTRA_LOCAL_ONLY, StreamPrefs.localOnly(context))
            putExtra(CameraStreamService.EXTRA_REMOTE, remote)
            if (selection != null) {
                putExtra(CameraStreamService.EXTRA_CAMERA_ID, selection.cameraId)
                putExtra(CameraStreamService.EXTRA_LOGICAL_ID, selection.logicalId)
                putExtra(CameraStreamService.EXTRA_WIDTH, selection.width)
                putExtra(CameraStreamService.EXTRA_HEIGHT, selection.height)
                putExtra(CameraStreamService.EXTRA_OIS, selection.ois)
            }
        }
        return try {
            ContextCompat.startForegroundService(context, intent)
            Result.Started
        } catch (e: Exception) {
            // Likely ForegroundServiceStartNotAllowedException (foreground assumption failed).
            android.util.Log.w("StreamLauncher", "Could not start stream service", e)
            Result.Rejected("start_refused")
        }
    }

    /** Remote-start convenience: reproduce the last local selection. */
    fun startFromPrefs(context: Context): Result =
        start(context, StreamPrefs.lastSelection(context), remote = true)
}
