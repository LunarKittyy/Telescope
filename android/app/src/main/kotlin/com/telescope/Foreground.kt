package com.telescope

import android.app.Notification
import android.app.Service
import android.content.pm.ServiceInfo
import android.os.Build

// Starts the camera services in the foreground, with the microphone type once it's allowed
object Foreground {
    // Returns whether the microphone type is in force
    fun promote(svc: Service, id: Int, n: Notification, withMic: Boolean): Boolean {
        // The type parameter only works on R+; before that the manifest declaration applies
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) svc.startForeground(id, n, types(withMic)) else svc.startForeground(id, n)
        return micInForce(Build.VERSION.SDK_INT, withMic)
    }

    // Camera, and the microphone too when it's allowed (Android refuses a type whose permission is missing)
    fun types(withMic: Boolean): Int =
        ServiceInfo.FOREGROUND_SERVICE_TYPE_CAMERA or (if (withMic) ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE else 0)

    // Before Android 11 the manifest types apply, so the microphone counts as in force
    fun micInForce(sdk: Int, withMic: Boolean): Boolean = withMic || sdk < Build.VERSION_CODES.R
}
