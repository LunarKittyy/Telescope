package com.telescope

import org.junit.jupiter.api.Assertions.assertArrayEquals
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import java.io.ByteArrayOutputStream
import java.net.ServerSocket
import java.net.Socket
import java.nio.charset.StandardCharsets
import java.util.concurrent.atomic.AtomicReference

class MjpegServerTest {

    private data class Response(val status: Int, val headers: String, val body: ByteArray)

    private fun actualPort(server: MjpegServer): Int {
        val field = MjpegServer::class.java.getDeclaredField("serverSocket")
        field.isAccessible = true
        return (field.get(server) as ServerSocket).localPort
    }

    private fun request(port: Int, raw: String): Response {
        Socket("127.0.0.1", port).use { socket ->
            socket.soTimeout = 2_000
            socket.getOutputStream().apply {
                write(raw.toByteArray(StandardCharsets.ISO_8859_1))
                flush()
            }
            val bytes = socket.getInputStream().readBytes()
            val marker = "\r\n\r\n".toByteArray(StandardCharsets.ISO_8859_1)
            val split = bytes.indexOfSubArray(marker)
            val headers = bytes.copyOfRange(0, split).toString(StandardCharsets.ISO_8859_1)
            val body = bytes.copyOfRange(split + marker.size, bytes.size)
            val status = headers.lineSequence().first().split(" ")[1].toInt()
            return Response(status, headers, body)
        }
    }

    private fun ByteArray.indexOfSubArray(needle: ByteArray): Int {
        for (i in 0..size - needle.size) {
            if (needle.indices.all { j -> this[i + j] == needle[j] }) return i
        }
        error("HTTP header terminator not found")
    }

    private fun authGet(port: Int, path: String, token: String?): Response {
        val authLine = if (token != null) "Authorization: Bearer $token\r\n" else ""
        return request(port, "GET $path HTTP/1.1\r\n$authLine\r\n")
    }

    private fun authPost(port: Int, path: String, token: String?, jsonBody: String): Response {
        val bodyBytes = jsonBody.toByteArray(StandardCharsets.UTF_8)
        val authLine = if (token != null) "Authorization: Bearer $token\r\n" else ""
        val raw = "POST $path HTTP/1.1\r\n" +
            authLine +
            "Content-Type: application/json\r\n" +
            "Content-Length: ${bodyBytes.size}\r\n\r\n" +
            jsonBody
        return request(port, raw)
    }

    @Test
    fun `v1 state endpoint returns UTF-8 JSON with length and no CORS header when authorized`() {
        val body = "{\"camera\":\"télé\"}"
        val server = MjpegServer(0, { body }, { "{}" }, "127.0.0.1", tokens = { listOf("secret-token") })
        server.start()
        try {
            val response = authGet(actualPort(server), "/v1/state", "secret-token")
            assertEquals(200, response.status)
            assertEquals(body, response.body.toString(StandardCharsets.UTF_8))
            assertTrue(response.headers.contains("Content-Type: application/json"))
            assertTrue(response.headers.contains("Content-Length: ${body.toByteArray().size}"))
            assertTrue(!response.headers.contains("Access-Control-Allow-Origin"))
        } finally {
            server.stop()
        }
    }

    @Test
    fun `v1 state endpoint rejects missing authorization header`() {
        val server = MjpegServer(0, { "{}" }, { "{}" }, "127.0.0.1", tokens = { listOf("secret-token") })
        server.start()
        try {
            assertEquals(401, authGet(actualPort(server), "/v1/state", null).status)
        } finally {
            server.stop()
        }
    }

    @Test
    fun `v1 state endpoint rejects wrong token`() {
        val server = MjpegServer(0, { "{}" }, { "{}" }, "127.0.0.1", tokens = { listOf("secret-token") })
        server.start()
        try {
            assertEquals(401, authGet(actualPort(server), "/v1/state", "wrong-token").status)
        } finally {
            server.stop()
        }
    }

    @Test
    fun `v1 state endpoint rejects everything when no token is paired yet`() {
        val server = MjpegServer(0, { "{}" }, { "{}" }, "127.0.0.1", tokens = { emptyList() })
        server.start()
        try {
            assertEquals(401, authGet(actualPort(server), "/v1/state", "anything").status)
        } finally {
            server.stop()
        }
    }

    @Test
    fun `v1 control endpoint decodes JSON body and invokes handler`() {
        val received = AtomicReference<Map<String, String>>()
        val server = MjpegServer(
            0,
            { "{}" },
            { params -> received.set(params); "{\"ok\":true}" },
            "127.0.0.1",
            tokens = { listOf("secret-token") },
        )
        server.start()
        try {
            val response = authPost(
                actualPort(server), "/v1/control", "secret-token",
                "{\"action\":\"camera\",\"id\":\"wide angle\"}",
            )
            assertEquals(200, response.status)
            assertEquals(mapOf("action" to "camera", "id" to "wide angle"), received.get())
        } finally {
            server.stop()
        }
    }

