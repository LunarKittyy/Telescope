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
    // The first listener to /v1/audio: start the mic, or say why not. The last one leaving: stop it.
    val startAudio: () -> String? = { "No microphone" },
    val stopAudio: () -> Unit = {},
    private val requestDeadlineMs: Int = HttpWire.REQUEST_DEADLINE_MS,
    private val pending: PendingLimiter = PendingLimiter(),
    // TLS in the app (PhoneTls); plain sockets only in tests.
    private val socketFactory: javax.net.ServerSocketFactory = javax.net.ServerSocketFactory.getDefault(),
    // A viewer sent nothing for this long is dropped: with nothing to write it would never notice the computer left.
    private val viewerGiveUpMs: Long = VIEWER_GIVE_UP_MS,
) {
    private var serverSocket: ServerSocket? = null
    private val clients = CopyOnWriteArrayList<MjpegClient>()
    private val h264Clients = CopyOnWriteArrayList<H264Client>()
    @Volatile private var h264Config: ByteArray? = null
    private val audioClients = CopyOnWriteArrayList<AudioClient>()
    private val audioLock = Any()
    private val running = AtomicBoolean(false)

    // Updated on every authorized request; feeds battery-saving watchdog.
    @Volatile private var lastAuthorizedRequestAtMs: Long = System.currentTimeMillis()

    // Bounds concurrent authorized streams; taken only after the token check, so strangers can't fill it.
    private val streamSlots = Semaphore(MAX_CONCURRENT_STREAMS)

    fun start() {
        running.set(true)
        lastAuthorizedRequestAtMs = System.currentTimeMillis()
        // Set SO_REUSEADDR before binding to avoid EADDRINUSE on quick restart.
        serverSocket = socketFactory.createServerSocket().apply {
            reuseAddress = true
            bind(java.net.InetSocketAddress(java.net.InetAddress.getByName(bindAddr), port), 50)
        }
        thread(name = "mjpeg-accept", isDaemon = true) {
            while (running.get()) {
                try {
                    val socket = serverSocket?.accept() ?: break
                    val address = socket.inetAddress?.hostAddress.orEmpty()
                    if (!pending.tryAcquire(address)) {
                        try { socket.close() } catch (_: Exception) {}
                        continue
                    }
                    thread(name = "mjpeg-client", isDaemon = true) { dispatch(socket, address) }
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

    fun sendAudio(chunk: ByteArray) {
        for (c in audioClients) c.queue.offer(chunk)
    }

    // Drop MJPEG viewers (the stream moved to H.264): with no frames to write they'd never notice a closed socket and keep their slot.
    fun closeMjpegClients() {
        clients.forEach { it.close() }
        clients.clear()
    }

    /** Drop H.264 viewers (the encoder is gone); their readers see the end of the stream. */
    fun closeH264Clients() {
        h264Clients.forEach { it.close() }
        h264Clients.clear()
        h264Config = null
    }

    fun stop() {
        running.set(false)
        closeMjpegClients()
        closeH264Clients()
        synchronized(audioLock) {  // so a /v1/audio request finishing now can't start the mic after this
            audioClients.forEach { it.close() }
            audioClients.clear()
        }
        try { serverSocket?.close() } catch (_: Exception) {}
    }

    private fun dispatch(socket: Socket, address: String) {
        var streaming = false
        var pendingHeld = true
        fun releasePending() { if (pendingHeld) { pendingHeld = false; pending.release(address) } }
        // Leaves the pending pool for a stream slot; false (503 sent) when every slot is taken.
        fun takeStreamSlot(): Boolean {
            releasePending()
            if (streamSlots.tryAcquire()) return true
            HttpWire.sendError(socket.getOutputStream(), 503, "Service Unavailable")
            return false
        }
        try {
            val request = HttpWire.readRequest(socket, requestDeadlineMs) ?: return  // already responded/closed on error
            if (!running.get()) return  // stopped while this request was coming in

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
                    if (!takeStreamSlot()) return
                    streaming = true
                    try {
                        val client = MjpegClient(socket)
                        clients.add(client)
                        onVideoClient(H264Stream.CODEC_MJPEG)
                        client.stream()          // blocks until disconnected
                        clients.remove(client)
                    } finally { streamSlots.release() }
                }
                "/v1/video.h264" -> {
                    if (request.method != "GET") { HttpWire.sendError(socket.getOutputStream(), 405, "Method Not Allowed"); return }
                    if (!isAuthorized(request)) { HttpWire.sendError(socket.getOutputStream(), 401, "Unauthorized"); return }
                    if (!takeStreamSlot()) return
                    streaming = true
                    try {
                        val client = H264Client(socket)
                        h264Config?.let { client.queue.offerConfig(it) }
                        h264Clients.add(client)
                        onVideoClient(H264Stream.CODEC_H264)
                        requestKeyFrame()        // a decoder can only start at one
                        client.stream()
                        h264Clients.remove(client)
                    } finally { streamSlots.release() }
                }
                "/v1/audio" -> {
                    if (request.method != "GET") { HttpWire.sendError(socket.getOutputStream(), 405, "Method Not Allowed"); return }
                    if (!isAuthorized(request)) { HttpWire.sendError(socket.getOutputStream(), 401, "Unauthorized"); return }
                    if (!takeStreamSlot()) return
                    try {
                        val client = AudioClient(socket)
                        val problem = synchronized(audioLock) {
                            if (!running.get()) "Not streaming"
                            else (if (audioClients.isEmpty()) startAudio() else null).also { if (it == null) audioClients.add(client) }
                        }
                        if (problem != null) { HttpWire.sendError(socket.getOutputStream(), 403, problem); return }
                        streaming = true
                        try {
                            client.stream()
                        } finally {
                            synchronized(audioLock) {
                                audioClients.remove(client)
                                if (audioClients.isEmpty()) stopAudio()
                            }
                        }
                    } finally { streamSlots.release() }
                }
                else -> HttpWire.sendError(socket.getOutputStream(), 404, "Not Found")
            }
        } catch (_: SocketTimeoutException) {
            // Client opened a connection but never finished sending a request.
        } catch (_: Exception) {
        } finally {
            releasePending()
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

    /**
     * Whether a computer is taking the video right now. A pulled cable or a dropped Wi-Fi link can
     * leave a connection open for minutes, so only a viewer that took a write recently counts.
     */
    fun hasActiveViewer(now: Long = System.currentTimeMillis()): Boolean =
        clients.any { now - it.lastWriteAtMs < VIEWER_STALE_MS } ||
            h264Clients.any { now - it.lastWriteAtMs < VIEWER_STALE_MS }

    private fun pollMs(): Long = minOf(2_000L, viewerGiveUpMs)

    private fun givenUp(lastWriteAtMs: Long): Boolean = System.currentTimeMillis() - lastWriteAtMs >= viewerGiveUpMs

    // A long-lived stream socket. Nagle off: otherwise the last piece of each frame can sit waiting for the
    // computer's ACK, which Windows delays by up to 200 ms.
    private fun openStream(socket: Socket): java.io.OutputStream {
        socket.soTimeout = 0
        socket.tcpNoDelay = true
        return socket.getOutputStream()
    }

    companion object {
        private const val MAX_CONCURRENT_STREAMS = 16
        const val VIEWER_STALE_MS = 5_000L
        const val VIEWER_GIVE_UP_MS = 10_000L
    }

    inner class MjpegClient(private val socket: Socket) {
        private val queue = ArrayBlockingQueue<ByteArray>(2)
        private val alive = AtomicBoolean(true)
        @Volatile var lastWriteAtMs: Long = System.currentTimeMillis()
            private set

        fun stream() {
            try {
                val out = openStream(socket)
                val hdr = "HTTP/1.1 200 OK\r\n" +
                    "Content-Type: multipart/x-mixed-replace; boundary=--mjpegframe\r\n" +
                    "Cache-Control: no-cache\r\nConnection: keep-alive\r\n\r\n"
                out.write(hdr.toByteArray(Charsets.UTF_8))
                out.flush()

                // Each part goes out in one write, so no small piece of it waits on its own packet. The CRLF that
                // ends a part leads the next one's header instead, which puts the same bytes on the wire.
                var wire = ByteArray(0)
                var first = true
                while (alive.get()) {
                    val frame = queue.poll(pollMs(), TimeUnit.MILLISECONDS) ?: if (givenUp(lastWriteAtMs)) break else continue
                    val partHdr = ((if (first) "" else "\r\n") + "--mjpegframe\r\nContent-Type: image/jpeg\r\n" +
                                   "Content-Length: ${frame.size}\r\n\r\n").toByteArray(Charsets.UTF_8)
                    val total = partHdr.size + frame.size
                    if (wire.size < total) wire = ByteArray(total + total / 4)
                    System.arraycopy(partHdr, 0, wire, 0, partHdr.size)
                    System.arraycopy(frame, 0, wire, partHdr.size, frame.size)
                    out.write(wire, 0, total)
                    out.flush()
                    first = false
                    lastWriteAtMs = System.currentTimeMillis()
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
        @Volatile var lastWriteAtMs: Long = System.currentTimeMillis()
            private set

        fun stream() {
            try {
                val out = openStream(socket)
                out.write(("HTTP/1.1 200 OK\r\nContent-Type: video/h264\r\n" +
                    "Cache-Control: no-cache\r\nConnection: close\r\n\r\n").toByteArray(Charsets.UTF_8))
                out.flush()
                while (alive.get()) {
                    val packet = queue.poll(pollMs()) ?: if (givenUp(lastWriteAtMs)) break else continue
                    out.write(packet)
                    out.flush()
                    lastWriteAtMs = System.currentTimeMillis()
                }
            } catch (_: Exception) {}
            finally { alive.set(false); try { socket.close() } catch (_: Exception) {} }
        }

        fun close() { alive.set(false); try { socket.close() } catch (_: Exception) {} }
    }

    inner class AudioClient(private val socket: Socket) {
        val queue = PcmClientQueue()
        private val alive = AtomicBoolean(true)
        private var lastWriteAtMs = System.currentTimeMillis()

        fun stream() {
            try {
                val out = openStream(socket)
                out.write(("HTTP/1.1 200 OK\r\nContent-Type: ${AudioStream.CONTENT_TYPE}\r\n" +
                    "Cache-Control: no-cache\r\nConnection: close\r\n\r\n").toByteArray(Charsets.UTF_8))
                out.flush()
                while (alive.get()) {
                    val chunk = queue.poll(pollMs()) ?: if (givenUp(lastWriteAtMs)) break else continue
                    out.write(chunk)
                    out.flush()
                    lastWriteAtMs = System.currentTimeMillis()
                }
            } catch (_: Exception) {}
            finally { alive.set(false); try { socket.close() } catch (_: Exception) {} }
        }

        fun close() { alive.set(false); try { socket.close() } catch (_: Exception) {} }
    }
}
