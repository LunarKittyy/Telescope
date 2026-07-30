package com.telescope

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import java.net.ConnectException
import java.net.NoRouteToHostException
import java.net.SocketTimeoutException

class PairingTest {

    private fun payload(
        version: Int = PAIRING_PROTOCOL_VERSION,
        port: String = "8765",
        candidates: String = """[{"ip":"192.168.1.42","interface":"Wi-Fi","kind":"lan"}]""",
        nonce: String = "nonce-abc",
        token: String = "token-xyz",
    ) = """{"version":$version,"port":$port,"candidates":$candidates,"nonce":"$nonce","token":"$token"}"""

    private fun ok(raw: String): PairingOffer {
        val parsed = parsePairingOffer(raw)
        assertTrue(parsed is PairingParse.Ok, "expected a valid payload, got $parsed")
        return (parsed as PairingParse.Ok).offer
    }

    // ── Parsing ───────────────────────────────────────────────────────────────

    @Test
    fun `parses a version 2 payload with lan and tailscale candidates`() {
        val offer = ok(payload(candidates = """[
            {"ip":"192.168.1.42","interface":"Wi-Fi","kind":"lan"},
            {"ip":"100.90.12.34","interface":"tailscale0","kind":"tailscale"},
            {"ip":"203.0.113.7","interface":"eth9","kind":"other"}
        ]"""))

        assertEquals(2, offer.version)
        assertEquals(8765, offer.port)
        assertEquals("nonce-abc", offer.nonce)
        assertEquals("token-xyz", offer.token)
        assertEquals(
            listOf(
                PairingCandidate("192.168.1.42", "Wi-Fi", PairingKind.LAN),
                PairingCandidate("100.90.12.34", "tailscale0", PairingKind.TAILSCALE),
                PairingCandidate("203.0.113.7", "eth9", PairingKind.OTHER),
            ),
            offer.candidates,
        )
    }

    @Test
    fun `ignores unknown top-level fields`() {
        val raw = payload().dropLast(1) + ""","future":{"nested":true}}"""
        assertEquals("nonce-abc", ok(raw).nonce)
    }

    @Test
    fun `rejects other protocol versions as unsupported, not invalid`() {
        assertEquals(PairingParse.UnsupportedVersion, parsePairingOffer(payload(version = 3)))
        // A v1 payload has no "candidates" at all - still a version problem,
        // and the user needs to hear "update", not "invalid QR code".
        assertEquals(
            PairingParse.UnsupportedVersion,
            parsePairingOffer("""{"version":1,"port":8765,"ips":["192.168.1.42"],"nonce":"n","token":"t"}"""),
        )
    }

    @Test
    fun `rejects malformed and non-pairing payloads`() {
        listOf(
            "",
            "not json",
            "{}",
            """{"version":2}""",
            "https://example.com",
        ).forEach { assertEquals(PairingParse.Invalid, parsePairingOffer(it), "for: $it") }
    }

    @Test
    fun `rejects an empty candidate list`() {
        assertEquals(PairingParse.Invalid, parsePairingOffer(payload(candidates = "[]")))
    }

    @Test
    fun `rejects candidates that are not valid IPv4 literals`() {
        listOf(
            """[{"ip":"not-an-ip","interface":"Wi-Fi","kind":"lan"}]""",
            """[{"ip":"192.168.1.256","interface":"Wi-Fi","kind":"lan"}]""",
            """[{"ip":"01.2.3.4","interface":"Wi-Fi","kind":"lan"}]""",
            """[{"ip":"192.168.1","interface":"Wi-Fi","kind":"lan"}]""",
            """[{"ip":"fe80::1","interface":"Wi-Fi","kind":"lan"}]""",
            // One bad entry poisons the batch rather than being skipped: the
            // desktop that generated it isn't behaving, so don't half-trust it.
            """[{"ip":"192.168.1.42","interface":"Wi-Fi","kind":"lan"},
                {"ip":"nope","interface":"eth0","kind":"lan"}]""",
        ).forEach { assertEquals(PairingParse.Invalid, parsePairingOffer(payload(candidates = it)), "for: $it") }
    }

    @Test
    fun `rejects candidates with a kind this app does not know`() {
        assertEquals(
            PairingParse.Invalid,
            parsePairingOffer(payload(candidates = """[{"ip":"192.168.1.42","interface":"Wi-Fi","kind":"relay"}]""")),
        )
    }

    @Test
    fun `rejects a candidate missing the interface field`() {
        assertEquals(
            PairingParse.Invalid,
            parsePairingOffer(payload(candidates = """[{"ip":"192.168.1.42","kind":"lan"}]""")),
        )
    }