    @Test
    fun `v1 control endpoint rejects malformed JSON body`() {
        var called = false
        val server = MjpegServer(0, { "{}" }, { called = true; "{}" }, "127.0.0.1", tokens = { listOf("secret-token") })
        server.start()
        try {
            val response = authPost(actualPort(server), "/v1/control", "secret-token", "not json")
            assertEquals(400, response.status)
            assertTrue(!called)
        } finally {
            server.stop()
        }
    }

    @Test
    fun `v1 control endpoint requires POST method`() {
        val server = MjpegServer(0, { "{}" }, { "{}" }, "127.0.0.1", tokens = { listOf("secret-token") })
        server.start()
        try {
            assertEquals(405, authGet(actualPort(server), "/v1/control", "secret-token").status)
        } finally {
            server.stop()
        }
    }

    @Test
    fun `v1 control endpoint rejects non-JSON content type`() {
        var called = false
        val server = MjpegServer(0, { "{}" }, { called = true; "{}" }, "127.0.0.1", tokens = { listOf("secret-token") })
        server.start()
        try {
            val raw = "POST /v1/control HTTP/1.1\r\nAuthorization: Bearer secret-token\r\n" +
                "Content-Type: text/plain\r\nContent-Length: 2\r\n\r\n{}"
            assertEquals(400, request(actualPort(server), raw).status)
            assertTrue(!called)
        } finally {
            server.stop()
        }
    }

    @Test
    fun `v1 state and v1 video endpoints return 405 for the wrong method`() {
        val server = MjpegServer(0, { "{}" }, { "{}" }, "127.0.0.1", tokens = { listOf("secret-token") })
        server.start()
        try {
            val postState = "POST /v1/state HTTP/1.1\r\nAuthorization: Bearer secret-token\r\n" +
                "Content-Type: application/json\r\nContent-Length: 0\r\n\r\n"
            assertEquals(405, request(actualPort(server), postState).status)

            val postVideo = "POST /v1/video HTTP/1.1\r\nAuthorization: Bearer secret-token\r\n\r\n"
            assertEquals(405, request(actualPort(server), postVideo).status)
        } finally {
            server.stop()
        }
    }

    @Test
    fun `v1 control endpoint rejects missing content-length`() {
        val server = MjpegServer(0, { "{}" }, { "{}" }, "127.0.0.1", tokens = { listOf("secret-token") })
        server.start()
        try {
            val raw = "POST /v1/control HTTP/1.1\r\nAuthorization: Bearer secret-token\r\n\r\n{}"
            assertEquals(400, request(actualPort(server), raw).status)
        } finally {
            server.stop()
        }
    }

    @Test
    fun `v1 control endpoint rejects oversized body`() {
        val server = MjpegServer(0, { "{}" }, { "{}" }, "127.0.0.1", tokens = { listOf("secret-token") })
        server.start()
        try {
            val raw = "POST /v1/control HTTP/1.1\r\nAuthorization: Bearer secret-token\r\n" +
                "Content-Type: application/json\r\nContent-Length: 4097\r\n\r\n"
            assertEquals(413, request(actualPort(server), raw).status)
        } finally {
            server.stop()
        }
    }

    @Test
    fun `legacy unversioned routes are gone`() {
        val server = MjpegServer(0, { "{}" }, { "{}" }, "127.0.0.1", tokens = { listOf("secret-token") })
        server.start()
        try {
            assertEquals(404, authGet(actualPort(server), "/cameras", "secret-token").status)
            assertEquals(404, authGet(actualPort(server), "/video", "secret-token").status)
            assertEquals(404, authGet(actualPort(server), "/control", "secret-token").status)
        } finally {
            server.stop()
        }
    }

    @Test
    fun `unknown paths and unsupported methods return errors`() {
        val server = MjpegServer(0, { "{}" }, { "{}" }, "127.0.0.1", tokens = { listOf("secret-token") })
        server.start()
        try {
            assertEquals(404, authGet(actualPort(server), "/missing", "secret-token").status)
            assertEquals(
                400,
                request(actualPort(server), "PUT /v1/state HTTP/1.1\r\n\r\n").status,
            )
        } finally {
            server.stop()
        }
    }

    @Test
    fun `oversized request headers return 431`() {
        val server = MjpegServer(0, { "{}" }, { "{}" }, "127.0.0.1", tokens = { listOf("secret-token") })
        server.start()
        try {
            val raw = "GET /v1/state HTTP/1.1\r\nX-Fill: ${"x".repeat(17 * 1024)}\r\n\r\n"
            assertEquals(431, request(actualPort(server), raw).status)
        } finally {
            server.stop()
        }
    }

