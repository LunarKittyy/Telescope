package com.telescope

import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import java.net.InetSocketAddress
import java.net.ServerSocket
import java.net.Socket
import java.nio.charset.StandardCharsets
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.atomic.AtomicBoolean

/**
 * Dynamic over a real socket: the server's own sender, socket buffers and link samples, a computer reading at a
 * Wi-Fi-like speed that drops and stalls. Each packet carries when it was made, so the delay is measured end to end.
 */
class DynamicBitrateSocketTest {

    private fun port(server: MjpegServer): Int {
        val field = MjpegServer::class.java.getDeclaredField("serverSocket")
        field.isAccessible = true
        return (field.get(server) as ServerSocket).localPort
    }

    /** Link speed in Mbps at ms since the start. */
    private fun linkMbps(ms: Long): Double = when {
        ms < 6_000 -> 40.0
        ms in 11_000 until 11_700 -> 0.0  // a stall
        ms < 14_000 -> 4.0
        else -> 40.0
    }

    private class Run(val delays: List<LongArray>, val bitrates: List<Int>, val samples: Int) {
        fun p90(fromMs: Long, toMs: Long): Long {
            val d = delays.filter { it[0] in fromMs until toMs }.map { it[1] }.sorted()
            return if (d.isEmpty()) Long.MAX_VALUE else d[(d.size - 1) * 9 / 10]
        }
    }

    /** Streams for [seconds] over the scheduled link: Dynamic, or Auto's fixed bitrate. */
    private fun run(dynamic: Boolean, seconds: Int): Run {
        val auto = H264Stream.defaultBitrate(1920, 1080, 30)
        val controller = DynamicBitrate(auto, H264Stream.dynamicCeiling(1920, 1080, 30))
        val rate = java.util.concurrent.atomic.AtomicInteger(controller.bitrate)
        val keyWanted = AtomicBoolean(true)
        val samples = CopyOnWriteArrayList<DynamicBitrate.Sample>()
        val server = MjpegServer(0, { "{}" }, { "{}" }, "127.0.0.1", tokens = { listOf("t") },
            requestKeyFrame = { keyWanted.set(true) },
            onH264Link = { s -> samples.add(s); if (dynamic) controller.update(s)?.let { rate.set(it) } })
        server.dynamicH264 = dynamic
        server.start()
        val start = System.currentTimeMillis()
        val done = AtomicBoolean(false)
        val delays = CopyOnWriteArrayList<LongArray>()  // (arrived at ms since start, delay ms)
        val bitrates = CopyOnWriteArrayList<Int>()
        try {
            val reader = Thread {
                val socket = Socket()
                socket.receiveBufferSize = 32 * 1024  // the computer's side shouldn't hide seconds of video either
                socket.connect(InetSocketAddress("127.0.0.1", port(server)))
                socket.soTimeout = 5_000
                socket.use {
                    it.getOutputStream().write("GET /v1/video.h264 HTTP/1.1\r\nAuthorization: Bearer t\r\n\r\n"
                        .toByteArray(StandardCharsets.ISO_8859_1))
                    val input = it.getInputStream()
                    val head = StringBuilder()
                    while (!head.endsWith("\r\n\r\n")) head.append(input.read().toChar())
                    val buf = ByteArray(64 * 1024)
                    val pending = java.io.ByteArrayOutputStream()
                    var allowance = 0.0
                    var last = System.currentTimeMillis()
                    while (!done.get()) {
                        val now = System.currentTimeMillis()
                        allowance = minOf(allowance + linkMbps(now - start) * 1_000_000 / 8 * (now - last) / 1000, 64.0 * 1024)
                        last = now
                        if (allowance < 1024) { Thread.sleep(2); continue }
                        val n = try { input.read(buf, 0, allowance.toInt()) } catch (_: java.net.SocketTimeoutException) { continue }
                        if (n < 0) break
                        allowance -= n
                        pending.write(buf, 0, n)
                        // Packets end with the delimiter; each starts with a start code and its time in hex.
                        val bytes = pending.toByteArray()
                        var from = 0
                        while (true) {
                            val end = indexOf(bytes, H264Stream.AUD, from)
                            if (end < 0) break
                            if (end - from >= 21) {
                                val made = String(bytes, from + 5, 16, StandardCharsets.ISO_8859_1).toLong(16)
                                val arrived = System.currentTimeMillis()
                                delays.add(longArrayOf(arrived - start, arrived - made))
                            }
                            from = end + H264Stream.AUD.size
                        }
                        pending.reset()
                        pending.write(bytes, from, bytes.size - from)
                    }
                }
            }.apply { isDaemon = true; start() }

            // The encoder: 30 fps at the controller's bitrate, a keyframe every second or when asked.
            var frame = 0
            val rnd = java.util.Random(5)
            while (System.currentTimeMillis() - start < seconds * 1000L) {
                val key = keyWanted.getAndSet(false) || frame % 30 == 0
                val size = (rate.get() / 8.0 / 30 * 0.9 * (if (key) 5.0 else 25.0 / 29.0) * (0.8 + 0.4 * rnd.nextDouble())).toInt()
                val packet = ByteArray(maxOf(size, 32)) { 0x55 }
                byteArrayOf(0, 0, 0, 1, 0x65).copyInto(packet)
                "%016x".format(System.currentTimeMillis()).toByteArray(StandardCharsets.ISO_8859_1).copyInto(packet, 5)
                server.sendH264(packet, key, config = false)
                if (frame % 30 == 0) bitrates.add(rate.get())
                frame++
                val next = start + frame * 1000L / 30
                val wait = next - System.currentTimeMillis()
                if (wait > 0) Thread.sleep(wait)
            }
            done.set(true)
            reader.join(6_000)
        } finally {
            server.stop()
        }

        return Run(delays, bitrates, samples.size)
    }

