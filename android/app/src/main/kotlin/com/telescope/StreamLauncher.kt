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
        fps: Int? = null,
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
            if (fps != null) putExtra(CameraStreamService.EXTRA_FPS, fps)
        }
        // Opened before the start, or a service that settles first would have its settle() undone
        SessionStartWindow.begin()
        return try {
            // A plain start is allowed: the waiting service already keeps the app in the foreground.
            if (covered) context.startService(intent) else ContextCompat.startForegroundService(context, intent)
            Result.Started
        } catch (e: Exception) {
            SessionStartWindow.cancel()
            android.util.Log.w("StreamLauncher", "Could not start stream service", e)
            Result.Rejected("start_refused")
        }
    }

    // Remote start using the remembered selection, at the size and rate the computer asks for when it says.
    fun startFromPrefs(context: Context, opening: StreamOpening? = null): Result {
        val remembered = StreamPrefs.lastSelection(context)
        val selection = if (opening?.width != null && opening.height != null)
            (remembered ?: StreamPrefs.DEFAULT_SELECTION).copy(width = opening.width, height = opening.height)
        else remembered
        return start(context, selection, remote = true, fps = opening?.fps)
    }
}

// What a computer's start asks for, so a size that just failed (too much for the encoder) isn't what opens again.
// Each part is optional: an older computer sends none of them, and the phone then opens as it last did.
data class StreamOpening(val width: Int?, val height: Int?, val fps: Int?) {
    companion object {
        private const val MAX_SIDE = 16_384

        fun from(params: Map<String, String>): StreamOpening? {
            val w = params["width"]?.toIntOrNull()?.takeIf { it in 1..MAX_SIDE }
            val h = params["height"]?.toIntOrNull()?.takeIf { it in 1..MAX_SIDE }
            val fps = params["fps"]?.toIntOrNull()?.takeIf { it > 0 }?.coerceAtMost(120)
            val sized = w != null && h != null
            if (!sized && fps == null) return null
            return StreamOpening(if (sized) w else null, if (sized) h else null, fps)
        }
    }
}