    @Test
    fun `video endpoint requires authorization`() {
        val server = MjpegServer(0, { "{}" }, { "{}" }, "127.0.0.1", tokens = { listOf("secret-token") })
        server.start()
        try {
            assertEquals(401, authGet(actualPort(server), "/v1/video", null).status)
        } finally {
            server.stop()
        }
    }

    @Test
    fun `video endpoint streams a queued JPEG frame when authorized`() {
        val server = MjpegServer(0, { "{}" }, { "{}" }, "127.0.0.1", tokens = { listOf("secret-token") })
        server.start()
        try {
            Socket("127.0.0.1", actualPort(server)).use { socket ->
                socket.soTimeout = 2_000
                socket.getOutputStream().apply {
                    write("GET /v1/video HTTP/1.1\r\nAuthorization: Bearer secret-token\r\n\r\n".toByteArray())
                    flush()
                }
                val input = socket.getInputStream()
                val header = readUntil(input, "\r\n\r\n".toByteArray())
                assertTrue(header.toString(StandardCharsets.ISO_8859_1)
                    .contains("multipart/x-mixed-replace"))

                val jpeg = byteArrayOf(1, 2, 3, 4)
                server.sendFrame(jpeg)
                val partHeader = readUntil(input, "\r\n\r\n".toByteArray())
                assertTrue(partHeader.toString(StandardCharsets.ISO_8859_1)
                    .contains("Content-Length: ${jpeg.size}"))
                val received = input.readNBytes(jpeg.size)
                assertArrayEquals(jpeg, received)
            }
        } finally {
            server.stop()
        }
    }

    @Test
    fun `closing the MJPEG viewers ends their stream and frees their slot`() {
        val server = MjpegServer(0, { "{}" }, { "{}" }, "127.0.0.1", tokens = { listOf("secret-token") })
        server.start()
        try {
            Socket("127.0.0.1", actualPort(server)).use { socket ->
                socket.soTimeout = 2_000
                socket.getOutputStream().apply {
                    write("GET /v1/video HTTP/1.1\r\nAuthorization: Bearer secret-token\r\n\r\n".toByteArray())
                    flush()
                }
                val input = socket.getInputStream()
                readUntil(input, "\r\n\r\n".toByteArray())
                assertTrue(server.hasActiveViewer())
                server.closeMjpegClients()
                // The viewer sees the end of the stream instead of waiting on frames that never come.
                assertEquals(-1, input.read())
                assertFalse(server.hasActiveViewer())
            }
        } finally {
            server.stop()
        }
    }

    @Test
    fun `a viewer with nothing to watch is dropped and frees its slot`() {
        val server = MjpegServer(0, { "{}" }, { "{}" }, "127.0.0.1", tokens = { listOf("secret-token") },
            viewerGiveUpMs = 200)
        server.start()
        try {
            Socket("127.0.0.1", actualPort(server)).use { socket ->
                socket.soTimeout = 3_000
                socket.getOutputStream().write("GET /v1/video.h264 HTTP/1.1\r\nAuthorization: Bearer secret-token\r\n\r\n"
                    .toByteArray(StandardCharsets.ISO_8859_1))
                val input = socket.getInputStream()
                readUntil(input, "\r\n\r\n".toByteArray())
                // No frames flow (the camera stalled, or the computer left): the phone ends it instead of holding the slot.
                assertEquals(-1, input.read())
            }
        } finally {
            server.stop()
        }
    }

    @Test
    fun `a viewer stuck in a blocked write is dropped while frames keep flowing`() {
        val server = MjpegServer(0, { "{}" }, { "{}" }, "127.0.0.1", tokens = { listOf("secret-token") },
            viewerGiveUpMs = 300)
        server.start()
        val sending = java.util.concurrent.atomic.AtomicBoolean(true)
        val frame = ByteArray(256 * 1024)
        val sender = Thread { while (sending.get()) { server.sendFrame(frame); Thread.sleep(20) } }
        try {
            Socket().use { socket ->
                socket.receiveBufferSize = 4096
                socket.connect(java.net.InetSocketAddress("127.0.0.1", actualPort(server)))
                socket.getOutputStream().write("GET /v1/video HTTP/1.1\r\nAuthorization: Bearer secret-token\r\n\r\n"
                    .toByteArray(StandardCharsets.ISO_8859_1))
                sender.start()
                Thread.sleep(1_500)  // not reading: the phone's write blocks, as on a link that died without a FIN
                socket.soTimeout = 3_000
                val input = socket.getInputStream()
                val buf = ByteArray(64 * 1024)
                val deadline = System.currentTimeMillis() + 3_000
                var ended = false
                while (!ended && System.currentTimeMillis() < deadline) {
                    ended = try { input.read(buf) < 0 } catch (_: java.io.IOException) { true }
                }
                assertTrue(ended, "the stalled viewer should have been closed")
            }
        } finally {
            sending.set(false)
            sender.join(2_000)
            server.stop()
        }
    }

