package com.telescope

import org.junit.jupiter.api.Assertions.assertArrayEquals
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertSame
import org.junit.jupiter.api.Test
import java.nio.ByteBuffer

class UdpVideoTest {
    private val key = ByteArray(32) { it.toByte() }
    private fun hex(s: String) = s.chunked(2).map { it.toInt(16).toByte() }.toByteArray()

    private fun oldFragment(seq: Int, data: ByteArray, key: Boolean, index: Int): ByteArray {
        val count = maxOf(1, (data.size + UdpWire.MAX_PAYLOAD - 1) / UdpWire.MAX_PAYLOAD)
        val from = index * UdpWire.MAX_PAYLOAD
        val to = minOf(data.size, from + UdpWire.MAX_PAYLOAD)
        return ByteBuffer.allocate(UdpWire.HEADER + to - from).apply {
            putInt(seq); putShort(index.toShort()); putShort(count.toShort()); put((if (key) UdpWire.FLAG_KEY else 0).toByte())
            put(data, from, to - from)
        }.array()
    }

    private class Session(authorized: () -> Boolean = { true }, timeoutMs: Long = 2000L) : java.io.Closeable {
        val desktop = java.net.DatagramSocket(0, java.net.InetAddress.getLoopbackAddress())
        private val listener = java.net.ServerSocket(0, 1, java.net.InetAddress.getLoopbackAddress())
        private val session = java.net.Socket(java.net.InetAddress.getLoopbackAddress(), listener.localPort)
        val client = UdpVideoClient(listener.accept(),
            java.net.InetSocketAddress(java.net.InetAddress.getLoopbackAddress(), desktop.localPort),
            stillAuthorized = authorized, feedbackTimeoutMs = timeoutMs, requestKeyFrame = {})
        private val hello = client.hello()
        private val key = java.util.Base64.getDecoder().decode(Regex("\"key\":\"([^\"]+)\"").find(hello)!!.groupValues[1])
        private val port = Regex("\"port\":(\\d+)").find(hello)!!.groupValues[1].toInt()
        private val out = UdpSeal(key, UdpWire.TO_PHONE)
        val runner = Thread { client.run { null } }.apply { start() }

        fun tell(plain: ByteArray) {
            val sealed = out.seal(plain)
            desktop.send(java.net.DatagramPacket(sealed, sealed.size, java.net.InetAddress.getLoopbackAddress(), port))
        }

        fun hello() {
            tell(byteArrayOf(UdpWire.HELLO))
            val deadline = System.nanoTime() + 1_000_000_000L
            while (!client.hasActiveViewer() && System.nanoTime() < deadline) Thread.sleep(5)
            assertTrue(client.hasActiveViewer())
        }

        override fun close() {
            client.close(); session.close(); desktop.close(); listener.close()
            runner.join(2000)
        }
    }

    @Test
    fun `revoking pairing cuts off an existing udp session`() {
        val authorized = java.util.concurrent.atomic.AtomicBoolean(true)
        Session(authorized::get).use { session ->
            session.hello()
            authorized.set(false)
            session.runner.join(1500)
            assertFalse(session.runner.isAlive)
            assertFalse(session.client.hasActiveViewer())
        }
    }

    @Test
    fun `the desktop's reports reach Dynamic, and an older desktop's silence counts as nothing measured`() {
        fun report(received: Int, highest: Int) = ByteBuffer.allocate(UdpReport.SIZE).put(UdpWire.REPORT)
            .putInt(received).putInt(highest).putInt(0).putInt(0).putInt(-1).putShort(0).putShort(0).array()
        Session().use { session ->
            session.hello()
            assertFalse(session.client.reporting)
            assertNull(session.client.takeLink())
            session.tell(report(0, 0))
            val deadline = System.nanoTime() + 1_000_000_000L
            while (!session.client.reporting && System.nanoTime() < deadline) Thread.sleep(5)
            assertTrue(session.client.reporting)
            assertNull(session.client.takeLink())  // the first one only starts the count
            session.tell(report(90, 100))
            var take: UdpLink.Take? = null
            while (take == null && System.nanoTime() < deadline + 1_000_000_000L) { Thread.sleep(5); take = session.client.takeLink() }
            assertEquals(0.1, take!!.loss, 1e-9)
        }
    }

