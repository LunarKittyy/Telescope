package com.telescope

import android.content.Context
import android.net.nsd.NsdManager
import android.net.nsd.NsdServiceInfo

// Announces the session port on the LAN (mDNS/DNS-SD via NsdManager) so the desktop can find this
// phone's current address after the router hands it a new IP. The TXT "id" is the phone id the
// desktop paired with; the address alone proves nothing, the desktop still authenticates on connect.
class LanAnnouncer(private val context: Context) {
    private var listener: NsdManager.RegistrationListener? = null

    @Synchronized
    fun start(port: Int) {
        if (listener != null) return
        // Local only means "keep this phone off the network", which includes not advertising it.
        if (StreamPrefs.localOnly(context)) return
        val nsd = context.getSystemService(Context.NSD_SERVICE) as? NsdManager ?: return
        val info = NsdServiceInfo().apply {
            serviceName = "Telescope ${PairedComputers.phoneName(context)}"
            serviceType = SERVICE_TYPE
            setPort(port)
            setAttribute("id", PairedComputers.phoneId(context))
        }
        val l = object : NsdManager.RegistrationListener {
            override fun onServiceRegistered(info: NsdServiceInfo) {}
            override fun onRegistrationFailed(info: NsdServiceInfo, code: Int) {
                android.util.Log.w(TAG, "NSD registration failed: $code")
            }
            override fun onServiceUnregistered(info: NsdServiceInfo) {}
            override fun onUnregistrationFailed(info: NsdServiceInfo, code: Int) {}
        }
        try {
            nsd.registerService(info, NsdManager.PROTOCOL_DNS_SD, l)
            listener = l
        } catch (e: Exception) {
            android.util.Log.w(TAG, "NSD registration threw", e)
        }
    }

    @Synchronized
    fun stop() {
        val l = listener ?: return
        listener = null
        val nsd = context.getSystemService(Context.NSD_SERVICE) as? NsdManager ?: return
        try { nsd.unregisterService(l) } catch (_: Exception) {}
    }

    companion object {
        const val SERVICE_TYPE = "_telescope._tcp"
        private const val TAG = "LanAnnouncer"
    }
}
