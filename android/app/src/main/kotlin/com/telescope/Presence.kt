package com.telescope

import java.security.MessageDigest
import javax.crypto.Mac
import javax.crypto.spec.SecretKeySpec

internal const val PRESENCE_PORT = 8769
internal const val PRESENCE_PACKET_LIMIT = 512

internal fun presenceAddress(ip: String): Boolean = isValidIpv4(ip) && !ip.startsWith("127.") && !ip.startsWith("169.254.") && ip != "0.0.0.0" && ip.split('.')[0].toInt() !in 224..239

internal fun presenceKind(ip: String): PairingKind {
    val parts = ip.split('.').map { it.toInt() }
    return when {
        parts[0] == 100 && parts[1] in 64..127 -> PairingKind.TAILSCALE
        parts[0] == 10 || (parts[0] == 172 && parts[1] in 16..31) || (parts[0] == 192 && parts[1] == 168) -> PairingKind.LAN
        else -> PairingKind.OTHER
    }
}

internal fun boundedPresenceAddresses(addresses: List<String>): List<String> {
    val usable = addresses.filter(::presenceAddress).distinct()
    val picked = usable.take(4)
    val tailscale = usable.firstOrNull { presenceKind(it) == PairingKind.TAILSCALE }
    return if (tailscale != null && tailscale !in picked) picked.take(3) + tailscale else picked
}

internal data class PresencePacket(val type: String, val sender: String, val recipient: String, val challenge: String, val version: String, val addresses: List<String>) {
    private fun body() = listOf("TP1", type, sender, recipient, challenge, version, addresses.joinToString(",")).joinToString("\n")

    fun encode(token: String): ByteArray? {
        if (token.isEmpty() || !valid()) return null
        val body = body()
        val packet = "$body\n${sign(token, body).joinToString("") { "%02x".format(it) }}".toByteArray(Charsets.UTF_8)
        return packet.takeIf { it.size <= PRESENCE_PACKET_LIMIT }
    }

    private fun valid(): Boolean = type in listOf("q", "a") && identity.matches(sender) && identity.matches(recipient) && nonce.matches(challenge) && version.length <= 64 && version.all { it.code in 32..126 && it != ',' } && addresses.size <= 4 && addresses.all(::presenceAddress)

    companion object {
        private val identity = Regex("[A-Za-z0-9_-]{1,64}")
        private val nonce = Regex("[0-9a-f]{32}")
        private val signature = Regex("[0-9a-f]{64}")

        private fun hmac(key: ByteArray, bytes: ByteArray): ByteArray = Mac.getInstance("HmacSHA256").run {
            init(SecretKeySpec(key, "HmacSHA256"))
            doFinal(bytes)
        }

        private fun sign(token: String, body: String): ByteArray = hmac(hmac(token.toByteArray(Charsets.UTF_8), "telescope-presence-key-v1".toByteArray(Charsets.UTF_8)), body.toByteArray(Charsets.UTF_8))

        fun decode(bytes: ByteArray, token: String): PresencePacket? {
            if (token.isEmpty() || bytes.size > PRESENCE_PACKET_LIMIT || bytes.any { it.toInt() !in 10..126 }) return null
            val lines = bytes.toString(Charsets.UTF_8).split('\n')
            if (lines.size != 8 || lines[0] != "TP1" || !signature.matches(lines[7])) return null
            val packet = PresencePacket(lines[1], lines[2], lines[3], lines[4], lines[5], lines[6].takeIf { it.isNotEmpty() }?.split(',').orEmpty())
            if (!packet.valid()) return null
            val supplied = lines[7].chunked(2).map { it.toInt(16).toByte() }.toByteArray()
            return packet.takeIf { MessageDigest.isEqual(sign(token, lines.take(7).joinToString("\n")), supplied) }
        }
    }
}

// The caller owns the monotonic clock and jitter, including when Android wakes after a long sleep.
internal class PresenceSchedule(private val clock: () -> Long, private val jitter: (Long) -> Long) {
    private var interaction = clock()
    private var next = interaction
    private var waiting = false

    fun delay(): Long = (next - clock()).coerceAtLeast(0)

    fun hint(waiting: Boolean = this.waiting) {
        val now = clock()
        if (waiting == this.waiting && now - interaction < 1_000) return
        this.waiting = waiting
        interaction = now
        next = minOf(next, now)
    }

    fun due(): Boolean {
        val now = clock()
        if (now < next) return false
        val age = now - interaction
        val interval = when {
            waiting -> 60_000L
            age < 10_000 -> 2_000L
            age < 180_000 -> 5_000L
            else -> 45_000L
        }
        val spread = when {
            waiting -> 0L
            interval == 45_000L -> 15_000L
            else -> interval / 10
        }
        next = now + interval + jitter(spread).coerceIn(-spread, spread)
        return true
    }
}

internal data class PresenceSend(val address: String, val bytes: ByteArray, val port: Int = PRESENCE_PORT)
internal data class PresenceContact(val computer: PairedComputer, val address: String, val version: String, val addresses: List<String>)

// A datagram can only update a pairing after the answer to our own fresh challenge arrives from its destination.
internal class PresenceExchange(private val phoneId: String, private val version: String, private val clock: () -> Long, private val nonce: () -> String) {
    private data class Challenge(val id: String, val token: String, val address: String, val port: Int, val expires: Long)
    private val pending = mutableMapOf<String, Challenge>()
    private val replay = mutableMapOf<String, MutableMap<String, Long>>()
    private val lastRequest = mutableMapOf<String, Long>()
    private val confirmed = mutableMapOf<String, Long>()

