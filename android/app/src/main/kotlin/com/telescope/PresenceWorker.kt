package com.telescope

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.net.ConnectivityManager
import android.net.LinkProperties
import android.net.Network
import android.net.NetworkCapabilities
import android.net.NetworkRequest
import android.os.Build
import android.os.SystemClock
import androidx.core.content.ContextCompat
import java.net.Inet4Address
import java.net.InetSocketAddress
import java.nio.ByteBuffer
import java.nio.channels.DatagramChannel
import java.security.SecureRandom
import java.util.concurrent.atomic.AtomicBoolean
import kotlin.concurrent.thread

// Owned only while SessionEndpoint has an activity, stream or Wait owner; it never touches camera commands.
internal class PresenceWorker(private val context: Context, private val waiting: () -> Boolean, private val degraded: (Boolean) -> Unit) {
    private data class Endpoint(val network: Network, val address: String, val wifi: Boolean, val vpn: Boolean, val channel: DatagramChannel)
    private val running = AtomicBoolean(true)
    private val changed = AtomicBoolean(true)
    private val hinted = AtomicBoolean(true)
    private val io = PresenceSockets()
    private val cm = context.getSystemService(ConnectivityManager::class.java)
    private val callback = object : ConnectivityManager.NetworkCallback() {
        override fun onAvailable(network: Network) = networkChanged()
        override fun onLost(network: Network) = networkChanged()
        override fun onLinkPropertiesChanged(network: Network, properties: LinkProperties) = networkChanged()
    }
    private val power = object : BroadcastReceiver() {
        override fun onReceive(context: Context, intent: Intent) { hint() }
    }
    @Volatile private var knownCredentials = PairedComputers.list(context).computers.map { it.id to it.token }
    private val pairingsChanged: () -> Unit = {
        val current = PairedComputers.list(context).computers.map { it.id to it.token }
        if (current != knownCredentials) { knownCredentials = current; hint() }
    }
    private var registeredNetwork = false
    private var registeredPower = false

