package com.telescope

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

class PresenceTest {
    private class Clock(var now: Long = 0) { fun read() = now }
    private val computer = PairedComputer("pc-1", "Desktop", "token", 0)
    private val address = "192.168.1.10"
    private val challenge = "00000000000000000000000000000001"

    @Test
    fun `schedule catches up on wake and slows after interaction without reset by replies`() {
        val clock = Clock()
        val schedule = PresenceSchedule(clock::read) { 0 }
        assertTrue(schedule.due())
        assertFalse(schedule.due())
        clock.now = 1_999
        assertFalse(schedule.due())
        clock.now = 2_000
        assertTrue(schedule.due())
        clock.now = 10_000
        assertTrue(schedule.due())
        clock.now = 14_999
        assertFalse(schedule.due())
        clock.now = 180_000
        assertTrue(schedule.due())
        clock.now = 224_999
        assertFalse(schedule.due())
        clock.now = 900_000
        assertTrue(schedule.due())
        assertFalse(schedule.due())
        schedule.hint()
        assertTrue(schedule.due())
        clock.now += 2_000
        assertTrue(schedule.due())
    }

    @Test
    fun `rapid hints coalesce without sending more often than once a second`() {
        val clock = Clock()
        val schedule = PresenceSchedule(clock::read) { 0 }
        assertTrue(schedule.due())
        for (time in 100L..900L step 100L) {
            clock.now = time
            schedule.hint()
            assertFalse(schedule.due())
        }
        clock.now = 1_000
        schedule.hint()
        assertTrue(schedule.due())
        clock.now = 1_100
        schedule.hint()
        assertFalse(schedule.due())
        schedule.hint(waiting = true)
        assertTrue(schedule.due())
        clock.now = 1_200
        schedule.hint(waiting = true)
        assertFalse(schedule.due())
    }

    @Test
    fun `fast timers jitter within ten percent and Wait remains sixty seconds`() {
        for ((age, interval) in listOf(0L to 2_000L, 10_000L to 5_000L)) {
            for (sign in listOf(-1L, 1L)) {
                val clock = Clock()
                val schedule = PresenceSchedule(clock::read) { it * sign }
                clock.now = age
                assertTrue(schedule.due())
                clock.now += interval + interval / 10 * sign - 1
                assertFalse(schedule.due())
                clock.now++
                assertTrue(schedule.due())
            }
        }
        val clock = Clock()
        val schedule = PresenceSchedule(clock::read) { 100_000 }
        schedule.hint(waiting = true)
        assertTrue(schedule.due())
        clock.now = 59_999
        assertFalse(schedule.due())
        clock.now++
        assertTrue(schedule.due())
    }

    @Test
    fun `Wait sends initial exchange then uses a minute while still receiving requests`() {
        val clock = Clock()
        val schedule = PresenceSchedule(clock::read) { 0 }
        schedule.hint(waiting = true)
        assertTrue(schedule.due())
        clock.now = 59_999
        assertFalse(schedule.due())
        clock.now = 60_000
        assertTrue(schedule.due())
    }

    @Test
    fun `wait deadline follows the minute timer and interaction hints`() {
        val clock = Clock()
        val schedule = PresenceSchedule(clock::read) { 0 }
        schedule.hint(waiting = true)
        assertEquals(0L, schedule.delay())
        assertTrue(schedule.due())
        assertEquals(60_000L, schedule.delay())
        clock.now = 59_999
        assertEquals(1L, schedule.delay())
        schedule.hint(waiting = false)
        assertEquals(0L, schedule.delay())
        assertTrue(schedule.due())
        assertEquals(2_000L, schedule.delay())
        clock.now = 900_000
        assertEquals(0L, schedule.delay())
    }

    @Test
    fun `idle jitter is bounded between thirty and sixty seconds`() {
        for (jitter in listOf(-100_000L, 100_000L)) {
            val clock = Clock()
            val schedule = PresenceSchedule(clock::read) { jitter }
            schedule.due()
            clock.now = 180_000
            assertTrue(schedule.due())
            clock.now += if (jitter < 0) 29_999 else 59_999
            assertFalse(schedule.due())
            clock.now++
            assertTrue(schedule.due())
        }
    }

