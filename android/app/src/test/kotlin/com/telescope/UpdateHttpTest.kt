package com.telescope

import java.io.File
import java.io.IOException
import java.net.ServerSocket
import java.net.Socket
import java.nio.file.Files
import java.security.MessageDigest
import org.junit.jupiter.api.AfterEach
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNotNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.assertThrows

// A wifi can reach the computer and nothing else. Every way a connection with no internet behind it fails
// has to end as a failed check or a download message, quickly, with no file left behind.
class UpdateHttpTest {

    private val http = UpdateHttp("test", timeoutMs = 500)
    private val servers = mutableListOf<ServerSocket>()
    private val payload = ByteArray(1000) { 'z'.code.toByte() }

    @AfterEach
    fun closeServers() = servers.forEach { it.close() }

    private fun serve(reply: (Socket) -> Unit): String {
        val server = ServerSocket(0)
        servers += server
        Thread {
            runCatching {
                while (true) {
                    server.accept().use { conn ->
                        conn.getInputStream().read(ByteArray(4096))
                        reply(conn)
                    }
                }
            }
        }.apply { isDaemon = true }.start()
        return "http://127.0.0.1:${server.localPort}/x"
    }

    private fun send(conn: Socket, text: String) = conn.getOutputStream().apply { write(text.toByteArray()); flush() }

    private val offline: Map<String, () -> String> = mapOf(
        "refused" to { ServerSocket(0).use { "http://127.0.0.1:${it.localPort}/x" } },
        "no dns" to { "http://telescope-update-test.invalid/x" },
        "no answer" to { serve { Thread.sleep(5000) } },
        "stalls mid body" to { serve { send(it, "HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\n{"); Thread.sleep(5000) } },
        "cut off" to { serve { send(it, "HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\n{") } },
        "not http" to { serve { send(it, "hello\r\n\r\n") } },
        "reset" to { serve { it.setSoLinger(true, 0) } },
        "login page" to { serve { send(it, "HTTP/1.1 200 OK\r\nContent-Length: 13\r\n\r\n<html></html>") } },
    )

    private fun timed(name: String, block: () -> Unit) {
        val started = System.nanoTime()
        block()
        val ms = (System.nanoTime() - started) / 1_000_000
        assertTrue(ms < 3000, "$name took ${ms}ms")
    }

    @Test
    fun `a check without internet fails instead of reading as up to date`() {
        for ((name, url) in offline) {
            timed(name) { assertThrows<IOException>(name) { http.fetchManifest(url()) } }
        }
    }

    @Test
    fun `a download without internet says so and leaves nothing`() {
        val sha = MessageDigest.getInstance("SHA-256").digest(payload).joinToString("") { "%02x".format(it) }
        for ((name, url) in offline) {
            val dir = Files.createTempDirectory("update").toFile()
            val asset = ManifestAsset("Telescope.apk", url(), sha, payload.size.toLong())
            var message: String? = null
            timed(name) { message = http.download(asset, File(dir, "Telescope.apk")) {} }
            assertNotNull(message, name)
            if (name != "login page") assertTrue("internet connection" in message!!, "$name: $message")
            assertEquals(emptyList<String>(), dir.list()!!.toList(), name)
            dir.deleteRecursively()
        }
    }
}