    @Test
    fun `rejects unusable ports and blank secrets`() {
        assertEquals(PairingParse.Invalid, parsePairingOffer(payload(port = "0")))
        assertEquals(PairingParse.Invalid, parsePairingOffer(payload(port = "70000")))
        assertEquals(PairingParse.Invalid, parsePairingOffer(payload(nonce = "")))
        assertEquals(PairingParse.Invalid, parsePairingOffer(payload(token = "   ")))
    }

    @Test
    fun `accepts the loopback candidate USB pairing advertises`() {
        val offer = ok(payload(candidates = """[{"ip":"127.0.0.1","interface":"USB (adb)","kind":"other"}]"""))
        assertEquals(PairingKind.OTHER, offer.candidates.single().kind)
    }

    @Test
    fun `validates IPv4 literals the same way the desktop does`() {
        listOf("0.0.0.0", "255.255.255.255", "1.2.3.4").forEach { assertTrue(isValidIpv4(it), it) }
        listOf("256.2.3.4", "01.2.3.4", "1.2.3", "1.2.3.4.5", "a.b.c.d", "", "1.2.3.-1")
            .forEach { assertFalse(isValidIpv4(it), it) }
    }

    // ── Attempt ordering ──────────────────────────────────────────────────────

    private val lan = PairingCandidate("192.168.1.42", "Wi-Fi", PairingKind.LAN)
    private val lan2 = PairingCandidate("10.1.2.3", "eth0", PairingKind.LAN)
    private val ts = PairingCandidate("100.90.12.34", "tailscale0", PairingKind.TAILSCALE)

    @Test
    fun `tries LAN candidates over Wi-Fi first, then everything over the default network`() {
        assertEquals(
            listOf(
                PairingRoute(lan, PairingRouteKind.WIFI),
                PairingRoute(lan2, PairingRouteKind.WIFI),
                PairingRoute(lan, PairingRouteKind.DEFAULT),
                PairingRoute(lan2, PairingRouteKind.DEFAULT),
                PairingRoute(ts, PairingRouteKind.DEFAULT),
            ),
            pairingRoutes(listOf(lan, lan2, ts), hasWifi = true),
        )
    }

    @Test
    fun `never binds a non-LAN candidate to Wi-Fi`() {
        // Tailscale's route is the tunnel itself, and USB pairing goes to
        // loopback - forcing either onto the Wi-Fi interface would break it.
        val usb = PairingCandidate("127.0.0.1", "USB (adb)", PairingKind.OTHER)
        val routes = pairingRoutes(listOf(ts, usb), hasWifi = true)
        assertTrue(routes.all { it.via == PairingRouteKind.DEFAULT })
    }

    @Test
    fun `falls back to the default network when there is no Wi-Fi network`() {
        assertEquals(
            listOf(
                PairingRoute(lan, PairingRouteKind.DEFAULT),
                PairingRoute(ts, PairingRouteKind.DEFAULT),
            ),
            pairingRoutes(listOf(lan, ts), hasWifi = false),
        )
    }

    // ── Failure reporting ─────────────────────────────────────────────────────

    @Test
    fun `failure message names every attempt and points at the VPN and USB`() {
        val message = pairingFailureMessage(listOf(
            PairingAttemptFailure("192.168.1.42", PairingRouteKind.WIFI, "timed out"),
            PairingAttemptFailure("100.90.12.34", PairingRouteKind.DEFAULT, "unreachable"),
        ))

        assertEquals(
            """
            Could not reach the desktop.

            Tried:
            • 192.168.1.42 over Wi-Fi: timed out
            • 100.90.12.34 over the default network: unreachable

            Your VPN may be blocking local-network access. Enable its LAN-access option, pause the VPN temporarily, or use USB pairing.
            """.trimIndent(),
            message,
        )
    }

    @Test
    fun `failure message still explains itself with nothing to list`() {
        val message = pairingFailureMessage(emptyList())
        assertFalse(message.contains("Tried:"))
        assertTrue(message.contains("USB pairing"))
    }

    @Test
    fun `network errors are described in plain language`() {
        assertEquals("timed out", describeNetworkError(SocketTimeoutException("connect timed out")))
        assertEquals("no route to host", describeNetworkError(NoRouteToHostException("No route to host")))
        assertEquals("connection refused", describeNetworkError(ConnectException("Connection refused")))
        assertEquals("unreachable", describeNetworkError(ConnectException("Network is unreachable")))
        assertEquals("IllegalStateException", describeNetworkError(IllegalStateException()))
    }
}
