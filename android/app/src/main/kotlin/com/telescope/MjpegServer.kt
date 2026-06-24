package com.telescope

import java.io.OutputStream
import java.net.ServerSocket
import java.net.Socket
import java.util.concurrent.ArrayBlockingQueue
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import kotlin.concurrent.thread

/**
 * HTTP server that serves:
 *   GET /video          – MJPEG stream (multipart/x-mixed-replace)
 *   GET /cameras        – JSON list of all cameras + current state
 *   GET /control?...    – Camera control commands, returns JSON
 */
class MjpegServer(
    val port: Int,
    val getCamerasJson: () -> String,
    val handleControl: (Map<String, String>) -> String,
    val bindAddr: String = "0.0.0.0",
) {
    private var serverSocket: ServerSocket? = null
    private val clients = CopyOnWriteArrayList<MjpegClient>()
    private val running = AtomicBoolean(false)

    fun start() {
        running.set(true)
        serverSocket = ServerSocket(port, 50, java.net.InetAddress.getByName(bindAddr))
        thread(name = "mjpeg-accept", isDaemon = true) {
            while (running.get()) {
                try {
                    val socket = serverSocket?.accept() ?: break
                    thread(name = "mjpeg-client", isDaemon = true) { dispatch(socket) }
                } catch (e: Exception) {
                    if (running.get()) android.util.Log.e("MjpegServer", "Accept error", e)
                }
            }
        }
    }

    fun sendFrame(jpeg: ByteArray) {
        val dead = mutableListOf<MjpegClient>()
        for (c in clients) { if (!c.enqueue(jpeg)) dead.add(c) }
        if (dead.isNotEmpty()) clients.removeAll(dead.toSet())
    }

    fun stop() {
        running.set(false)
        clients.forEach { it.close() }
        clients.clear()
        try { serverSocket?.close() } catch (_: Exception) {}
    }

    // ── HTTP dispatch ───────────────────────────────────────────────────────

    private fun dispatch(socket: Socket) {
        try {
            val inp = socket.getInputStream()
            val buf = ByteArray(4096)
            var raw = ""
            while (!raw.contains("\r\n\r\n") && !raw.contains("\n\n")) {
                val n = inp.read(buf)
                if (n <= 0) { socket.close(); return }
                raw += String(buf, 0, n, Charsets.ISO_8859_1)
            }
            val path = raw.split("\r\n", "\n").firstOrNull()
                ?.split(" ")?.getOrNull(1) ?: "/video"

            when {
                path.startsWith("/cameras") -> {
                    sendJson(socket.getOutputStream(), getCamerasJson())
                    socket.close()
                }
                path.startsWith("/control") -> {
                    val params = parseQuery(path.substringAfter("?", ""))
                    sendJson(socket.getOutputStream(), handleControl(params))
                    socket.close()
                }
                else -> {   // /video or anything else → MJPEG stream
                    val client = MjpegClient(socket)
                    clients.add(client)
                    client.stream()          // blocks until disconnected
                    clients.remove(client)
                }
            }
        } catch (_: Exception) {
            try { socket.close() } catch (_: Exception) {}
        }
    }

    private fun sendJson(out: OutputStream, json: String) {
        val body = json.toByteArray(Charsets.UTF_8)
        val hdr  = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n" +
                   "Content-Length: ${body.size}\r\nAccess-Control-Allow-Origin: *\r\n\r\n"
        out.write(hdr.toByteArray(Charsets.UTF_8))
        out.write(body)
        out.flush()
    }

    private fun parseQuery(q: String): Map<String, String> =
        q.split("&").filter { "=" in it }
            .associate { it.substringBefore("=") to it.substringAfter("=") }

    // ── MJPEG client ────────────────────────────────────────────────────────

    inner class MjpegClient(private val socket: Socket) {
        private val queue = ArrayBlockingQueue<ByteArray>(2)
        private val alive = AtomicBoolean(true)

        fun stream() {
            try {
                val out = socket.getOutputStream()
                val hdr = "HTTP/1.1 200 OK\r\n" +
                    "Content-Type: multipart/x-mixed-replace; boundary=--mjpegframe\r\n" +
                    "Cache-Control: no-cache\r\nConnection: keep-alive\r\n\r\n"
                out.write(hdr.toByteArray(Charsets.UTF_8))
                out.flush()

                while (alive.get()) {
                    val frame = queue.poll(2_000L, TimeUnit.MILLISECONDS) ?: continue
                    val partHdr = "--mjpegframe\r\nContent-Type: image/jpeg\r\n" +
                                  "Content-Length: ${frame.size}\r\n\r\n"
                    out.write(partHdr.toByteArray(Charsets.UTF_8))
                    out.write(frame)
                    out.write("\r\n".toByteArray(Charsets.UTF_8))
                    out.flush()
                }
            } catch (_: Exception) {}
            finally { alive.set(false); try { socket.close() } catch (_: Exception) {} }
        }

        fun enqueue(jpeg: ByteArray): Boolean {
            if (!alive.get() || socket.isClosed) return false
            queue.poll()   // drop oldest to keep latency low
            queue.offer(jpeg)
            return true
        }

        fun close() { alive.set(false); try { socket.close() } catch (_: Exception) {} }
    }
}
