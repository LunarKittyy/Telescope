package com.telescope

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
// can still start the camera. Holds no camera, microphone or wake lock itself; a start from the computer goes through
// StreamLauncher like any other. Opt-in and off by default, and only ever started while MainActivity is visible, never
// from the background or at boot. Not sticky: if Android kills it, it comes back the next time the app is opened.
class WaitingService : Service() {

    private var holdsEndpoint = false

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
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
        if (holdsEndpoint) {
            SessionEndpoint.release(SessionEndpoint.OWNER_WAITING)
            holdsEndpoint = false
        }
        super.onDestroy()
    }

    private fun startForegroundCompat() {
        val open = PendingIntent.getActivity(this, 0,
            Intent(this, MainActivity::class.java), PendingIntent.FLAG_IMMUTABLE)
        val stop = PendingIntent.getService(this, 0,
            Intent(this, WaitingService::class.java).setAction(ACTION_STOP), PendingIntent.FLAG_IMMUTABLE)
        val n = NotificationCompat.Builder(this, CHANNEL_ID)
            .setContentTitle("Telescope")
            .setContentText("Waiting for your computer")
            .setSmallIcon(R.drawable.ic_notification)
            .setColor(ContextCompat.getColor(this, R.color.colorPrimary))
            .setContentIntent(open)
            .addAction(0, "Stop waiting", stop)
            .setOngoing(true)
            .build()
        // The special-use type exists from Android 14; older versions take the type from the manifest.
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
            startForeground(NOTIF_ID, n, ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE)
        } else {
            startForeground(NOTIF_ID, n)
        }
    }

    companion object {
        private const val TAG = "WaitingService"
        private const val CHANNEL_ID = "telescope_waiting"
        private const val NOTIF_ID = 2  // CameraStreamService uses 1; both can show at once
        private const val ACTION_STOP = "com.telescope.action.STOP_WAITING"

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
