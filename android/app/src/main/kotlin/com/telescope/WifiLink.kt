package com.telescope

import android.content.Context
import android.net.ConnectivityManager
import android.net.NetworkCapabilities
import android.net.wifi.WifiInfo
import android.os.Build

// The phone's Wi-Fi as the desktop needs it to say why a stream can't keep up: the band, the signal and the link speed
data class WifiLink(val band: String?, val rssi: Int?, val mbps: Int?) {
    companion object {
        /** "2.4", "5" or "6" (GHz) for a channel's frequency in MHz, or null when it isn't one of those. */
        fun band(mhz: Int): String? = when (mhz) {
            in 2400..2500 -> "2.4"
            in 4900..5899 -> "5"
            in 5925..7125 -> "6"
            else -> null
        }

        /** The Wi-Fi the phone is on (Android 10 and newer, no extra permission), or null on mobile data or older. */
        fun current(context: Context): WifiLink? {
            if (Build.VERSION.SDK_INT < Build.VERSION_CODES.Q) return null
            return try {
                val cm = context.getSystemService(ConnectivityManager::class.java) ?: return null
                val caps = cm.getNetworkCapabilities(cm.activeNetwork) ?: return null
                if (!caps.hasTransport(NetworkCapabilities.TRANSPORT_WIFI)) return null
                val info = caps.transportInfo as? WifiInfo ?: return null
                WifiLink(
                    band = band(info.frequency),
                    rssi = info.rssi.takeIf { it in -126..0 },  // -127 is "unknown"
                    mbps = info.linkSpeed.takeIf { it > 0 },
                )
            } catch (_: Exception) {
                null
            }
        }
    }
}
