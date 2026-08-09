package com.telescope

import android.Manifest
import android.content.Context
import android.content.Intent
import androidx.core.content.ContextCompat

// Single start point for CameraStreamService: local (MainActivity spinners) and remote (SessionServer/StreamPrefs) stay in sync.
object StreamLauncher {

    sealed interface Result {
        object Started : Result
        object AlreadyStreaming : Result
        // Reason is machine-readable token, not display text; desktop maps to UI message.
        data class Rejected(val reason: String) : Result
    }

    // Remote path must only call while MainActivity visible or service running (needed for startForegroundService on Android 12+).
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
            android.util.Log.w("StreamLauncher", "Could not start stream service", e)
            Result.Rejected("start_refused")
        }
    }

    // Convenience: remote start using last local selection.
    fun startFromPrefs(context: Context): Result =
        start(context, StreamPrefs.lastSelection(context), remote = true)
}
