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

// Serves GET /v1/video (MJPEG), GET /v1/video.h264 (Annex-B), GET /v1/state, POST /v1/control;
// all require bearer token. Opening a video route tells the service which codec to run.
class MjpegServer(
    val port: Int,
    val getCamerasJson: () -> String,
    val handleControl: (Map<String, String>) -> String,
    val bindAddr: String = "0.0.0.0",
    // Read on every request, so pairing or unpairing a computer applies without restarting the stream.
    val tokens: () -> List<String>,
    // A viewer connected to the route for this codec (H264Stream.CODEC_*).
    val onVideoClient: (codec: String) -> Unit = {},
    val requestKeyFrame: () -> Unit = {},
) {
    private var serverSocket: ServerSocket? = null
    private val clients = CopyOnWriteArrayList<MjpegClient>()
    private val h264Clients = CopyOnWriteArrayList<H264Client>()
    @Volatile private var h264Config: ByteArray? = null
    private val running = AtomicBoolean(false)

    // Updated on every authorized request; feeds battery-saving watchdog.
    @Volatile private var lastAuthorizedRequestAtMs: Long = System.currentTimeMillis()

    // Bounds total concurrent connections; prevents thread exhaustion from slow peers.
    private val clientSlots = Semaphore(MAX_CONCURRENT_CLIENTS)

    fun start() {
        running.set(true)
        lastAuthorizedRequestAtMs = System.currentTimeMillis()
        // Set SO_REUSEADDR before binding to avoid EADDRINUSE on quick restart.
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

    fun sendH264(packet: ByteArray, key: Boolean, config: Boolean) {
        if (config) {
            h264Config = packet
            h264Clients.forEach { it.queue.offerConfig(packet) }
            return
        }
        if (h264Clients.isEmpty()) return
        val framed = H264Stream.withDelimiter(packet)
        var wantKey = false
        for (c in h264Clients) { if (c.queue.offer(framed, key)) wantKey = true }
        if (wantKey) requestKeyFrame()
    }

    /** Drop H.264 viewers (the encoder is gone); their readers see the end of the stream. */
    fun closeH264Clients() {
        h264Clients.forEach { it.close() }
        h264Clients.clear()
        h264Config = null
    }

    fun stop() {
        running.set(false)
        clients.forEach { it.close() }
        clients.clear()
        closeH264Clients()
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
                    onVideoClient(H264Stream.CODEC_MJPEG)
                    client.stream()          // blocks until disconnected
                    clients.remove(client)
                }
                "/v1/video.h264" -> {
                    if (request.method != "GET") { HttpWire.sendError(socket.getOutputStream(), 405, "Method Not Allowed"); return }
                    if (!isAuthorized(request)) { HttpWire.sendError(socket.getOutputStream(), 401, "Unauthorized"); return }
                    streaming = true
                    val client = H264Client(socket)
                    h264Config?.let { client.queue.offerConfig(it) }
                    h264Clients.add(client)
                    onVideoClient(H264Stream.CODEC_H264)
                    requestKeyFrame()        // a decoder can only start at one
                    client.stream()
                    h264Clients.remove(client)
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

    private fun isAuthorized(request: HttpWire.Request): Boolean {
        val ok = HttpWire.bearerMatchesAny(tokens(), request)
        if (ok) lastAuthorizedRequestAtMs = System.currentTimeMillis()
        return ok
    }

    /** Milliseconds since the last request that passed token auth. */
    fun idleForMs(): Long = System.currentTimeMillis() - lastAuthorizedRequestAtMs

    companion object {
        private const val MAX_CONCURRENT_CLIENTS = 16
    }

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

    inner class H264Client(private val socket: Socket) {
        val queue = H264ClientQueue()
        private val alive = AtomicBoolean(true)

        fun stream() {
            try {
                socket.soTimeout = 0
                val out = socket.getOutputStream()
                out.write(("HTTP/1.1 200 OK\r\nContent-Type: video/h264\r\n" +
                    "Cache-Control: no-cache\r\nConnection: close\r\n\r\n").toByteArray(Charsets.UTF_8))
                out.flush()
                while (alive.get()) {
                    val packet = queue.poll(2_000L) ?: continue
                    out.write(packet)
                    out.flush()
                }
            } catch (_: Exception) {}
            finally { alive.set(false); try { socket.close() } catch (_: Exception) {} }
        }

        fun close() { alive.set(false); try { socket.close() } catch (_: Exception) {} }
    }
}