    @Test
    fun `a request that finishes after stop does not start the mic`() {
        val started = java.util.concurrent.atomic.AtomicInteger()
        val server = MjpegServer(0, { "{}" }, { "{}" }, "127.0.0.1", tokens = { listOf("t") },
            startAudio = { started.incrementAndGet(); null })
        server.start()
        Socket("127.0.0.1", actualPort(server)).use { socket ->
            socket.soTimeout = 2_000
            val out = socket.getOutputStream()
            out.write("GET /v1/audio HTTP/1.1\r\n".toByteArray(StandardCharsets.ISO_8859_1))
            out.flush()
            Thread.sleep(100)
            server.stop()
            out.write("Authorization: Bearer t\r\n\r\n".toByteArray(StandardCharsets.ISO_8859_1))
            out.flush()
            try { socket.getInputStream().readBytes() } catch (_: Exception) {}
        }
        assertEquals(0, started.get())
    }

    @Test
    fun `a TLS handshake trickled a byte at a time is cut off at the request deadline`() {
        val identity = TlsIdentity.generate()
        val server = MjpegServer(0, { "{}" }, { "{}" }, "127.0.0.1", tokens = { listOf("t") },
            requestDeadlineMs = 300, socketFactory = identity.serverSocketFactory())
        server.start()
        try {
            Socket("127.0.0.1", actualPort(server)).use { socket ->
                socket.soTimeout = 3_000
                val out = socket.getOutputStream()
                // A TLS record header promising 16 KB, then the rest far too slowly.
                out.write(byteArrayOf(0x16, 0x03, 0x01, 0x40, 0x00))
                out.flush()
                val started = System.currentTimeMillis()
                var closed = false
                while (System.currentTimeMillis() - started < 3_000) {
                    try {
                        out.write(0)
                        out.flush()
                    } catch (_: Exception) { closed = true; break }
                    Thread.sleep(100)
                }
                assertTrue(closed)
            }
        } finally {
            server.stop()
        }
    }

    @Test
    fun `a video viewer counts as active only while it takes frames`() {
        val server = MjpegServer(0, { "{}" }, { "{}" }, "127.0.0.1", tokens = { listOf("secret-token") })
        server.start()
        try {
            assertFalse(server.hasActiveViewer())
            Socket("127.0.0.1", actualPort(server)).use { socket ->
                socket.soTimeout = 2_000
                socket.getOutputStream().apply {
                    write("GET /v1/video HTTP/1.1\r\nAuthorization: Bearer secret-token\r\n\r\n".toByteArray())
                    flush()
                }
                readUntil(socket.getInputStream(), "\r\n\r\n".toByteArray())
                assertTrue(server.hasActiveViewer())
                // Nothing written for longer than the stale window: a link that went quiet.
                val later = System.currentTimeMillis() + MjpegServer.VIEWER_STALE_MS + 1_000
                assertFalse(server.hasActiveViewer(later))
            }
        } finally {
            server.stop()
        }
    }

    @Test
    fun `idleForMs grows while nothing authorized arrives and resets on the next authorized request`() {
        val server = MjpegServer(0, { "{}" }, { "{}" }, "127.0.0.1", tokens = { listOf("secret-token") })
        server.start()
        try {
            Thread.sleep(60)
            assertTrue(server.idleForMs() >= 60)

            authGet(actualPort(server), "/v1/state", "secret-token")
            assertTrue(server.idleForMs() < 60)
        } finally {
            server.stop()
        }
    }

    @Test
    fun `idleForMs does not reset on an unauthorized request`() {
        val server = MjpegServer(0, { "{}" }, { "{}" }, "127.0.0.1", tokens = { listOf("secret-token") })
        server.start()
        try {
            Thread.sleep(60)
            authGet(actualPort(server), "/v1/state", "wrong-token")
            assertTrue(server.idleForMs() >= 60)
        } finally {
            server.stop()
        }
    }

    private fun readUntil(input: java.io.InputStream, marker: ByteArray): ByteArray {
        val out = ByteArrayOutputStream()
        while (true) {
            val next = input.read()
            if (next < 0) error("stream ended before marker")
            out.write(next)
            val bytes = out.toByteArray()
            if (bytes.size >= marker.size &&
                bytes.copyOfRange(bytes.size - marker.size, bytes.size).contentEquals(marker)) {
                return bytes
            }
        }
    }