    @Test
    fun `HMAC shared vector matches Rust and does not disclose the pairing token`() {
        val packet = PresencePacket("q", "phone-1", "pc-1", challenge, "5.1.4", listOf("192.168.1.2"))
        val bytes = packet.encode("token")!!
        assertEquals("aaf2cc3e64c8e35f0fdc93132eb06cdfa0d83b65039411d3a94a200245ba8d35", bytes.toString(Charsets.UTF_8).substringAfterLast('\n'))
        assertFalse(bytes.toString(Charsets.UTF_8).contains("token"))
        assertEquals(packet, PresencePacket.decode(bytes, "token"))
        assertNull(PresencePacket.decode(bytes, "wrong"))
        assertNull(PresencePacket.decode(bytes.toString(Charsets.UTF_8).replace("5.1.4", "5.1.5").toByteArray(), "token"))
    }

    @Test
    fun `bounded packets reject malformed identity challenges addresses and extra data`() {
        val good = PresencePacket("q", "phone-1", "pc-1", challenge, "5.1.4", emptyList())
        assertNull(good.copy(sender = "bad\nidentity").encode("token"))
        assertNull(good.copy(challenge = "0").encode("token"))
        assertNull(good.copy(addresses = listOf("127.0.0.1")).encode("token"))
        assertNull(good.copy(addresses = List(5) { address }).encode("token"))
        assertNull(PresencePacket.decode(ByteArray(513), "token"))
        assertNull(PresencePacket.decode(good.encode("token")!! + '\n'.code.toByte(), "token"))
    }

    @Test
    fun `fake transport accepts only fresh answers from the challenged address once`() {
        val clock = Clock()
        val exchange = PresenceExchange("phone-1", "5.1.4", clock::read) { challenge }
        val sent = mutableListOf<PresenceSend>()
        sent += exchange.request(computer, address, emptyList())!!
        assertEquals(address, sent.single().address)
        val query = PresencePacket.decode(sent.single().bytes, computer.token)!!
        val answer = PresencePacket("a", computer.id, "phone-1", query.challenge, "5.1.5", listOf("192.168.1.12")).encode(computer.token)!!
        assertNull(exchange.receive(answer, "192.168.1.11", listOf(computer), emptyList()).second)
        assertNull(exchange.receive(answer, address, listOf(computer), emptyList(), port = 10_000).second)
        val contact = exchange.receive(answer, address, listOf(computer), emptyList()).second!!
        assertEquals("5.1.5", contact.version)
        assertEquals(listOf("192.168.1.12"), contact.addresses)
        assertNull(exchange.receive(answer, address, listOf(computer), emptyList()).second)
        exchange.request(computer, address, emptyList())
        clock.now = 10_000
        assertNull(exchange.receive(answer, address, listOf(computer), emptyList()).second)
    }

    @Test
    fun `unsolicited authenticated request gets a bounded answer and challenge but no address update`() {
        val clock = Clock()
        val exchange = PresenceExchange("phone-1", "5.1.4", clock::read) { challenge }
        val request = PresencePacket("q", computer.id, "phone-1", "00000000000000000000000000000002", "5.1.4", listOf(address)).encode(computer.token)!!
        val (sends, contact) = exchange.receive(request, address, listOf(computer), emptyList())
        assertNull(contact)
        assertEquals(listOf("a", "q"), sends.map { PresencePacket.decode(it.bytes, computer.token)!!.type })
        assertTrue(exchange.receive(request, address, listOf(computer), emptyList()).first.isEmpty())
        clock.now = 500
        assertTrue(exchange.receive(request, address, listOf(computer), emptyList()).first.isEmpty())
    }

    @Test
    fun `a just confirmed source earns an answer without another return challenge`() {
        val clock = Clock()
        val exchange = PresenceExchange("phone-1", "5.1.4", clock::read) { challenge }
        exchange.request(computer, address, emptyList())
        val answer = PresencePacket("a", computer.id, "phone-1", challenge, "5.1.4", emptyList()).encode(computer.token)!!
        exchange.receive(answer, address, listOf(computer), emptyList())
        val request = PresencePacket("q", computer.id, "phone-1", "00000000000000000000000000000002", "5.1.4", emptyList()).encode(computer.token)!!
        val sends = exchange.receive(request, address, listOf(computer), emptyList()).first
        assertEquals(listOf("a"), sends.map { PresencePacket.decode(it.bytes, computer.token)!!.type })
    }

