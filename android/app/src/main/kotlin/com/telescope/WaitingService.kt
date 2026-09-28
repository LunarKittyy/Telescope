package com.telescope

import android.annotation.SuppressLint
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder
import androidx.core.app.NotificationCompat
import androidx.core.content.ContextCompat

// "Wait for my computer": keeps the session port up while the screen is off or the app is closed, so a paired computer
// can still start the camera. Android 14+ only lets a camera or microphone foreground service start while the app is on
// screen, so this one takes those types then and the stream runs under it (StreamLauncher starts CameraStreamService as
// a plain service). It never opens the camera or mic itself and holds no wake lock. Opt-in and off by default, and only
// ever started while MainActivity is visible, never from the background or at boot. Not sticky: if Android kills it, it
// comes back the next time the app is opened.
class WaitingService : Service() {

    private var holdsEndpoint = false

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
        RecentRuns.recordCrashes(this)
        val ch = NotificationChannel(CHANNEL_ID, "Waiting for the computer", NotificationManager.IMPORTANCE_LOW)
            .apply { description = "Shown while Telescope keeps waiting for a paired computer to start the camera" }
        (getSystemService(NOTIFICATION_SERVICE) as NotificationManager).createNotificationChannel(ch)
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == ACTION_STOP) {
            // "Stop waiting" on the notification: the same as switching it off in the app.
            StreamPrefs.setWaitForComputer(this, false)
            stopSelf()
            return START_NOT_STICKY
        }
        try {
            // Must come first: Android stops an app whose foreground service doesn't promote itself in time.
            startForegroundCompat()
        } catch (e: Exception) {
            android.util.Log.w(TAG, "Could not start waiting in the foreground", e)
            covering = false
            stopSelf()
            return START_NOT_STICKY
        }
        if (!holdsEndpoint) {
            SessionEndpoint.acquire(this, SessionEndpoint.OWNER_WAITING)
            holdsEndpoint = true
        }
        return START_NOT_STICKY
    }

    override fun onDestroy() {
        covering = false
        hasMic = false
        // A stream running under this service loses its camera access with it.
        CameraStreamService.instance?.onCoverLost()
        if (holdsEndpoint) {
            SessionEndpoint.release(SessionEndpoint.OWNER_WAITING)
            holdsEndpoint = false
        }
        super.onDestroy()
    }

    private fun startForegroundCompat() {
        val withMic = AudioStreamer.permitted(this)
        val n = buildNotification(this, streaming = CameraStreamService.instance?.isStreaming == true)
        // Camera, and the mic once it's allowed (Android refuses a type whose permission is missing). Before Android 11
        // the manifest types apply and a background service may use the camera anyway.
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            val types = ServiceInfo.FOREGROUND_SERVICE_TYPE_CAMERA or
                (if (withMic) ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE else 0)
            startForeground(NOTIF_ID, n, types)
        } else {
            startForeground(NOTIF_ID, n)
        }
        hasMic = withMic || Build.VERSION.SDK_INT < Build.VERSION_CODES.R
        covering = true
    }

    companion object {
        private const val TAG = "WaitingService"
        private const val CHANNEL_ID = "telescope_waiting"
        private const val NOTIF_ID = 2  // CameraStreamService uses 1; both can show at once
        private const val ACTION_STOP = "com.telescope.action.STOP_WAITING"

        // True while this runs in the foreground with the camera type: a stream can then run under it.
        @Volatile
        var covering = false
            private set

        // Whether that includes the microphone type.
        @Volatile
        var hasMic = false
            private set

        private fun buildNotification(context: Context, streaming: Boolean): android.app.Notification {
            val open = PendingIntent.getActivity(context, 0,
                Intent(context, MainActivity::class.java), PendingIntent.FLAG_IMMUTABLE)
            val stop = PendingIntent.getService(context, 0,
                Intent(context, WaitingService::class.java).setAction(ACTION_STOP), PendingIntent.FLAG_IMMUTABLE)
            return NotificationCompat.Builder(context, CHANNEL_ID)
                .setContentTitle("Telescope")
                .setContentText(if (streaming) "Camera is streaming" else "Waiting for your computer")
                .setSmallIcon(R.drawable.ic_notification)
                .setColor(ContextCompat.getColor(context, R.color.colorPrimary))
                .setContentIntent(open)
                .addAction(0, "Stop waiting", stop)
                .setOngoing(true)
                .build()
        }

        // A stream under this service shows here instead of its own notification. Only a notify: calling
        // startForeground again from the background would be refused. Without the notification permission the
        // foreground notification is hidden anyway, so a refused notify changes nothing.
        @SuppressLint("MissingPermission")
        fun showStreaming(context: Context, streaming: Boolean) {
            if (!covering) return
            (context.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager)
                .notify(NOTIF_ID, buildNotification(context, streaming))
        }

        // Only from a visible activity: starting a foreground service from the background is refused.
        fun start(context: Context) {
            try {
                ContextCompat.startForegroundService(context, Intent(context, WaitingService::class.java))
            } catch (e: Exception) {
                android.util.Log.w(TAG, "Could not start waiting", e)
            }
        }

        fun stop(context: Context) {
            context.stopService(Intent(context, WaitingService::class.java))
        }
    }
}