    @Test
    fun `h264 route picks the codec, asks for a keyframe and streams config then packets`() {
        val codecs = java.util.concurrent.CopyOnWriteArrayList<String>()
        val keyRequests = java.util.concurrent.atomic.AtomicInteger()
        val server = MjpegServer(0, { "{}" }, { "{}" }, "127.0.0.1", tokens = { listOf("t") },
            onVideoClient = { codec, _, _ -> codecs.add(codec) }, requestKeyFrame = { keyRequests.incrementAndGet() })
        server.start()
        try {
            server.sendH264(byteArrayOf(0, 0, 0, 1, 0x67), key = false, config = true)
            Socket("127.0.0.1", actualPort(server)).use { socket ->
                socket.soTimeout = 2_000
                socket.getOutputStream().write("GET /v1/video.h264 HTTP/1.1\r\nAuthorization: Bearer t\r\n\r\n"
                    .toByteArray(StandardCharsets.ISO_8859_1))
                val input = socket.getInputStream()
                val head = StringBuilder()
                while (!head.endsWith("\r\n\r\n")) head.append(input.read().toChar())
                assertTrue(head.startsWith("HTTP/1.1 200"))
                assertTrue(head.contains("Content-Type: video/h264"))
                val deadline = System.currentTimeMillis() + 2_000
                while (codecs.isEmpty() && System.currentTimeMillis() < deadline) Thread.sleep(10)
                assertEquals(listOf(H264Stream.CODEC_H264), codecs.toList())
                assertTrue(keyRequests.get() >= 1)
                server.sendH264(byteArrayOf(9), key = false, config = false)  // dropped: not a keyframe
                server.sendH264(byteArrayOf(0, 0, 0, 1, 0x65), key = true, config = false)
                val want = byteArrayOf(0, 0, 0, 1, 0x67, 0, 0, 0, 1, 0x65) + H264Stream.AUD
                val got = ByteArray(want.size)
                var n = 0
                while (n < got.size) n += input.read(got, n, got.size - n)
                assertArrayEquals(want, got)
            }
        } finally {
            server.stop()
        }
    }

    @Test
    fun `h264 route requires the token`() {
        val server = MjpegServer(0, { "{}" }, { "{}" }, "127.0.0.1", tokens = { listOf("t") })
        server.start()
        try {
            assertEquals(401, authGet(actualPort(server), "/v1/video.h264", null).status)
        } finally {
            server.stop()
        }
    }

    @Test
    fun `audio route refuses with the reason when the mic can't start`() {
        val server = MjpegServer(0, { "{}" }, { "{}" }, "127.0.0.1", tokens = { listOf("t") },
            startAudio = { MicProblem.PERMISSION })
        server.start()
        try {
            val response = authGet(actualPort(server), "/v1/audio", "t")
            assertEquals(403, response.status)
            val body = response.body.toString(StandardCharsets.UTF_8)
            assertTrue(body.contains("Allow the microphone"))
            assertTrue(body.contains("\"code\":\"mic_permission\""))
        } finally {
            server.stop()
        }
    }

    @Test
    fun `audio route streams pcm while listened to and stops the mic after`() {
        val starts = java.util.concurrent.atomic.AtomicInteger()
        val stops = java.util.concurrent.atomic.AtomicInteger()
        val server = MjpegServer(0, { "{}" }, { "{}" }, "127.0.0.1", tokens = { listOf("t") },
            startAudio = { starts.incrementAndGet(); null }, stopAudio = { stops.incrementAndGet() })
        server.start()
        try {
            Socket("127.0.0.1", actualPort(server)).use { socket ->
                socket.soTimeout = 2_000
                socket.getOutputStream().write("GET /v1/audio HTTP/1.1\r\nAuthorization: Bearer t\r\n\r\n"
                    .toByteArray(StandardCharsets.ISO_8859_1))
                val input = socket.getInputStream()
                val head = StringBuilder()
                while (!head.endsWith("\r\n\r\n")) head.append(input.read().toChar())
                assertTrue(head.contains("Content-Type: ${AudioStream.CONTENT_TYPE}"))
                assertEquals(1, starts.get())
                server.sendAudio(byteArrayOf(1, 2, 3, 4))
                val got = ByteArray(4)
                var n = 0
                while (n < 4) n += input.read(got, n, 4 - n)
                assertArrayEquals(byteArrayOf(1, 2, 3, 4), got)
                // With the camera off the mic is the whole stream: a listener keeps the idle watchdog away.
                assertTrue(server.hasActiveViewer())
                assertFalse(server.hasActiveViewer(System.currentTimeMillis() + MjpegServer.VIEWER_STALE_MS + 1_000))
            }
            val deadline = System.currentTimeMillis() + 5_000
            while (stops.get() == 0 && System.currentTimeMillis() < deadline) {
                server.sendAudio(byteArrayOf(0))  // the write to the closed socket ends the listener
                Thread.sleep(20)
            }
            assertEquals(1, stops.get())
        } finally {
            server.stop()
        }
    }