    init {
        try {
            val request = NetworkRequest.Builder().apply {
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
                    clearCapabilities()
                } else {
                    removeCapability(NetworkCapabilities.NET_CAPABILITY_NOT_RESTRICTED)
                    removeCapability(NetworkCapabilities.NET_CAPABILITY_TRUSTED)
                    removeCapability(NetworkCapabilities.NET_CAPABILITY_NOT_VPN)
                }
            }.build()
            cm?.registerNetworkCallback(request, callback)
            registeredNetwork = cm != null
        } catch (_: Exception) {}
        try {
            ContextCompat.registerReceiver(context, power, IntentFilter(Intent.ACTION_POWER_CONNECTED), ContextCompat.RECEIVER_NOT_EXPORTED)
            registeredPower = true
        } catch (_: Exception) {}
        PairedComputers.addListener(pairingsChanged)
        thread(name = "presence", isDaemon = true) { work() }
    }

    private fun networkChanged() { changed.set(true); hint() }
    fun hint() { hinted.set(true); io.wakeup() }
    fun refresh() { networkChanged() }

    fun stop() {
        running.set(false)
        io.close()
        if (registeredNetwork) runCatching { cm?.unregisterNetworkCallback(callback) }
        if (registeredPower) runCatching { context.unregisterReceiver(power) }
        PairedComputers.removeListener(pairingsChanged)
    }

    private fun endpoints(existing: List<Endpoint>): Pair<List<Endpoint>, Boolean> {
        val result = mutableListOf<Endpoint>()
        var failed = false
        var attempted = 0
        try {
            @Suppress("DEPRECATION")
            val networks = cm?.allNetworks.orEmpty()
            networkLoop@ for (network in networks) {
                val caps = cm?.getNetworkCapabilities(network) ?: continue
                val wifi = caps.hasTransport(NetworkCapabilities.TRANSPORT_WIFI) && caps.hasCapability(NetworkCapabilities.NET_CAPABILITY_NOT_VPN)
                val vpn = caps.hasTransport(NetworkCapabilities.TRANSPORT_VPN)
                if (!wifi && !vpn) continue
                for (link in cm?.getLinkProperties(network)?.linkAddresses.orEmpty()) {
                    val address = link.address
                    if (address !is Inet4Address || address.isLoopbackAddress || address.isLinkLocalAddress) continue
                    val ip = address.hostAddress ?: continue
                    if (attempted++ >= 8) break@networkLoop
                    val previous = existing.firstOrNull { it.network == network && it.address == ip && it.channel.isOpen }
                    if (previous != null) { result += previous; continue }
                    existing.filter { it.address == ip && it.network != network }.forEach { runCatching { it.channel.close() } }
                    try {
                        val channel = io.open {
                            network.bindSocket(it.socket())
                            it.bind(InetSocketAddress(address, PRESENCE_PORT))
                        } ?: break@networkLoop
                        result += Endpoint(network, ip, wifi, vpn, channel)
                    } catch (e: Exception) {
                        failed = true
                        android.util.Log.w("Presence", "Could not bind presence on a network", e)
                    }
                }
            }
        } catch (e: Exception) {
            failed = true
            android.util.Log.w("Presence", "Could not enumerate presence networks", e)
        }
        existing.filter { it !in result }.forEach { runCatching { it.channel.close() } }
        return result to failed
    }

    private fun work() {
        val random = SecureRandom()
        val clock = { SystemClock.elapsedRealtime() }
        val schedule = PresenceSchedule(clock) { spread -> if (spread == 0L) 0 else random.nextInt((spread * 2 + 1).toInt()).toLong() - spread }
        val exchange = PresenceExchange(PairedComputers.phoneId(context), BuildConfig.VERSION_NAME, clock) {
            ByteArray(16).also(random::nextBytes).joinToString("") { "%02x".format(it) }
        }
        var sockets = emptyList<Endpoint>()
        var retryAt = 0L
        var bindFailed = false
        var cursor = 0
        var wasLocalOnly = false
        val buffer = ByteBuffer.allocate(PRESENCE_PACKET_LIMIT + 1)
        try {
            while (running.get()) {
                val localOnly = StreamPrefs.localOnly(context)
                if (localOnly != wasLocalOnly) changed.set(true)
                wasLocalOnly = localOnly
                val rebuild = changed.getAndSet(false)
                if (rebuild || (!localOnly && (bindFailed || sockets.isEmpty()) && clock() >= retryAt)) {
                    if (rebuild) {
                        sockets.forEach { runCatching { it.channel.close() } }
                        sockets = emptyList()
                    }
                    val (available, failed) = if (localOnly) emptyList<Endpoint>() to false else endpoints(sockets)
                    sockets = available
                    bindFailed = failed
                    retryAt = clock() + if (failed) 1_000 else 30_000
                    if (!running.get()) break
                    degraded(!localOnly && (bindFailed || sockets.isEmpty()))
                }
                if (hinted.getAndSet(false)) schedule.hint(waiting())
                val computers = PairedComputers.list(context).computers.take(64)
                exchange.peers(computers)
                val local = boundedPresenceAddresses(sockets.map { it.address })
                if (!localOnly && schedule.due()) {
                    val targets = computers.filter { it.presence != false }.flatMap { computer ->
                        computer.addresses.take(4).map { computer to it }
                    }
                    if (targets.isNotEmpty()) {
                        repeat(minOf(targets.size, 16)) {
                            val (computer, target) = targets[cursor.mod(targets.size)]
                            cursor = (cursor + 1).mod(targets.size)
                            val socket = when (target.kind) {
                                PairingKind.LAN -> sockets.firstOrNull { it.wifi }
                                PairingKind.TAILSCALE -> sockets.firstOrNull { it.vpn }
                                PairingKind.OTHER -> sockets.firstOrNull { it.network == cm?.activeNetwork } ?: sockets.firstOrNull { it.wifi }
                            }
                            if (socket != null) exchange.request(computer, target.ip, local)?.let { send(socket, it) }
                        }
                    }
                }
                val retryDelay = if (bindFailed || sockets.isEmpty()) (retryAt - clock()).coerceAtLeast(0) else Long.MAX_VALUE
                val timeout = if (localOnly) null else minOf(schedule.delay(), retryDelay)
                for (channel in io.readable(timeout)) {
                    val socket = sockets.firstOrNull { it.channel == channel } ?: continue
                    for (packet in 0 until 16) {
                        buffer.clear()
                        val source = try { channel.receive(buffer) as? InetSocketAddress } catch (_: Exception) { changed.set(true); null } ?: break
                        if (!StreamPrefs.localOnly(context)) {
                            buffer.flip()
                            val bytes = ByteArray(buffer.remaining()).also { buffer.get(it) }
                            val address = source.address.hostAddress.orEmpty()
                            val (answers, contact) = exchange.receive(bytes, address, PairedComputers.list(context).computers.take(64), local, source.port)
                            for (answer in answers) send(socket, answer)
                            contact?.let { PairedComputers.presenceContact(context, it) }
                        }
                    }
                }
            }
        } catch (e: Exception) {
            if (running.get()) {
                android.util.Log.w("Presence", "Presence worker stopped", e)
                degraded(true)
            }
        } finally {
            io.close()
        }
    }

    private fun send(socket: Endpoint, packet: PresenceSend) {
        if (!running.get() || StreamPrefs.localOnly(context)) return
        runCatching { socket.channel.send(ByteBuffer.wrap(packet.bytes), InetSocketAddress(packet.address, packet.port)) }
    }
}
