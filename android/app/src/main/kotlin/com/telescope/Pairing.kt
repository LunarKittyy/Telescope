package com.telescope

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import java.net.NoRouteToHostException
import java.net.SocketTimeoutException
import java.net.UnknownHostException

// QR-code pairing payload and routing logic; Android-free for JVM unit testing
// Version bumped in lockstep with desktop; mismatch means one is stale
const val PAIRING_PROTOCOL_VERSION = 2

@Serializable
enum class PairingKind {
    /** A physical local network - reachable over Wi-Fi, and the one route
     *  worth forcing off the default network when a VPN holds it. */
    @SerialName("lan") LAN,
    @SerialName("tailscale") TAILSCALE,
    @SerialName("other") OTHER,
}

@Serializable
data class PairingCandidate(
    val ip: String,
    /** The desktop-side adapter this address belongs to, for diagnostics. */
    @SerialName("interface") val iface: String,
    val kind: PairingKind,
)

@Serializable
data class PairingOffer(
    val version: Int,
    val port: Int,
    val candidates: List<PairingCandidate>,
    val nonce: String,
    val token: String,
)

sealed interface PairingParse {
    data class Ok(val offer: PairingOffer) : PairingParse
    /** Well-formed JSON, but a protocol version this app doesn't speak. */
    data object UnsupportedVersion : PairingParse
    data object Invalid : PairingParse
}

/** Which of the phone's own networks an attempt goes out over. */
enum class PairingRouteKind { WIFI, DEFAULT }

data class PairingRoute(val candidate: PairingCandidate, val via: PairingRouteKind)

data class PairingAttemptFailure(
    val ip: String,
    val via: PairingRouteKind,
    /** Short human-readable cause, e.g. "timed out". */
    val problem: String,
)

private val pairingJson = Json { ignoreUnknownKeys = true }

// Parses a scanned (or adb-pushed) pairing payload, rejecting anything unusable: wrong version, malformed JSON, bad port, missing nonce/token, or an empty/invalid candidate list.
fun parsePairingOffer(raw: String): PairingParse {
    val offer = try {
        pairingJson.decodeFromString(PairingOffer.serializer(), raw)
    } catch (_: Exception) {
        // Check version separately since v1 payloads also fail deserialization.
        return if (rawVersion(raw)?.let { it != PAIRING_PROTOCOL_VERSION } == true)
            PairingParse.UnsupportedVersion
        else
            PairingParse.Invalid
    }
    if (offer.version != PAIRING_PROTOCOL_VERSION) return PairingParse.UnsupportedVersion
    if (offer.port !in 1..65535) return PairingParse.Invalid
    if (offer.nonce.isBlank() || offer.token.isBlank()) return PairingParse.Invalid
    if (offer.candidates.isEmpty()) return PairingParse.Invalid
    if (offer.candidates.any { !isValidIpv4(it.ip) }) return PairingParse.Invalid
    return PairingParse.Ok(offer)
}

private fun rawVersion(raw: String): Int? = try {
    Json.parseToJsonElement(raw)
        .let { it as? kotlinx.serialization.json.JsonObject }
        ?.get("version")
        ?.let { (it as? kotlinx.serialization.json.JsonPrimitive)?.content?.toIntOrNull() }
} catch (_: Exception) {
    null
}

/** Same rules as the desktop's valid_ipv4(): four decimal octets, 0-255, no
 *  leading zeros (which some resolvers read as octal). */
fun isValidIpv4(ip: String): Boolean {
    val parts = ip.split(".")
    if (parts.size != 4) return false
    return parts.all { part ->
        val value = part.toIntOrNull() ?: return@all false
        value in 0..255 && value.toString() == part
    }
}

// Try LAN over Wi-Fi first (works through VPNs), then all over default network
fun pairingRoutes(candidates: List<PairingCandidate>, hasWifi: Boolean): List<PairingRoute> {
    val overWifi =
        if (hasWifi)
            candidates.filter { it.kind == PairingKind.LAN }
                .map { PairingRoute(it, PairingRouteKind.WIFI) }
        else emptyList()
    return overWifi + candidates.map { PairingRoute(it, PairingRouteKind.DEFAULT) }
}

/** Connect/read timeout for a single attempt. */
const val PAIR_ATTEMPT_TIMEOUT_MS = 2_000

// Ceiling on the whole run, however many candidates there are - otherwise eight candidates tried twice is half a minute of progress-free waiting.
const val PAIR_TOTAL_BUDGET_MS = 12_000L

/** Below this there isn't enough left for an attempt to mean anything. */
private const val PAIR_MIN_ATTEMPT_MS = 500

// How long the next attempt may take given elapsedMs so far, or null when the budget is spent and remaining candidates should be abandoned.
fun attemptTimeoutMs(elapsedMs: Long, budgetMs: Long = PAIR_TOTAL_BUDGET_MS): Int? {
    val remaining = budgetMs - elapsedMs
    if (remaining < PAIR_MIN_ATTEMPT_MS) return null
    return minOf(PAIR_ATTEMPT_TIMEOUT_MS.toLong(), remaining).toInt()
}

/** Short, non-technical cause for the "Tried:" list in a failure report. */
fun describeNetworkError(error: Throwable): String {
    val message = error.message.orEmpty().lowercase()
    return when {
        error is SocketTimeoutException -> "timed out"
        error is NoRouteToHostException -> "no route to host"
        message.contains("refused") -> "connection refused"
        message.contains("unreachable") -> "unreachable"
        error is UnknownHostException -> "unreachable"
        message.isNotBlank() -> message
        else -> error.javaClass.simpleName
    }
}

private fun PairingRouteKind.label(): String = when (this) {
    PairingRouteKind.WIFI -> "Wi-Fi"
    PairingRouteKind.DEFAULT -> "the default network"
}

// Failure message listing tried candidates and suggesting VPN/guest Wi-Fi/USB options
fun pairingFailureMessage(failures: List<PairingAttemptFailure>, untried: Int = 0): String {
    val tried = failures.joinToString("\n") { "• ${it.ip} over ${it.via.label()}: ${it.problem}" }
    return buildString {
        append("Could not reach the desktop.\n\n")
        if (failures.isEmpty()) {
            append("The QR code offered no address this phone could try.\n\n")
        } else {
            append("Tried:\n").append(tried).append("\n")
            if (untried > 0) {
                append("• $untried more not tried - gave up after ")
                append("${PAIR_TOTAL_BUDGET_MS / 1000} seconds\n")
            }
            append("\n")
        }
        append(
            "Your VPN may be blocking local-network access. Enable its LAN-access " +
                "option, pause the VPN temporarily, or use USB pairing."
        )
    }
}
