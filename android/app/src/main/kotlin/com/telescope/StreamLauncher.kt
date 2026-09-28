package com.telescope

import android.Manifest
import android.content.Context
import android.content.Intent
import androidx.core.content.ContextCompat

// Single start point for CameraStreamService, opening with the selection the computer last picked.
object StreamLauncher {

    sealed interface Result {
        object Started : Result
        object AlreadyStreaming : Result
        // Reason is machine-readable token, not display text; desktop maps to UI message.
        data class Rejected(val reason: String) : Result
    }

    // Remote path: reachable only while MainActivity is visible, a stream runs, or WaitingService waits. While it waits,
    // the stream runs under its foreground service: Android 14+ refuses a new camera foreground service started from the
    // background. Otherwise Android may still refuse the start (Rejected) or the camera (the service then fails and stops
    // itself).
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

        val covered = WaitingService.covering
        val intent = Intent(context, CameraStreamService::class.java).apply {
            putExtra(CameraStreamService.EXTRA_LOCAL_ONLY, StreamPrefs.localOnly(context))
            putExtra(CameraStreamService.EXTRA_REMOTE, remote)
            putExtra(CameraStreamService.EXTRA_COVERED, covered)
            if (selection != null) {
                putExtra(CameraStreamService.EXTRA_CAMERA_ID, selection.cameraId)
                putExtra(CameraStreamService.EXTRA_LOGICAL_ID, selection.logicalId)
                putExtra(CameraStreamService.EXTRA_WIDTH, selection.width)
                putExtra(CameraStreamService.EXTRA_HEIGHT, selection.height)
                putExtra(CameraStreamService.EXTRA_OIS, selection.ois)
            }
        }
        return try {
            // A plain start is allowed: the waiting service already keeps the app in the foreground.
            if (covered) context.startService(intent) else ContextCompat.startForegroundService(context, intent)
            SessionStartWindow.begin()
            Result.Started
        } catch (e: Exception) {
            android.util.Log.w("StreamLauncher", "Could not start stream service", e)
            Result.Rejected("start_refused")
        }
    }

    // Remote start using the remembered selection.
    fun startFromPrefs(context: Context): Result =
        start(context, StreamPrefs.lastSelection(context), remote = true)
}
