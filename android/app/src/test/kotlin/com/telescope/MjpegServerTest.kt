package com.telescope

import org.junit.jupiter.api.Assertions.assertArrayEquals
import org.junit.jupiter.api.Assertions.assertEquals
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
    fun `v1 state endpoint returns UTF-8 JSON with CORS and length when authorized`() {
        val body = "{\"camera\":\"télé\"}"
        val server = MjpegServer(0, { body }, { "{}" }, "127.0.0.1", token = "secret-token")
        server.start()
        try {
            val response = authGet(actualPort(server), "/v1/state", "secret-token")
            assertEquals(200, response.status)
            assertEquals(body, response.body.toString(StandardCharsets.UTF_8))
            assertTrue(response.headers.contains("Content-Type: application/json"))
            assertTrue(response.headers.contains("Content-Length: ${body.toByteArray().size}"))
            assertTrue(response.headers.contains("Access-Control-Allow-Origin: *"))
        } finally {
            server.stop()
        }
    }

    @Test
    fun `v1 state endpoint rejects missing authorization header`() {
        val server = MjpegServer(0, { "{}" }, { "{}" }, "127.0.0.1", token = "secret-token")
        server.start()
        try {
            assertEquals(401, authGet(actualPort(server), "/v1/state", null).status)
        } finally {
            server.stop()
        }
    }

    @Test
    fun `v1 state endpoint rejects wrong token`() {
        val server = MjpegServer(0, { "{}" }, { "{}" }, "127.0.0.1", token = "secret-token")
        server.start()
        try {
            assertEquals(401, authGet(actualPort(server), "/v1/state", "wrong-token").status)
        } finally {
            server.stop()
        }
    }

    @Test
    fun `v1 state endpoint rejects everything when no token is paired yet`() {
        val server = MjpegServer(0, { "{}" }, { "{}" }, "127.0.0.1", token = null)
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
            token = "secret-token",
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
        val server = MjpegServer(0, { "{}" }, { called = true; "{}" }, "127.0.0.1", token = "secret-token")
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
        val server = MjpegServer(0, { "{}" }, { "{}" }, "127.0.0.1", token = "secret-token")
        server.start()
        try {
            assertEquals(400, authGet(actualPort(server), "/v1/control", "secret-token").status)
        } finally {
            server.stop()
        }
    }

    @Test
    fun `v1 control endpoint rejects missing content-length`() {
        val server = MjpegServer(0, { "{}" }, { "{}" }, "127.0.0.1", token = "secret-token")
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
        val server = MjpegServer(0, { "{}" }, { "{}" }, "127.0.0.1", token = "secret-token")
        server.start()
        try {
            val raw = "POST /v1/control HTTP/1.1\r\nAuthorization: Bearer secret-token\r\nContent-Length: 4097\r\n\r\n"
            assertEquals(413, request(actualPort(server), raw).status)
        } finally {
            server.stop()
        }
    }

    @Test
    fun `legacy unversioned routes are gone`() {
        val server = MjpegServer(0, { "{}" }, { "{}" }, "127.0.0.1", token = "secret-token")
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
        val server = MjpegServer(0, { "{}" }, { "{}" }, "127.0.0.1", token = "secret-token")
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
        val server = MjpegServer(0, { "{}" }, { "{}" }, "127.0.0.1", token = "secret-token")
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
        val server = MjpegServer(0, { "{}" }, { "{}" }, "127.0.0.1", token = "secret-token")
        server.start()
        try {
            assertEquals(401, authGet(actualPort(server), "/v1/video", null).status)
        } finally {
            server.stop()
        }
    }

    @Test
    fun `video endpoint streams a queued JPEG frame when authorized`() {
        val server = MjpegServer(0, { "{}" }, { "{}" }, "127.0.0.1", token = "secret-token")
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
}
