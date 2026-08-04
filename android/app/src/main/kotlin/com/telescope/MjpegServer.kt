package com.telescope

import java.net.ServerSocket
import java.net.Socket
import java.net.SocketTimeoutException
import java.util.concurrent.ArrayBlockingQueue
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.Semaphore
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import kotlin.concurrent.thread

/**
 * HTTP server that serves:
 *   GET  /v1/video    - MJPEG stream (multipart/x-mixed-replace)
 *   GET  /v1/state    - JSON list of all cameras + current state
 *   POST /v1/control  - Camera control commands (JSON body), returns JSON
 *
 * All three routes require a bearer token matching [token], checked with a
 * constant-time comparison ([java.security.MessageDigest.isEqual]). A null
 * [token] (nothing paired yet) rejects every request with 401.
 *
 * Lives only while a stream is running - it is created in
 * [CameraStreamService.startServer] and destroyed in
 * [CameraStreamService.stopStreaming]. Stream *lifecycle* commands therefore
 * can't live here (there's no server to receive them when idle); they're on
 * [SessionServer]'s separate always-reachable port instead.
 *
 * Status codes: `400` malformed request line/headers/body or wrong
 * Content-Type, `401` missing/mismatched token, `404` unknown path, `405`
 * wrong method for a known path, `413` control body over
 * [HttpWire.MAX_BODY_BYTES], `431` headers over [HttpWire.MAX_HEADER_BYTES].
 * Every non-streaming response body, success or error, is JSON. The request
 * reading, token check and response writing are all [HttpWire], shared with
 * [SessionServer].
 */
class MjpegServer(
    val port: Int,
    val getCamerasJson: () -> String,
    val handleControl: (Map<String, String>) -> String,
    val bindAddr: String = "0.0.0.0",
    val token: String?,
) {
    private var serverSocket: ServerSocket? = null
    private val clients = CopyOnWriteArrayList<MjpegClient>()
    private val running = AtomicBoolean(false)

    // Bounds total connections being served at once (streaming + short-lived
    // /v1/state and /v1/control requests) so a peer that opens many partial or
    // slow connections can't exhaust a thread per connection indefinitely.
    private val clientSlots = Semaphore(MAX_CONCURRENT_CLIENTS)

    fun start() {
        running.set(true)
        // Bind via the no-arg constructor + explicit setReuseAddress(true) instead of
        // the ServerSocket(port, backlog, addr) convenience constructor, which binds
        // immediately and gives no chance to set SO_REUSEADDR first. Without it, a
        // quick stop-then-start (the previous MjpegServer's socket just closed, or its
        // last accepted /v1/video connection still winding down into TIME_WAIT) can hit
        // EADDRINUSE on the same port - and since this used to be uncaught, it crashed
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
            socket.soTimeout = HttpWire.READ_TIMEOUT_MS
            HttpWire.sendError(socket.getOutputStream(), 503, "Service Unavailable")
        } catch (_: Exception) {
        } finally {
            try { socket.close() } catch (_: Exception) {}
        }
    }

    // ── HTTP dispatch ───────────────────────────────────────────────────────

    private fun dispatch(socket: Socket) {
        var streaming = false
        try {
            socket.soTimeout = HttpWire.READ_TIMEOUT_MS
            val request = HttpWire.readRequest(socket) ?: return  // already responded/closed on error

            when (request.path) {
                "/v1/state" -> {
                    if (request.method != "GET") { HttpWire.sendError(socket.getOutputStream(), 405, "Method Not Allowed"); return }
                    if (!isAuthorized(request)) { HttpWire.sendError(socket.getOutputStream(), 401, "Unauthorized"); return }
                    HttpWire.sendJson(socket.getOutputStream(), getCamerasJson())
                }
                "/v1/control" -> {
                    if (request.method != "POST") { HttpWire.sendError(socket.getOutputStream(), 405, "Method Not Allowed"); return }
                    if (!isAuthorized(request)) { HttpWire.sendError(socket.getOutputStream(), 401, "Unauthorized"); return }
                    if (!HttpWire.isJsonBody(request)) {
                        HttpWire.sendError(socket.getOutputStream(), 400, "Bad Request"); return
                    }
                    val body = HttpWire.readBody(socket, request) ?: return  // already responded on error
                    val params = HttpWire.parseJsonParams(body)
                    if (params == null) {
                        HttpWire.sendError(socket.getOutputStream(), 400, "Bad Request")
                    } else {
                        HttpWire.sendJson(socket.getOutputStream(), handleControl(params))
                    }
                }
                "/v1/video" -> {
                    if (request.method != "GET") { HttpWire.sendError(socket.getOutputStream(), 405, "Method Not Allowed"); return }
                    if (!isAuthorized(request)) { HttpWire.sendError(socket.getOutputStream(), 401, "Unauthorized"); return }
                    streaming = true
                    val client = MjpegClient(socket)
                    clients.add(client)
                    client.stream()          // blocks until disconnected
                    clients.remove(client)
                }
                else -> HttpWire.sendError(socket.getOutputStream(), 404, "Not Found")
            }
        } catch (_: SocketTimeoutException) {
            // Client opened a connection but never finished sending a request.
        } catch (_: Exception) {
        } finally {
            if (!streaming) try { socket.close() } catch (_: Exception) {}
        }
    }

    private fun isAuthorized(request: HttpWire.Request): Boolean =
        HttpWire.bearerMatches(token, request)

    companion object {
        private const val MAX_CONCURRENT_CLIENTS = 16
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