    @Test
    fun `pacing spreads bytes at the rate, catches up an oversleep, and banks no more than a burst`() {
        val pacer = UdpPacer(burst = 10_000)  // 8 Mbps below: 1000 bytes a millisecond
        assertEquals(0L, pacer.waitNs(10_000, 8_000_000, 0))  // the burst goes at once
        assertEquals(0L, pacer.waitNs(1_000, 8_000_000, 0))  // under a millisecond owed: no sleep yet
        assertEquals(1_000_000L, pacer.waitNs(1_000, 8_000_000, 0))
        // It asked for 1 ms and the phone slept 40: the time it's behind goes out now, not lost to the cap
        pacer.woke(40_000_000)
        repeat(39) { assertEquals(0L, pacer.waitNs(1_000, 8_000_000, 40_000_000)) }
        assertTrue(pacer.waitNs(1_000, 8_000_000, 40_000_000) > 0)
        assertEquals(0L, pacer.waitNs(5_000, 0, 41_000_000))  // off
        assertEquals(0L, pacer.waitNs(10_000, 8_000_000, 600_000_000_000L))  // on again ten minutes later: one burst
        assertEquals(0L, pacer.waitNs(1_000, 8_000_000, 600_000_000_000L))
        assertEquals(1_000_000L, pacer.waitNs(1_000, 8_000_000, 600_000_000_000L))  // not ten minutes' worth
        val steady = UdpPacer(burst = 10_000)
        steady.waitNs(10_000, 100_000_000, 0)
        assertEquals(0L, steady.waitNs(1_000, 100_000_000, 900_000_000_000L))  // minutes at one rate: no overflow
    }

    @Test
    fun `a frame whose last datagram overslept doesn't bank the idle after it`() {
        val pacer = UdpPacer(burst = 10_000)  // 8 Mbps: 1000 bytes a millisecond
        pacer.waitNs(10_000, 8_000_000, 0)
        pacer.waitNs(1_000, 8_000_000, 0)
        assertEquals(1_000_000L, pacer.waitNs(1_000, 8_000_000, 0))  // the frame's last datagram sleeps
        pacer.woke(5_000_000)  // 4 ms late
        // The next frame, a big keyframe, comes 33 ms later: a burst goes at once and the rest is paced
        var atOnce = 0
        while (pacer.waitNs(1_000, 8_000_000, 33_000_000) <= 0) atOnce++
        assertTrue(atOnce * 1_000 <= 10_000 + 1_000, "$atOnce datagrams at once")
    }

    @Test
    fun `pacing follows Dynamic's bitrate as well as what the encoder made`() {
        assertEquals(UdpVideoClient.PACE_MIN_BPS, UdpVideoClient.paceFor(100_000.0))
        assertEquals((6_000_000 * UdpVideoClient.PACE).toInt(), UdpVideoClient.paceFor(500_000.0, 6_000_000))
        assertEquals((8_000_000 * UdpVideoClient.PACE).toInt(), UdpVideoClient.paceFor(8_000_000.0, 6_000_000))
    }

    @Test
    fun `once the link's speed is known pacing stays a little over it`() {
        // 3 Mbps video on a 4 Mbps link: 2.5 times would overflow a short router buffer with each keyframe
        assertEquals((4_000_000 * UdpVideoClient.PACE_OVER_LINK).toInt(), UdpVideoClient.paceFor(2_500_000.0, 3_000_000, 4_000_000.0))
        // never slower than a little over the video, nor faster than without it
        assertEquals((3_000_000 * UdpVideoClient.PACE_FLOOR).toInt(), UdpVideoClient.paceFor(2_500_000.0, 3_000_000, 1_000_000.0))
        assertEquals((3_000_000 * UdpVideoClient.PACE).toInt(), UdpVideoClient.paceFor(2_500_000.0, 3_000_000, 50_000_000.0))
    }

    @Test
    fun `a desktop that sends no feedback loses its session`() {
        Session(timeoutMs = 100L).use { session ->
            session.runner.join(1000)
            assertFalse(session.runner.isAlive)
        }
    }

    @Test
    fun `udp viewers count as watching and close with the h264 encoder`() {
        Session().use { session ->
            session.hello()
            val server = MjpegServer(0, { "{}" }, { "{}" }, tokens = { listOf("secret") })
            val field = MjpegServer::class.java.getDeclaredField("udpClients").apply { isAccessible = true }
            @Suppress("UNCHECKED_CAST")
            (field.get(server) as MutableList<UdpVideoClient>).add(session.client)
            assertTrue(server.hasActiveViewer())
            server.closeH264Clients()
            assertFalse(server.hasActiveViewer())
            session.runner.join(1500)
            assertFalse(session.runner.isAlive)
        }
    }