    @Test
    fun `dynamic follows a real socket down and through a stall`() {
        val auto = H264Stream.defaultBitrate(1920, 1080, 30)
        val r = run(dynamic = true, seconds = 20)
        println("Dynamic, bitrate each second (Mbps): " + r.bitrates.joinToString(" ") { "%.1f".format(it / 1e6) })
        println("p90 delay: fast ${r.p90(1_000, 6_000)} ms, slow ${r.p90(8_000, 11_000)} ms, after the stall " +
            "${r.p90(12_700, 14_000)} ms, fast again ${r.p90(16_000, 20_000)} ms; ${r.delays.size} frames, ${r.samples} samples")

        assertTrue(r.samples > 60, "link samples every 250 ms")
        assertTrue(r.bitrates[5] > auto, "climbs on the fast link: ${r.bitrates[5]}")
        assertTrue(r.bitrates[10] < 5_000_000, "down with the slow link: ${r.bitrates[10]}")
        // Real time on a shared machine: bounds with room to spare. Auto on the same link lags about 2 s (below), and
        // how soon it climbs back is up to its probing, which DynamicBitrateSimTest covers without a clock.
        assertTrue(r.p90(8_000, 11_000) < 800, "no lag on the slow link: ${r.p90(8_000, 11_000)}")
        assertTrue(r.p90(12_700, 14_000) < 1_000, "caught up after the stall: ${r.p90(12_700, 14_000)}")
        assertTrue(r.p90(16_000, 20_000) < 300, "quick on the fast link: ${r.p90(16_000, 20_000)}")
    }

    @Test
    fun `without dynamic the same slow link lags`() {
        // The control: shows this measures lag at all.
        val r = run(dynamic = false, seconds = 11)
        println("Auto, p90 delay: fast ${r.p90(1_000, 6_000)} ms, slow ${r.p90(8_000, 11_000)} ms")
        assertTrue(r.p90(8_000, 11_000) > 1_000, "lags: ${r.p90(8_000, 11_000)}")
    }

    private fun indexOf(haystack: ByteArray, needle: ByteArray, from: Int): Int {
        outer@ for (i in from..haystack.size - needle.size) {
            for (j in needle.indices) if (haystack[i + j] != needle[j]) continue@outer
            return i
        }
        return -1
    }
}