    @Test
    fun `revocation and replacement token invalidate outstanding exchanges`() {
        val clock = Clock()
        val exchange = PresenceExchange("phone-1", "5.1.4", clock::read) { challenge }
        exchange.request(computer, address, emptyList())
        val answer = PresencePacket("a", computer.id, "phone-1", challenge, "5.1.4", emptyList()).encode(computer.token)!!
        assertNull(exchange.receive(answer, address, emptyList(), emptyList()).second)
        assertNull(exchange.receive(answer, address, listOf(computer.copy(token = "new-token")), emptyList()).second)
    }

    @Test
    fun `version banner distinguishes no authenticated contact from legacy desktop`() {
        assertNull(desktopVersionNotice(null, "5.1.4"))
        assertEquals(DesktopVersionNotice.Legacy, desktopVersionNotice("", "5.1.4"))
        assertNull(desktopVersionNotice("5.1.4", "5.1.4"))
        assertEquals(DesktopVersionNotice.Different, desktopVersionNotice("5.1.3", "5.1.4"))
        assertEquals(DesktopVersionNotice.Different, desktopVersionNotice("5.1.4 nightly 5", "5.1.4"))
        assertNull(desktopVersionNotice("5.1.4 nightly 5", "5.1.4-nightly.5"))
        assertNull(desktopVersionNotice("5.1.4 dev", "5.1.4"))
        assertNull(desktopVersionNotice("5.1.4", "5.1.4-dev.1"))
    }

    @Test
    fun `old pairing storage migrates without inventing version or presence support`() {
        val list = PairedComputerList.decode("""[{"id":"pc-1","name":"Desktop","token":"token","pairedAtMs":0}]""")
        assertEquals(computer, list.computers.single())
        assertNull(list.computers.single().desktopVersion)
        assertNull(list.computers.single().presence)
        assertTrue(list.computers.single().addresses.isEmpty())
    }

    @Test
    fun `address migration excludes USB loopback preserves adapter kinds and stays bounded`() {
        val previous = listOf(PairingCandidate("100.64.1.2", "tailscale0", PairingKind.TAILSCALE))
        val result = mergeComputerAddresses(listOf("127.0.0.1", "192.168.1.2", "100.64.1.2", "192.168.1.3", "192.168.1.4", "192.168.1.5"), previous)
        assertEquals(4, result.size)
        assertEquals(PairingKind.TAILSCALE, result[1].kind)
        assertFalse(result.any { it.ip == "127.0.0.1" })
    }
    @Test
    fun `bounded lists retain Tailscale and reject unroutable address hints`() {
        assertEquals(listOf("192.168.1.1", "192.168.2.1", "192.168.3.1", "100.64.1.1"), boundedPresenceAddresses(listOf("192.168.1.1", "192.168.2.1", "192.168.3.1", "192.168.4.1", "100.64.1.1")))
        assertTrue(boundedPresenceAddresses(listOf("127.0.0.1", "169.254.1.1", "0.0.0.0", "224.0.0.1")).isEmpty())
    }

    @Test
    fun `many peers keep receiving prompt answers without exhausting another peer replay cache`() {
        val clock = Clock()
        var issued = 0
        val exchange = PresenceExchange("phone-1", "5.1.4", clock::read) { (++issued).toString(16).padStart(32, '0') }
        val peers = (1..64).map { computer.copy(id = "pc-$it", token = "token-$it") }
        repeat(10) { round ->
            clock.now = round * 1_000L
            for ((index, peer) in peers.withIndex()) {
                val nonce = (round * 64 + index + 1).toString(16).padStart(32, '0')
                val request = PresencePacket("q", peer.id, "phone-1", nonce, "5.1.4", emptyList()).encode(peer.token)!!
                val sends = exchange.receive(request, address, peers, emptyList()).first
                assertEquals("a", PresencePacket.decode(sends.first().bytes, peer.token)!!.type)
            }
        }
    }

}