    @Test
    fun `seals exactly like the desktop expects`() {
        // The same vector is in the desktop's test_udp_reader.py
        val seal = UdpSeal(key, UdpWire.TO_DESKTOP)
        repeat(5) { seal.seal(ByteArray(0)) }
        assertArrayEquals(hex("00000000000000055e76b1e3a4fb9044e263c4c95177c93d06d25e7b83374ba48c"), seal.seal("telescope".toByteArray()))
    }

    @Test
    fun `sealing into a reused buffer gives the same datagram as sealing to a new array`() {
        val a = UdpSeal(key, UdpWire.TO_DESKTOP)
        val b = UdpSeal(key, UdpWire.TO_DESKTOP)
        val buf = ByteArray(2048) { 7 }
        val plain = ByteArray(UdpWire.HEADER + UdpWire.MAX_PAYLOAD) { it.toByte() }
        for (size in listOf(plain.size, 810, 10, 0)) {
            val length = a.sealInto(plain, buf, size)
            assertEquals(8 + size + UdpSeal.TAG, length)
            assertArrayEquals(b.seal(plain.copyOf(size)), buf.copyOf(length))
        }
        assertEquals(-1, a.sealInto(ByteArray(100), ByteArray(50)))
        val length = a.sealInto(plain, buf)
        assertArrayEquals(b.seal(plain), buf.copyOf(length))
    }

    @Test
    fun `opens what the other side sealed, once, and nothing forged`() {
        val desktop = UdpSeal(key, UdpWire.TO_PHONE)
        val phone = UdpSeal(key, UdpWire.TO_PHONE)
        val a = desktop.seal(byteArrayOf(UdpWire.HELLO))
        val b = desktop.seal(byteArrayOf(UdpWire.KEYFRAME))
        assertArrayEquals(byteArrayOf(UdpWire.KEYFRAME), phone.open(b))
        assertArrayEquals(byteArrayOf(UdpWire.HELLO), phone.open(a))  // late but new: fine
        assertNull(phone.open(a))  // replayed
        val forged = b.copyOf().also { it[it.size - 1] = (it[it.size - 1] + 1).toByte() }
        assertNull(phone.open(forged))
        assertNull(UdpSeal(ByteArray(32), UdpWire.TO_PHONE).open(desktop.seal(byteArrayOf(1))))  // another key
        assertNull(UdpSeal(key, UdpWire.TO_DESKTOP).open(desktop.seal(byteArrayOf(1))))  // the other direction
    }

    @Test
    fun `far too old is refused even if never seen`() {
        val desktop = UdpSeal(key, UdpWire.TO_PHONE)
        val phone = UdpSeal(key, UdpWire.TO_PHONE)
        val old = desktop.seal(byteArrayOf(1))
        repeat(UdpSeal.WINDOW + 1) { phone.open(desktop.seal(byteArrayOf(2))) }
        assertNull(phone.open(old))
    }

    @Test
    fun `fragments match the old header and payload bytes`() {
        val buf = ByteArray(UdpWire.HEADER + UdpWire.MAX_PAYLOAD) { 7 }
        val sizes = listOf(0, 1, 1100, 1101, 3000, 300_000)
        for (key in listOf(false, true)) {
            for (size in sizes) {
                val data = ByteArray(size) { (it % 251).toByte() }
                val frame = UdpFrame(0x89ABCDEF.toInt(), data, key)
                assertEquals(maxOf(1, (size + 1099) / 1100), frame.count)
                val back = java.io.ByteArrayOutputStream()
                for (index in 0 until frame.count) {
                    val length = UdpWire.writeFragment(frame, index, buf)
                    assertEquals(UdpWire.fragmentSize(frame, index), length)
                    assertArrayEquals(oldFragment(frame.seq, data, key, index), buf.copyOf(length))
                    back.write(buf, UdpWire.HEADER, length - UdpWire.HEADER)
                }
                assertArrayEquals(data, back.toByteArray())
            }
        }
    }

    @Test
    fun `live frames and repairs use the same buffer and send only their fragment bytes`() {
        val frame = UdpFrame(7, ByteArray(3000) { (it % 251).toByte() }, key = true)
        val parts = mutableListOf<ByteArray>()
        var buffer: ByteArray? = null
        val transmitter = UdpTransmitter(UdpLink(), { 0L }, {}, { plain, length ->
            buffer?.let { assertSame(it, plain) }
            buffer = plain
            parts.add(plain.copyOf(length))
        })
        transmitter.frame(frame, 0, 0)
        transmitter.repair(frame, 2)
        val empty = UdpFrame(8, ByteArray(0), key = false)
        transmitter.frame(empty, 0, 0)
        assertEquals(5, parts.size)
        repeat(frame.count) { assertArrayEquals(oldFragment(frame.seq, frame.data, frame.key, it), parts[it]) }
        assertArrayEquals(parts[2], parts[3])
        assertArrayEquals(oldFragment(empty.seq, empty.data, empty.key, 0), parts[4])
    }