    // A feed that counts what's started and hands out a new reading of each kind on every latest()
    private class FakeFeed(private val has: List<String>, private val width: Int = 1) : SensorFeed {
        val running = java.util.concurrent.ConcurrentHashMap<String, Int>()
        private var seq = 0L
        override fun available() = has
        override fun start(kinds: Set<String>): SensorHandle {
            val started = kinds.intersect(has.toSet())
            started.forEach { running.merge(it, 1, Int::plus) }
            return SensorHandle(started, 0)
        }
        override fun stop(handle: SensorHandle) {
            handle.kinds.forEach { k -> running.computeIfPresent(k) { _, n -> if (n > 1) n - 1 else null } }
        }
        @Synchronized override fun latest(): Map<String, SensorValue> {
            seq++
            return running.keys.associateWith { if (it == SensorKinds.ORIENTATION) SensorValue(seq, floatArrayOf(1.5f, -2f), "0")
                                                 else SensorValue(seq, FloatArray(width) { seq.toFloat() }) }
        }
    }

    private fun openSensors(port: Int, query: String, token: String = "t"): Socket = Socket("127.0.0.1", port).apply {
        soTimeout = 3_000
        getOutputStream().write("GET /v1/sensors$query HTTP/1.1\r\nAuthorization: Bearer $token\r\n\r\n"
            .toByteArray(StandardCharsets.ISO_8859_1))
    }

    @Test
    fun `sensors route needs the token and is missing on a phone without a feed`() {
        val server = MjpegServer(0, { "{}" }, { "{}" }, "127.0.0.1", tokens = { listOf("t") }, sensors = FakeFeed(SensorKinds.ALL))
        val old = MjpegServer(0, { "{}" }, { "{}" }, "127.0.0.1", tokens = { listOf("t") })
        server.start()
        old.start()
        try {
            assertEquals(401, authGet(actualPort(server), "/v1/sensors", "wrong").status)
            assertEquals(404, authGet(actualPort(old), "/v1/sensors", "t").status)
        } finally {
            server.stop()
            old.stop()
        }
    }

    @Test
    fun `sensors route refuses when the phone has none of the kinds asked for`() {
        val feed = FakeFeed(listOf(SensorKinds.LIGHT))
        val server = MjpegServer(0, { "{}" }, { "{}" }, "127.0.0.1", tokens = { listOf("t") }, sensors = feed)
        server.start()
        try {
            val response = authGet(actualPort(server), "/v1/sensors?kinds=gyroscope", "t")
            assertEquals(409, response.status)
            assertTrue(response.body.toString(StandardCharsets.UTF_8).contains("\"code\":\"sensors_none\""))
            assertTrue(feed.running.isEmpty())
        } finally {
            server.stop()
        }
    }

    @Test
    fun `sensors route streams the kinds it has while the computer reads, then lets them go`() {
        val feed = FakeFeed(listOf(SensorKinds.ORIENTATION, SensorKinds.LIGHT, SensorKinds.GRAVITY))
        val server = MjpegServer(0, { "{}" }, { "{}" }, "127.0.0.1", tokens = { listOf("t") }, sensors = feed)
        server.start()
        try {
            val socket = openSensors(actualPort(server), "?kinds=orientation,light,magnetometer")
            val reader = socket.getInputStream().bufferedReader()
            val head = generateSequence { reader.readLine() }.takeWhile { it.isNotEmpty() }.toList()
            assertTrue(head.any { it == "Content-Type: ${SensorWire.CONTENT_TYPE}" })
            val hello = kotlinx.serialization.json.Json.parseToJsonElement(reader.readLine()).toString()
            assertTrue(hello.contains("\"orientation\"") && hello.contains("\"light\"") && !hello.contains("magnetometer"))
            assertEquals(mapOf(SensorKinds.ORIENTATION to 1, SensorKinds.LIGHT to 1), feed.running.toMap())
            val lines = (1..20).map { reader.readLine() }
            assertTrue(lines.all { it.startsWith("{\"t\":") && !it.contains("gravity") })
            assertTrue(lines.all { it.contains("\"orientation\":{\"roll\":1.5,\"pitch\":-2.0,\"camera\":\"0\"}") })
            // Light at most twice a second, against orientation's 30
            assertTrue(lines.count { it.contains("\"light\"") } <= 2)
            socket.close()
            val deadline = System.currentTimeMillis() + 5_000
            while (feed.running.isNotEmpty() && System.currentTimeMillis() < deadline) Thread.sleep(20)
            assertTrue(feed.running.isEmpty(), "the sensors should stop once the computer is gone")
        } finally {
            server.stop()
        }
    }

