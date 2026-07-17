package com.telescope

import java.io.OutputStream
import java.net.ServerSocket
import java.net.Socket
import java.net.SocketTimeoutException
import java.net.URLDecoder
import java.util.concurrent.ArrayBlockingQueue
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.Semaphore
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

    // Bounds total connections being served at once (streaming + short-lived
    // /cameras and /control requests) so a peer that opens many partial or
    // slow connections can't exhaust a thread per connection indefinitely.
    private val clientSlots = Semaphore(MAX_CONCURRENT_CLIENTS)

    fun start() {
        running.set(true)
        // Bind via the no-arg constructor + explicit setReuseAddress(true) instead of
        // the ServerSocket(port, backlog, addr) convenience constructor, which binds
        // immediately and gives no chance to set SO_REUSEADDR first. Without it, a
        // quick stop-then-start (the previous MjpegServer's socket just closed, or its
        // last accepted /video connection still winding down into TIME_WAIT) can hit
        // EADDRINUSE on the same port — and since this used to be uncaught, it crashed
        // the whole app instead of just failing this one restart.
        serverSocket = ServerSocket().apply {
            reuseAddress = true
            bind(java.net.InetSocketAddress(java.net.InetAddress.getByName(bindAddr), port), 50)
        }
        thread(name = "mjpeg-accept", isDaemon = true) {
            while (running.get()) {
                try {
                    val socket = serverSocket?.accept() ?: break
                    if (!clientSlots.tryAcquire()) {
                        thread(name = "mjpeg-reject", isDaemon = true) { rejectBusy(socket) }
                        continue
                    }
                    thread(name = "mjpeg-client", isDaemon = true) {
                        try { dispatch(socket) } finally { clientSlots.release() }
                    }
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

    private fun rejectBusy(socket: Socket) {
        try {
            socket.soTimeout = READ_TIMEOUT_MS
            sendError(socket.getOutputStream(), 503, "Service Unavailable")
        } catch (_: Exception) {
        } finally {
            try { socket.close() } catch (_: Exception) {}
        }
    }

    // ── HTTP dispatch ───────────────────────────────────────────────────────

    private fun dispatch(socket: Socket) {
        var streaming = false
        try {
            socket.soTimeout = READ_TIMEOUT_MS
            val request = readRequest(socket) ?: return  // already responded/closed on error

            when (request.path) {
                "/cameras" -> sendJson(socket.getOutputStream(), getCamerasJson())
                "/control" -> {
                    val params = parseQuery(request.query)
                    if (params == null) {
                        sendError(socket.getOutputStream(), 400, "Bad Request")
                    } else {
                        sendJson(socket.getOutputStream(), handleControl(params))
                    }
                }
                "/video" -> {
                    streaming = true
                    val client = MjpegClient(socket)
                    clients.add(client)
                    client.stream()          // blocks until disconnected
                    clients.remove(client)
                }
                else -> sendError(socket.getOutputStream(), 404, "Not Found")
            }
        } catch (_: SocketTimeoutException) {
            // Client opened a connection but never finished sending a request.
        } catch (_: Exception) {
        } finally {
            if (!streaming) try { socket.close() } catch (_: Exception) {}
        }
    }

    private data class Request(val method: String, val path: String, val query: String)

    /**
     * Reads and parses the request line, bounded by [MAX_HEADER_BYTES] so a
     * client that never sends a terminator can't hold a thread's read buffer
     * open indefinitely. Only `GET` is accepted. Returns null (having already
     * written an error response and closed the socket) on any parse failure.
     */
    private fun readRequest(socket: Socket): Request? {
        val inp = socket.getInputStream()
        val buf = ByteArray(4096)
        val sb = StringBuilder()
        while (!sb.contains("\r\n\r\n") && !sb.contains("\n\n")) {
            if (sb.length >= MAX_HEADER_BYTES) {
                sendError(socket.getOutputStream(), 431, "Request Header Fields Too Large")
                socket.close()
                return null
            }
            val n = inp.read(buf)
            if (n <= 0) { socket.close(); return null }
            sb.append(String(buf, 0, n, Charsets.ISO_8859_1))
        }
        val requestLine = sb.toString().split("\r\n", "\n").firstOrNull() ?: ""
        val parts = requestLine.split(" ")
        if (parts.size < 2 || parts[0] != "GET") {
            sendError(socket.getOutputStream(), 400, "Bad Request")
            socket.close()
            return null
        }
        val fullPath = parts[1]
        val path = fullPath.substringBefore("?")
        val query = fullPath.substringAfter("?", "")
        return Request("GET", path, query)
    }

    private fun sendJson(out: OutputStream, json: String) {
        val body = json.toByteArray(Charsets.UTF_8)
        val hdr  = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n" +
                   "Content-Length: ${body.size}\r\nAccess-Control-Allow-Origin: *\r\n\r\n"
        out.write(hdr.toByteArray(Charsets.UTF_8))
        out.write(body)
        out.flush()
    }

    private fun sendError(out: OutputStream, code: Int, reason: String) {
        try {
            val body = reason.toByteArray(Charsets.UTF_8)
            val hdr = "HTTP/1.1 $code $reason\r\nContent-Type: text/plain\r\n" +
                      "Content-Length: ${body.size}\r\nConnection: close\r\n\r\n"
            out.write(hdr.toByteArray(Charsets.UTF_8))
            out.write(body)
            out.flush()
        } catch (_: Exception) {}
    }

    /** Decodes query parameters; returns null if any value is malformed percent-encoding. */
    private fun parseQuery(q: String): Map<String, String>? {
        if (q.isEmpty()) return emptyMap()
        val result = mutableMapOf<String, String>()
        for (pair in q.split("&")) {
            if ("=" !in pair) continue
            try {
                val key = URLDecoder.decode(pair.substringBefore("="), "UTF-8")
                val value = URLDecoder.decode(pair.substringAfter("="), "UTF-8")
                result[key] = value
            } catch (_: IllegalArgumentException) {
                return null
            }
        }
        return result
    }

    companion object {
        private const val MAX_CONCURRENT_CLIENTS = 16
        private const val MAX_HEADER_BYTES = 16 * 1024
        private const val READ_TIMEOUT_MS = 5_000
    }

    // ── MJPEG client ────────────────────────────────────────────────────────

    inner class MjpegClient(private val socket: Socket) {
        private val queue = ArrayBlockingQueue<ByteArray>(2)
        private val alive = AtomicBoolean(true)

        fun stream() {
            try {
                socket.soTimeout = 0  // streaming connections are long-lived by design
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