    private fun expire() {
        val now = clock()
        pending.entries.removeAll { it.value.expires <= now }
        replay.values.forEach { requests -> requests.entries.removeAll { it.value <= now } }
        confirmed.entries.removeAll { it.value <= now }
    }

    fun peers(computers: List<PairedComputer>) {
        val credentials = computers.take(64).map { "${it.id}:${it.token}" }.toSet()
        pending.entries.removeAll { "${it.value.id}:${it.value.token}" !in credentials }
        replay.keys.retainAll(credentials)
        lastRequest.keys.retainAll(credentials)
        confirmed.keys.removeAll { source -> credentials.none { source.startsWith("$it:") } }
    }

    fun request(computer: PairedComputer, address: String, local: List<String>, port: Int = PRESENCE_PORT): PresenceSend? {
        expire()
        if (!presenceAddress(address) || pending.size >= 64) return null
        val challenge = nonce()
        val bytes = PresencePacket("q", phoneId, computer.id, challenge, version, boundedPresenceAddresses(local)).encode(computer.token) ?: return null
        pending[challenge] = Challenge(computer.id, computer.token, address, port, clock() + 10_000)
        return PresenceSend(address, bytes, port)
    }

    fun receive(bytes: ByteArray, address: String, computers: List<PairedComputer>, local: List<String>, port: Int = PRESENCE_PORT): Pair<List<PresenceSend>, PresenceContact?> {
        expire()
        peers(computers)
        if (!presenceAddress(address) || bytes.size > PRESENCE_PACKET_LIMIT) return emptyList<PresenceSend>() to null
        val sender = bytes.toString(Charsets.UTF_8).split('\n').getOrNull(2) ?: return emptyList<PresenceSend>() to null
        val computer = computers.take(64).firstOrNull { it.id == sender } ?: return emptyList<PresenceSend>() to null
        val packet = PresencePacket.decode(bytes, computer.token) ?: return emptyList<PresenceSend>() to null
        if (packet.recipient != phoneId) return emptyList<PresenceSend>() to null
        if (packet.type == "a") {
            val challenge = pending[packet.challenge] ?: return emptyList<PresenceSend>() to null
            if (challenge.id != computer.id || challenge.token != computer.token || challenge.address != address || challenge.port != port) return emptyList<PresenceSend>() to null
            pending.remove(packet.challenge)
            val source = "${computer.id}:${computer.token}:$address:$port"
            if (confirmed.size >= 64 && source !in confirmed) confirmed.minByOrNull { it.value }?.key?.let(confirmed::remove)
            confirmed[source] = clock() + 10_000
            return emptyList<PresenceSend>() to PresenceContact(computer, address, packet.version, packet.addresses)
        }
        val now = clock()
        val credential = "${computer.id}:${computer.token}"
        val requests = replay.getOrPut(credential) { mutableMapOf() }
        if (packet.challenge in requests || requests.size >= 512 || now - (lastRequest[credential] ?: Long.MIN_VALUE / 2) < 1_000) return emptyList<PresenceSend>() to null
        requests[packet.challenge] = now + 180_000
        lastRequest[credential] = now
        val answer = PresencePacket("a", phoneId, computer.id, packet.challenge, version, boundedPresenceAddresses(local)).encode(computer.token) ?: return emptyList<PresenceSend>() to null
        val sends = mutableListOf(PresenceSend(address, answer, port))
        if ("${computer.id}:${computer.token}:$address:$port" !in confirmed && pending.values.none { it.id == computer.id && it.address == address && it.port == port }) request(computer, address, local, port)?.let(sends::add)
        return sends to null
    }
}

internal enum class DesktopVersionNotice { Legacy, Different }

// Written the way the app shows its own version (5.1.4-nightly.4255), whichever way the desktop spelt it
internal fun displayAppVersion(version: String): String =
    Regex("^(\\d+\\.\\d+\\.\\d+) (nightly|dev)(?: (\\d+))?$").matchEntire(version.trim())?.let { m ->
        val (base, channel, build) = m.destructured
        if (build.isEmpty()) "$base-$channel" else "$base-$channel.$build"
    } ?: version.trim()

// Only a computer heard from since this app started counts, so a stored version from a computer that is now off never shows
internal fun desktopVersionNotice(desktop: String?, phone: String, heardThisRun: Boolean = true): DesktopVersionNotice? {
    if (desktop == null || !heardThisRun) return null
    if (desktop.isEmpty()) return DesktopVersionNotice.Legacy
    fun dev(v: String) = v.endsWith(" dev") || v.contains("-dev.")
    if (desktop.trim() == phone.trim() || dev(desktop) || dev(phone)) return null
    val nightly = Regex("^\\d+\\.\\d+\\.\\d+(?:-nightly\\.| nightly )(\\d+)$")
    val desktopBuild = nightly.matchEntire(desktop.trim())?.groupValues?.get(1)?.toLongOrNull()
    val phoneBuild = nightly.matchEntire(phone.trim())?.groupValues?.get(1)?.toLongOrNull()
    if (desktopBuild != null && desktopBuild == phoneBuild) return null
    return DesktopVersionNotice.Different
}