    @Test
    fun `stopping the server ends a sensor stream and unpairing cuts it off`() {
        val feed = FakeFeed(listOf(SensorKinds.ORIENTATION))
        val paired = AtomicReference(listOf("t", "gone"))
        val server = MjpegServer(0, { "{}" }, { "{}" }, "127.0.0.1", tokens = { paired.get() }, sensors = feed)
        server.start()
        try {
            val kept = openSensors(actualPort(server), "", "t")
            val removed = openSensors(actualPort(server), "", "gone")
            val keptReader = kept.getInputStream().bufferedReader()
            val removedReader = removed.getInputStream().bufferedReader()
            repeat(6) { keptReader.readLine(); removedReader.readLine() }
            assertEquals(2, feed.running[SensorKinds.ORIENTATION])
            paired.set(listOf("t"))
            assertTrue(generateSequence { removedReader.readLine() }.count() < 100)  // ends
            server.stop()
            assertTrue(generateSequence { try { keptReader.readLine() } catch (_: java.io.IOException) { null } }.count() < 100)
            val deadline = System.currentTimeMillis() + 5_000
            while (feed.running.isNotEmpty() && System.currentTimeMillis() < deadline) Thread.sleep(20)
            assertTrue(feed.running.isEmpty())
        } finally {
            server.stop()
        }
    }

    @Test
    fun `a computer that sends no keepalive loses its sensors even while every line still fits`() {
        val quiet = FakeFeed(listOf(SensorKinds.LIGHT))
        val server = MjpegServer(0, { "{}" }, { "{}" }, "127.0.0.1", tokens = { listOf("t", "u") }, sensors = quiet,
            sensorAliveMs = 700L)
        server.start()
        try {
            val gone = openSensors(actualPort(server), "", "t")  // never asks anything again, never closes
            val kept = openSensors(actualPort(server), "", "u")
            val pinging = java.util.concurrent.atomic.AtomicBoolean(true)
            val pinger = Thread {
                while (pinging.get()) {
                    runCatching { assertEquals(200, authGet(actualPort(server), "/v1/sensors/alive", "u").status) }
                    Thread.sleep(150)
                }
            }.apply { start() }
            val deadline = System.currentTimeMillis() + 5_000
            while (quiet.running[SensorKinds.LIGHT] != 1 && System.currentTimeMillis() < deadline) Thread.sleep(20)
            Thread.sleep(1_200)  // past the keepalive time
            assertEquals(1, quiet.running[SensorKinds.LIGHT], "only the computer that keeps asking keeps the sensor")
            pinging.set(false)
            pinger.join(2_000)
            val end = System.currentTimeMillis() + 5_000
            while (quiet.running.isNotEmpty() && System.currentTimeMillis() < end) Thread.sleep(20)
            assertTrue(quiet.running.isEmpty())
            gone.close()
            kept.close()
        } finally {
            server.stop()
        }
    }

    @Test
    fun `the keepalive needs the token and a phone with sensors`() {
        val server = MjpegServer(0, { "{}" }, { "{}" }, "127.0.0.1", tokens = { listOf("t") }, sensors = FakeFeed(SensorKinds.ALL))
        val old = MjpegServer(0, { "{}" }, { "{}" }, "127.0.0.1", tokens = { listOf("t") })
        server.start()
        old.start()
        try {
            assertEquals(200, authGet(actualPort(server), "/v1/sensors/alive", "t").status)
            assertEquals(401, authGet(actualPort(server), "/v1/sensors/alive", "x").status)
            assertEquals(404, authGet(actualPort(old), "/v1/sensors/alive", "t").status)
        } finally {
            server.stop()
            old.stop()
        }
    }

    @Test
    fun `a computer that stops reading without closing loses its sensors`() {
        // Wide readings fill the socket buffers in moments, like seconds of normal ones would
        val feed = FakeFeed(listOf(SensorKinds.GRAVITY), width = 2_000)
        val server = MjpegServer(0, { "{}" }, { "{}" }, "127.0.0.1", tokens = { listOf("t") }, sensors = feed,
            sensorStallMs = 800L)
        server.start()
        try {
            val socket = Socket()
            socket.receiveBufferSize = 4 * 1024
            socket.connect(java.net.InetSocketAddress("127.0.0.1", actualPort(server)))
            socket.getOutputStream().write("GET /v1/sensors HTTP/1.1\r\nAuthorization: Bearer t\r\n\r\n"
                .toByteArray(StandardCharsets.ISO_8859_1))
            val deadline = System.currentTimeMillis() + 2_000
            while (feed.running.isEmpty() && System.currentTimeMillis() < deadline) Thread.sleep(10)
            assertEquals(1, feed.running[SensorKinds.GRAVITY])
            // Never reads again and never closes, like a laptop that went to sleep
            val gone = System.currentTimeMillis() + 15_000
            while (feed.running.isNotEmpty() && System.currentTimeMillis() < gone) Thread.sleep(50)
            assertTrue(feed.running.isEmpty(), "the sensors should stop once the writes are stuck")
            socket.close()
        } finally {
            server.stop()
        }
    }

