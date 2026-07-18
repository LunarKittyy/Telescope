package com.telescope

import java.net.InetAddress
import java.net.InetSocketAddress
import java.net.ServerSocket
import java.net.Socket
import java.security.MessageDigest
import java.util.concurrent.atomic.AtomicBoolean
import kotlin.concurrent.thread

/**
 * A tiny, always-available HTTP responder for a single question: "is this
 * bearer token the one this phone is currently paired with?" Separate from
 * [MjpegServer] (which only exists while actively streaming, per
 * [CameraStreamService.onStartCommand]) and bound to its own port, so the
 * desktop can show real pairing status - and warn the user before they hit
 * Start - without a stream already running. Started/stopped alongside
 * MainActivity's own foreground lifetime, same as its pairReceiver.
 *
 * [tokenProvider] is read fresh on every request rather than captured once,
 * so a re-pair while this server is already running (no restart in between)
 * is reflected on the very next check.
 */
class PingServer(
    private val port: Int,
    private val tokenProvider: () -> String?,
) {
    private var serverSocket: ServerSocket? = null
    private val running = AtomicBoolean(false)

    /** Best-effort: a bind failure (e.g. the port is already taken by
     *  something else) just means status checks report "unreachable" - not
     *  worth crashing the app over. */
    fun start() {
        try {
            running.set(true)
            serverSocket = ServerSocket().apply {
                reuseAddress = true
                bind(InetSocketAddress(InetAddress.getByName("0.0.0.0"), port), 10)
            }
        } catch (e: Exception) {
            android.util.Log.w("PingServer", "Could not bind port $port", e)
            running.set(false)
            return
        }
        thread(name = "ping-accept", isDaemon = true) {
            while (running.get()) {
                try {
                    val socket = serverSocket?.accept() ?: break
                    thread(name = "ping-client", isDaemon = true) { handle(socket) }
                } catch (e: Exception) {
                    if (running.get()) android.util.Log.e("PingServer", "Accept error", e)
                }
            }
        }
    }

    fun stop() {
        running.set(false)
        try { serverSocket?.close() } catch (_: Exception) {}
    }

    private fun handle(socket: Socket) {
        try {
            socket.soTimeout = 3000
            val inp = socket.getInputStream()
            val buf = ByteArray(4096)
            val sb = StringBuilder()
            while (!sb.contains("\r\n\r\n") && !sb.contains("\n\n")) {
                if (sb.length >= 8192) return
                val n = inp.read(buf)
                if (n <= 0) return
                sb.append(String(buf, 0, n, Charsets.ISO_8859_1))
            }
            val lines = sb.toString().split("\r\n", "\n")
            val parts = (lines.firstOrNull() ?: "").split(" ")
            val method = parts.getOrNull(0)
            val path = parts.getOrNull(1)?.substringBefore("?")
            val authHeader = lines.drop(1)
                .firstOrNull { it.startsWith("authorization:", ignoreCase = true) }
                ?.substringAfter(":")?.trim()

            if (method != "GET" || path != "/v1/ping") {
                respond(socket, 404, "Not Found")
                return
            }
            val expected = tokenProvider()
            val provided = authHeader?.removePrefix("Bearer ")
            val authorized = expected != null && provided != null && MessageDigest.isEqual(
                expected.toByteArray(Charsets.UTF_8), provided.toByteArray(Charsets.UTF_8),
            )
            respond(socket, if (authorized) 200 else 401, if (authorized) "OK" else "Unauthorized")
        } catch (_: Exception) {
        } finally {
            try { socket.close() } catch (_: Exception) {}
        }
    }

    private fun respond(socket: Socket, code: Int, reason: String) {
        try {
            val out = socket.getOutputStream()
            val body = reason.toByteArray(Charsets.UTF_8)
            val hdr = "HTTP/1.1 $code $reason\r\nContent-Length: ${body.size}\r\nConnection: close\r\n\r\n"
            out.write(hdr.toByteArray(Charsets.UTF_8))
            out.write(body)
            out.flush()
        } catch (_: Exception) {}
    }

    companion object {
        const val DEFAULT_PORT = 8766
    }
}