    @Test
    fun `the desktop's udp port comes from the query`() {
        assertEquals(5000, MjpegServer.udpPort("port=5000"))
        assertEquals(5000, MjpegServer.udpPort("x=1&port=5000"))
        assertNull(MjpegServer.udpPort(""))
        assertNull(MjpegServer.udpPort("port=0"))
        assertNull(MjpegServer.udpPort("port=70000"))
        assertNull(MjpegServer.udpPort("port=abc"))
    }

    @Test
    fun `sends frames to the desktop, sends lost pieces again, and stops when the session ends`() {
        val desktop = java.net.DatagramSocket(0, java.net.InetAddress.getLoopbackAddress()).apply { soTimeout = 2000 }
        val server = java.net.ServerSocket(0, 1, java.net.InetAddress.getLoopbackAddress())
        val session = java.net.Socket(java.net.InetAddress.getLoopbackAddress(), server.localPort)
        val phoneSide = server.accept()
        val keyAsks = java.util.concurrent.atomic.AtomicInteger()
        val client = UdpVideoClient(phoneSide, java.net.InetSocketAddress(java.net.InetAddress.getLoopbackAddress(), desktop.localPort)) { keyAsks.incrementAndGet() }
        val hello = client.hello()
        val key = java.util.Base64.getDecoder().decode(Regex("\"key\":\"([^\"]+)\"").find(hello)!!.groupValues[1])
        val phonePort = Regex("\"port\":(\\d+)").find(hello)!!.groupValues[1].toInt()
        val runner = Thread { client.run { byteArrayOf(9, 9) } }.apply { start() }
        val inbound = UdpSeal(key, UdpWire.TO_DESKTOP)
        val outbound = UdpSeal(key, UdpWire.TO_PHONE)
        fun receive(): ByteBuffer {
            val buf = ByteArray(2048)
            val p = java.net.DatagramPacket(buf, buf.size)
            desktop.receive(p)
            return ByteBuffer.wrap(inbound.open(buf, p.length)!!)
        }
        fun tell(plain: ByteArray) {
            val sealed = outbound.seal(plain)
            desktop.send(java.net.DatagramPacket(sealed, sealed.size, java.net.InetAddress.getLoopbackAddress(), phonePort))
        }

        val packet = ByteArray(3000) { 7 } + H264Stream.AUD
        val data = byteArrayOf(9, 9) + packet
        client.offer(ByteArray(10) + H264Stream.AUD, key = false)  // nothing goes before a keyframe
        client.offer(packet, key = true)
        val first = receive()
        assertArrayEquals(oldFragment(0, data, true, 0), first.array())
        assertEquals(0, first.int)  // frame 0
        val count = run { first.short; first.short.toInt() }
        assertEquals(3, count)
        repeat(count - 1) { assertArrayEquals(oldFragment(0, data, true, it + 1), receive().array()) }

        tell(ByteBuffer.allocate(7).put(UdpWire.NACK).putInt(0).putShort(1).array())
        val again = receive()
        assertArrayEquals(oldFragment(0, data, true, 1), again.array())
        assertEquals(0, again.int); assertEquals(1, again.short.toInt())

        tell(ByteBuffer.allocate(7).put(UdpWire.NACK).putInt(0).putShort(2).array())
        assertArrayEquals(oldFragment(0, data, true, 2), receive().array())
        tell(ByteBuffer.allocate(5).put(UdpWire.NACK).putInt(0).array())
        repeat(count) { assertArrayEquals(oldFragment(0, data, true, it), receive().array()) }

        val next = byteArrayOf(1) + H264Stream.AUD
        client.offer(next, key = false)
        assertArrayEquals(oldFragment(1, next, false, 0), receive().array())

        tell(byteArrayOf(UdpWire.KEYFRAME))
        val deadline = System.currentTimeMillis() + 2000
        while (keyAsks.get() == 0 && System.currentTimeMillis() < deadline) Thread.sleep(10)
        assertEquals(1, keyAsks.get())

        session.close()  // the desktop went away
        runner.join(5000)
        assertEquals(false, runner.isAlive)
        desktop.close(); server.close()
    }
}