    @Test
    fun `removing a computer cuts off the video and mic it was already taking, not another computer's`() {
        val paired = AtomicReference(listOf("removed", "kept"))
        val stops = java.util.concurrent.atomic.AtomicInteger()
        val server = MjpegServer(0, { "{}" }, { "{}" }, "127.0.0.1", tokens = { paired.get() },
            startAudio = { null }, stopAudio = { stops.incrementAndGet() })
        server.start()
        val sending = java.util.concurrent.atomic.AtomicBoolean(true)
        val sender = Thread {
            while (sending.get()) { server.sendFrame(byteArrayOf(1, 2, 3)); server.sendAudio(byteArrayOf(0, 0)); Thread.sleep(20) }
        }
        fun open(path: String, token: String): Socket = Socket("127.0.0.1", actualPort(server)).apply {
            soTimeout = 5_000
            getOutputStream().write("GET $path HTTP/1.1\r\nAuthorization: Bearer $token\r\n\r\n"
                .toByteArray(StandardCharsets.ISO_8859_1))
            readUntil(getInputStream(), "\r\n\r\n".toByteArray())
        }
        fun ends(socket: Socket): Boolean {
            val buf = ByteArray(4096)
            val deadline = System.currentTimeMillis() + 5_000
            while (System.currentTimeMillis() < deadline) {
                if (try { socket.getInputStream().read(buf) < 0 } catch (_: java.io.IOException) { true }) return true
            }
            return false
        }
        try {
            val video = open("/v1/video", "removed")
            val mic = open("/v1/audio", "removed")
            val other = open("/v1/video", "kept")
            sender.start()
            paired.set(listOf("kept"))
            assertTrue(ends(video), "the removed computer's video should end")
            assertTrue(ends(mic), "the removed computer's mic should end")
            val deadline = System.currentTimeMillis() + 2_000
            while (stops.get() == 0 && System.currentTimeMillis() < deadline) Thread.sleep(20)
            assertEquals(1, stops.get())
            other.soTimeout = 1_000
            assertTrue(other.getInputStream().read(ByteArray(64)) > 0)
            listOf(video, mic, other).forEach { it.close() }
        } finally {
            sending.set(false)
            sender.join(2_000)
            server.stop()
        }
    }

    @Test
    fun `strangers holding connections open don't use up the stream slots`() {
        val server = MjpegServer(0, { "{}" }, { "{}" }, "127.0.0.1", tokens = { listOf("t") })
        server.start()
        val port = actualPort(server)
        val idle = (1..PendingLimiter.MAX_PER_ADDRESS + 20).map {
            runCatching {
                Socket().apply { bind(java.net.InetSocketAddress("127.0.0.2", 0)); connect(java.net.InetSocketAddress("127.0.0.1", port)) }
            }.getOrNull()
        }
        try {
            assertEquals(200, authGet(port, "/v1/state", "t").status)
            Socket("127.0.0.1", port).use { socket ->
                socket.soTimeout = 2_000
                socket.getOutputStream().write("GET /v1/video HTTP/1.1\r\nAuthorization: Bearer t\r\n\r\n"
                    .toByteArray(StandardCharsets.ISO_8859_1))
                val head = StringBuilder()
                val input = socket.getInputStream()
                while (!head.endsWith("\r\n\r\n")) head.append(input.read().toChar())
                assertTrue(head.startsWith("HTTP/1.1 200"))
            }
        } finally {
            idle.forEach { it?.close() }
            server.stop()
        }
    }

    @Test
    fun `a request still arriving after the deadline is dropped`() {
        val server = MjpegServer(0, { "{}" }, { "{}" }, "127.0.0.1", tokens = { listOf("t") }, requestDeadlineMs = 300)
        server.start()
        try {
            Socket("127.0.0.1", actualPort(server)).use { socket ->
                socket.soTimeout = 3_000
                val out = socket.getOutputStream()
                val started = System.currentTimeMillis()
                val closed = runCatching {
                    repeat(60) { out.write('a'.code); out.flush(); Thread.sleep(50) }
                }.isFailure || socket.getInputStream().read() == -1
                assertTrue(closed)
                assertTrue(System.currentTimeMillis() - started < 3_500)
            }
        } finally {
            server.stop()
        }
    }
}
