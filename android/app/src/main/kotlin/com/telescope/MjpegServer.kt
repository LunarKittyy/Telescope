package com.telescope

import java.io.OutputStream
import java.net.ServerSocket
import java.net.Socket
import java.net.SocketTimeoutException
import java.security.MessageDigest
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
 * constant-time comparison. A null [token] (nothing paired yet) rejects
 * every request with 401.
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
                "/v1/state" -> {
                    if (request.method != "GET") { sendError(socket.getOutputStream(), 400, "Bad Request"); return }
                    if (!isAuthorized(request)) { sendError(socket.getOutputStream(), 401, "Unauthorized"); return }
                    sendJson(socket.getOutputStream(), getCamerasJson())
                }
                "/v1/control" -> {
                    if (request.method != "POST") { sendError(socket.getOutputStream(), 400, "Bad Request"); return }
                    if (!isAuthorized(request)) { sendError(socket.getOutputStream(), 401, "Unauthorized"); return }
                    val body = readBody(socket, request) ?: return  // already responded on error
                    val params = parseControlBody(body)
                    if (params == null) {
                        sendError(socket.getOutputStream(), 400, "Bad Request")
                    } else {
                        sendJson(socket.getOutputStream(), handleControl(params))
                    }
                }
                "/v1/video" -> {
                    if (request.method != "GET") { sendError(socket.getOutputStream(), 400, "Bad Request"); return }
                    if (!isAuthorized(request)) { sendError(socket.getOutputStream(), 401, "Unauthorized"); return }
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

    /** Constant-time bearer-token check. A null [token] (nothing paired yet)
     *  always fails closed. */
    private fun isAuthorized(request: Request): Boolean {
        val expected = token ?: return false
        val header = request.headers["authorization"] ?: return false
        if (!header.startsWith(BEARER_PREFIX)) return false
        val provided = header.substring(BEARER_PREFIX.length)
        return MessageDigest.isEqual(
            expected.toByteArray(Charsets.UTF_8),
            provided.toByteArray(Charsets.UTF_8),
        )
    }

    private data class Request(
        val method: String,
        val path: String,
        val query: String,
        val headers: Map<String, String>,
        val leftoverBody: ByteArray,
    )

    /**
     * Reads and parses the request line and headers, bounded by [MAX_HEADER_BYTES]
     * so a client that never sends a terminator can't hold a thread's read buffer
     * open indefinitely. Only `GET`/`POST` are accepted. Returns null (having
     * already written an error response and closed the socket) on any parse
     * failure. Any body bytes already read past the header terminator while
     * filling the read buffer are preserved in [Request.leftoverBody] for
     * [readBody] to prepend.
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
        val raw = sb.toString()
        val term = if (raw.contains("\r\n\r\n")) "\r\n\r\n" else "\n\n"
        val termIdx = raw.indexOf(term)
        val headerPart = raw.substring(0, termIdx)
        val leftover = raw.substring(termIdx + term.length)

        val lines = headerPart.split("\r\n", "\n")
        val requestLine = lines.firstOrNull() ?: ""
        val parts = requestLine.split(" ")
        if (parts.size < 2 || (parts[0] != "GET" && parts[0] != "POST")) {
            sendError(socket.getOutputStream(), 400, "Bad Request")
            socket.close()
            return null
        }
        val method = parts[0]
        val fullPath = parts[1]
        val path = fullPath.substringBefore("?")
        val query = fullPath.substringAfter("?", "")

        val headers = mutableMapOf<String, String>()
        for (line in lines.drop(1)) {
            if (line.isBlank()) continue
            val idx = line.indexOf(':')
            if (idx <= 0) continue
            headers[line.substring(0, idx).trim().lowercase()] = line.substring(idx + 1).trim()
        }

        return Request(method, path, query, headers, leftover.toByteArray(Charsets.ISO_8859_1))
    }

    /** Reads exactly `Content-Length` bytes of body, bounded by [MAX_BODY_BYTES].
     *  Returns null (having already written an error response) on any failure. */
    private fun readBody(socket: Socket, request: Request): ByteArray? {
        val length = request.headers["content-length"]?.toIntOrNull()
        if (length == null || length < 0) {
            sendError(socket.getOutputStream(), 400, "Bad Request")
            return null
        }
        if (length > MAX_BODY_BYTES) {
            sendError(socket.getOutputStream(), 413, "Payload Too Large")
            return null
        }
        val out = ByteArray(length)
        val fromLeftover = minOf(request.leftoverBody.size, length)
        System.arraycopy(request.leftoverBody, 0, out, 0, fromLeftover)
        var read = fromLeftover
        val inp = socket.getInputStream()
        while (read < length) {
            val n = inp.read(out, read, length - read)
            if (n <= 0) { sendError(socket.getOutputStream(), 400, "Bad Request"); return null }
            read += n
        }
        return out
    }

    /**
     * Parses a flat JSON object body (`{"key": "string"|number|true|false|null, ...}`,
     * no nesting) into the same string-keyed param map the control handler
     * already expects. Deliberately hand-rolled rather than using org.json:
     * that class is an unmocked Android SDK stub under this module's plain-
     * JVM unit test setup (it throws unconditionally outside a real device/
     * Robolectric), which this class is specifically designed to avoid - see
     * the class doc. Returns null on any malformed input.
     */
    private fun parseControlBody(body: ByteArray): Map<String, String>? {
        return try {
            parseFlatJsonObject(String(body, Charsets.UTF_8))
        } catch (_: Exception) {
            null
        }
    }

    private fun parseFlatJsonObject(text: String): Map<String, String> {
        var i = 0
        fun skipWs() { while (i < text.length && text[i].isWhitespace()) i++ }
        fun expect(c: Char) {
            require(i < text.length && text[i] == c) { "expected '$c' at $i" }
            i++
        }
        fun parseString(): String {
            expect('"')
            val sb = StringBuilder()
            while (true) {
                require(i < text.length) { "unterminated string" }
                val c = text[i]
                when {
                    c == '"' -> { i++; return sb.toString() }
                    c == '\\' -> {
                        i++
                        require(i < text.length) { "bad escape" }
                        when (val esc = text[i]) {
                            '"', '\\', '/' -> sb.append(esc)
                            'n' -> sb.append('\n')
                            't' -> sb.append('\t')
                            'r' -> sb.append('\r')
                            'b' -> sb.append('\b')
                            'u' -> {
                                require(i + 4 < text.length) { "bad unicode escape" }
                                sb.append(text.substring(i + 1, i + 5).toInt(16).toChar())
                                i += 4
                            }
                            else -> throw IllegalArgumentException("bad escape $esc")
                        }
                        i++
                    }
                    else -> { sb.append(c); i++ }
                }
            }
        }
        fun parseValue(): String {
            skipWs()
            require(i < text.length) { "unexpected end" }
            if (text[i] == '"') return parseString()
            val start = i
            while (i < text.length && text[i] != ',' && text[i] != '}') i++
            val literal = text.substring(start, i).trim()
            require(literal.isNotEmpty()) { "empty value" }
            return literal
        }

        skipWs()
        expect('{')
        val result = mutableMapOf<String, String>()
        skipWs()
        if (i < text.length && text[i] == '}') {
            i++
        } else {
            while (true) {
                skipWs()
                val key = parseString()
                skipWs()
                expect(':')
                result[key] = parseValue()
                skipWs()
                require(i < text.length) { "unterminated object" }
                when (text[i]) {
                    ',' -> { i++ }
                    '}' -> { i++; skipWs(); require(i == text.length) { "trailing data" }; return result }
                    else -> throw IllegalArgumentException("expected , or }")
                }
            }
        }
        skipWs()
        require(i == text.length) { "trailing data" }
        return result
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

    companion object {
        private const val MAX_CONCURRENT_CLIENTS = 16
        private const val MAX_HEADER_BYTES = 16 * 1024
        private const val MAX_BODY_BYTES = 4 * 1024
        private const val READ_TIMEOUT_MS = 5_000
        private const val BEARER_PREFIX = "Bearer "
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
