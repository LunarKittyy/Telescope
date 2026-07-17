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

    @Test
    fun `cameras endpoint returns UTF-8 JSON with CORS and length`() {
        val body = "{\"camera\":\"télé\"}"
        val server = MjpegServer(0, { body }, { "{}" }, "127.0.0.1")
        server.start()
        try {
            val response = request(
                actualPort(server),
                "GET /cameras HTTP/1.1\r\nHost: localhost\r\n\r\n",
            )
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
    fun `control endpoint decodes query parameters and ignores pairs without equals`() {
        val received = AtomicReference<Map<String, String>>()
        val server = MjpegServer(
            0,
            { "{}" },
            { params -> received.set(params); "{\"ok\":true}" },
            "127.0.0.1",
        )
        server.start()
        try {
            val response = request(
                actualPort(server),
                "GET /control?action=camera&id=wide+angle&path=a%2Fb&ignored HTTP/1.1\r\n\r\n",
            )
            assertEquals(200, response.status)
            assertEquals(
                mapOf("action" to "camera", "id" to "wide angle", "path" to "a/b"),
                received.get(),
            )
        } finally {
            server.stop()
        }
    }

    @Test
    fun `empty control query is accepted`() {
        val received = AtomicReference<Map<String, String>>()
        val server = MjpegServer(
            0,
            { "{}" },
            { params -> received.set(params); "{}" },
            "127.0.0.1",
        )
        server.start()
        try {
            val response = request(actualPort(server), "GET /control HTTP/1.1\r\n\r\n")
            assertEquals(200, response.status)
            assertEquals(emptyMap<String, String>(), received.get())
        } finally {
            server.stop()
        }
    }

    @Test
    fun `malformed percent encoding is rejected before control handler`() {
        var called = false
        val server = MjpegServer(
            0,
            { "{}" },
            { called = true; "{}" },
            "127.0.0.1",
        )
        server.start()
        try {
            val response = request(
                actualPort(server),
                "GET /control?action=%ZZ HTTP/1.1\r\n\r\n",
            )
            assertEquals(400, response.status)
            assertTrue(!called)
        } finally {
            server.stop()
        }
    }

    @Test
    fun `unknown paths and non-GET methods return errors`() {
        val server = MjpegServer(0, { "{}" }, { "{}" }, "127.0.0.1")
        server.start()
        try {
            assertEquals(
                404,
                request(actualPort(server), "GET /missing HTTP/1.1\r\n\r\n").status,
            )
            assertEquals(
                400,
                request(actualPort(server), "POST /cameras HTTP/1.1\r\n\r\n").status,
            )
        } finally {
            server.stop()
        }
    }

    @Test
    fun `oversized request headers return 431`() {
        val server = MjpegServer(0, { "{}" }, { "{}" }, "127.0.0.1")
        server.start()
        try {
            val raw = "GET /cameras HTTP/1.1\r\nX-Fill: ${"x".repeat(17 * 1024)}\r\n\r\n"
            assertEquals(431, request(actualPort(server), raw).status)
        } finally {
            server.stop()
        }
    }

    @Test
    fun `video endpoint streams a queued JPEG frame`() {
        val server = MjpegServer(0, { "{}" }, { "{}" }, "127.0.0.1")
        server.start()
        try {
            Socket("127.0.0.1", actualPort(server)).use { socket ->
                socket.soTimeout = 2_000
                socket.getOutputStream().apply {
                    write("GET /video HTTP/1.1\r\n\r\n".toByteArray())
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
