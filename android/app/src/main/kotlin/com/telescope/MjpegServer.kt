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

// Serves GET /v1/video (MJPEG), GET /v1/video.h264 (Annex-B), GET /v1/audio, GET /v1/sensors, GET /v1/state,
// POST /v1/control, and photos on /v1/still and /v1/still/file; all require bearer token. Opening a video route tells
// the service which codec to run.
class MjpegServer(
    val port: Int,
    val getCamerasJson: () -> String,
    val handleControl: (Map<String, String>) -> String,
    val bindAddr: String = "0.0.0.0",
    // Read on every request, so pairing or unpairing a computer applies without restarting the stream.
    val tokens: () -> List<String>,
    // A viewer connected to the route for this codec (H264Stream.CODEC_*), with the token it came with.
    val onVideoClient: (codec: String, token: String?, usb: Boolean) -> Unit = { _, _, _ -> },
    val requestKeyFrame: () -> Unit = {},
    // Every LINK_SAMPLE_MS while H.264 goes out: how the link to the slowest viewer is doing (DynamicBitrate).
    val onH264Link: (DynamicBitrate.Sample) -> Unit = {},
    // The bitrate Dynamic set, 0 when it's off: UDP's pacing follows it (UdpVideoClient.paceFor)
    val dynamicBitrate: () -> Int = { 0 },
    // Whether Dynamic found where the link fills up: UDP's pacing then stays near the link's speed
    val dynamicFull: () -> Boolean = { false },
    // The first listener to /v1/audio: start the mic, or say why not. The last one leaving: stop it.
    val startAudio: () -> MicProblem? = { MicProblem.NO_MIC },
    val stopAudio: () -> Unit = {},
    // What /v1/sensors streams; null answers it 404 like a phone from before it.
    val sensors: SensorFeed? = null,
    private val requestDeadlineMs: Int = HttpWire.REQUEST_DEADLINE_MS,
    private val pending: PendingLimiter = PendingLimiter(),
    // TLS in the app (PhoneTls); plain sockets only in tests.
    private val socketFactory: javax.net.ServerSocketFactory = javax.net.ServerSocketFactory.getDefault(),
    // A viewer sent nothing for this long is dropped: with nothing to write it would never notice the computer left.
    private val viewerGiveUpMs: Long = VIEWER_GIVE_UP_MS,
    private val onWifiViewersChanged: (Boolean) -> Unit = {},
    // A sensor stream whose writes have been stuck this long has lost its computer: the sensors stop
    private val sensorStallMs: Long = SENSOR_STALL_MS,
    // And so has one whose computer sent no request at all for this long (it asks /v1/sensors/alive meanwhile)
    private val sensorAliveMs: Long = SENSOR_ALIVE_MS,
    // POST /v1/still: start a photo (or forget one); the JSON answer
    val handleStill: (Map<String, String>) -> String = { """{"ok":false,"error":"no photos"}""" },
    // GET /v1/still?id=: how that photo is getting on, as JSON; null for an id it doesn't know
    val stillStatus: (String) -> String? = { null },
    // GET /v1/still/file?id=&n=: the file and the name to send it as; null when there's no such file
    val stillFile: (String, Int) -> Pair<java.io.File, String>? = { _, _ -> null },
    // A file download that makes no progress for this long is cut off
    private val fileTimeoutMs: Int = FILE_TIMEOUT_MS,
) {
    private var serverSocket: ServerSocket? = null
    private val clients = CopyOnWriteArrayList<MjpegClient>()
    private val h264Clients = CopyOnWriteArrayList<H264Client>()
    private val udpClients = CopyOnWriteArrayList<UdpVideoClient>()
    @Volatile private var h264Config: ByteArray? = null
    // Dynamic bitrate: short socket buffers and a short backlog, so a full link shows up as a queue here right away.
    @Volatile var dynamicH264 = false
    private var encodedBytes = 0L         // since the last link sample that went to Dynamic; the encoder's thread only
    private var rateBytes = 0L            // since the last link sample of any kind, for encodedBps
    private var encodedBps = 0.0          // smoothed, for sizing Dynamic's socket buffers
    private var lastLinkSampleMs = 0L
    private val audioClients = CopyOnWriteArrayList<AudioClient>()
    private val audioLock = Any()
    private val sensorClients = CopyOnWriteArrayList<SensorClient>()
    private val lastSeenByToken = java.util.concurrent.ConcurrentHashMap<String, Long>()  // token: its last authorized request
    private val running = AtomicBoolean(false)
    private val wifiViewers = mutableSetOf<Socket>()

    private fun trackViewer(socket: Socket, watching: Boolean) = synchronized(wifiViewers) {
        if (socket.inetAddress.isLoopbackAddress) return@synchronized
        val was = wifiViewers.isNotEmpty()
        if (watching) wifiViewers.add(socket) else wifiViewers.remove(socket)
        if (was != wifiViewers.isNotEmpty()) onWifiViewersChanged(wifiViewers.isNotEmpty())
    }

    // A photo is being taken: a long exposure or a paused video sends no frames, which mustn't count as a viewer gone
    @Volatile private var holding = false
    @Volatile private var heldUntilMs = 0L

    fun holdViewers(on: Boolean) {
        if (holding && !on) heldUntilMs = System.currentTimeMillis()
        holding = on
    }

    // Updated on every authorized request; feeds battery-saving watchdog.
    @Volatile private var lastAuthorizedRequestAtMs: Long = System.currentTimeMillis()

    // Bounds concurrent authorized streams; taken only after the token check, so strangers can't fill it.
    private val streamSlots = Semaphore(MAX_CONCURRENT_STREAMS)

    fun start() {
        running.set(true)
        lastAuthorizedRequestAtMs = System.currentTimeMillis()
        // Set SO_REUSEADDR before binding to avoid EADDRINUSE on quick restart.
        val socket = socketFactory.createServerSocket()
        try {
            socket.reuseAddress = true
            socket.bind(java.net.InetSocketAddress(java.net.InetAddress.getByName(bindAddr), port), 50)
        } catch (e: Exception) {
            socket.close()
            running.set(false)
            throw e
        }
        serverSocket = socket
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
                    if (!running.get()) break
                    android.util.Log.e("MjpegServer", "Accept error", e)
                    if (serverSocket?.isClosed != false) break
                    Thread.sleep(ACCEPT_RETRY_MS)  // out of files, say: don't spin
                }
            }
        }
    }

    // Frames the camera made for the stream lately, either codec: the computer compares what arrived with it.
    val frameRate = FrameRate()

    fun sendFrame(jpeg: ByteArray) {
        frameRate.tick()
        val dead = mutableListOf<MjpegClient>()
        for (c in clients) { if (!c.enqueue(jpeg)) dead.add(c) else if (givenUp(c.lastWriteAtMs)) { c.close(); dead.add(c) } }
        if (dead.isNotEmpty()) clients.removeAll(dead.toSet())
    }

    fun sendH264(packet: ByteArray, key: Boolean, config: Boolean) {
        if (config) {
            h264Config = packet
            h264Clients.forEach { it.queue.offerConfig(packet) }
            return
        }
        frameRate.tick()
        udpClients.forEach { it.offer(packet, key) }
        if (h264Clients.isEmpty() && udpClients.isEmpty()) return
        val framed = H264Stream.withDelimiter(packet)
        var wantKey = false
        for (c in h264Clients) {
            if (givenUp(c.lastWriteAtMs)) { c.close(); continue }
            if (c.queue.offer(framed, key)) wantKey = true
        }
        if (wantKey) requestKeyFrame()
        encodedBytes += framed.size
        rateBytes += framed.size
        sampleH264Link()
    }

    private fun sampleH264Link() {
        val now = System.currentTimeMillis()
        if (now - lastLinkSampleMs < LINK_SAMPLE_MS) return
        val first = lastLinkSampleMs == 0L
        // Counted apart from encodedBytes, which a late report carries over to the next sample, over a span that doesn't
        if (!first) rateBytes.times(8_000.0 / (now - lastLinkSampleMs)).let { encodedBps = if (encodedBps == 0.0) it else 0.8 * encodedBps + 0.2 * it }
        rateBytes = 0
        lastLinkSampleMs = now
        var queueMs = 0L
        var sent = Long.MAX_VALUE
        var loss = 0.0
        var routerMs = 0L
        var lossLately = 0.0
        for (c in h264Clients) {
            c.limitBuffers(if (dynamicH264) dynamicSendBuffer(encodedBps) else 0)
            queueMs = maxOf(queueMs, c.queue.takeQueueMs())
            sent = minOf(sent, c.queue.takeSentBytes())
        }
        // A UDP desktop's reports say how its link did; one that sends none (an older desktop) can't be measured
        for (u in udpClients) {
            u.limitBuffer(if (dynamicH264) dynamicSendBuffer(encodedBps) else 0)
            u.paceBps = if (dynamicH264) UdpVideoClient.paceFor(encodedBps, dynamicBitrate(), if (dynamicFull()) u.peakBps() else 0.0) else 0
            val link = u.takeLink() ?: continue
            queueMs = maxOf(queueMs, link.queueMs)
            sent = minOf(sent, link.sentBytes)
            loss = maxOf(loss, link.loss)
            routerMs = maxOf(routerMs, link.routerMs)
            lossLately = maxOf(lossLately, link.lossLately)
        }
        val encoded = encodedBytes
        // A report that's late (they come every 100 ms) leaves this sample out: its bytes count towards the next one
        if (sent == Long.MAX_VALUE && !first && udpClients.any { it.reporting }) return
        encodedBytes = 0
        if (sent == Long.MAX_VALUE || first) return  // the first one only starts the count
        onH264Link(DynamicBitrate.Sample(now, queueMs, sent, encoded, loss, routerMs, lossLately))
    }

    fun sendAudio(chunk: ByteArray) {
        for (c in audioClients) { if (givenUp(c.lastWriteAtMs)) c.close() else c.queue.offer(chunk) }
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
        udpClients.forEach { it.close() }
        udpClients.clear()
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
        sensorClients.forEach { it.close() }
        sensorClients.clear()
        try { serverSocket?.close() } catch (_: Exception) {}
    }

    private fun dispatch(socket: Socket, address: String) {
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
                    val body = HttpWire.readBody(socket, request, HttpWire.MAX_CONTROL_BODY_BYTES) ?: return  // already responded on error
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
                    trackViewer(socket, true)
                    try {
                        val client = MjpegClient(socket, request)
                        clients.add(client)
                        onVideoClient(H264Stream.CODEC_MJPEG, HttpWire.bearerToken(request), socket.inetAddress.isLoopbackAddress)
                        client.stream()          // blocks until disconnected
                        clients.remove(client)
                    } finally { streamSlots.release() }
                }
                "/v1/video.h264" -> {
                    if (request.method != "GET") { HttpWire.sendError(socket.getOutputStream(), 405, "Method Not Allowed"); return }
                    if (!isAuthorized(request)) { HttpWire.sendError(socket.getOutputStream(), 401, "Unauthorized"); return }
                    if (!takeStreamSlot()) return
                    trackViewer(socket, true)
                    try {
                        val client = H264Client(socket, request)
                        h264Config?.let { client.queue.offerConfig(it) }
                        h264Clients.add(client)
                        onVideoClient(H264Stream.CODEC_H264, HttpWire.bearerToken(request), socket.inetAddress.isLoopbackAddress)
                        requestKeyFrame()        // a decoder can only start at one
                        client.stream()
                        h264Clients.remove(client)
                    } finally { streamSlots.release() }
                }
                "/v1/video.udp" -> {
                    if (request.method != "GET") { HttpWire.sendError(socket.getOutputStream(), 405, "Method Not Allowed"); return }
                    if (!isAuthorized(request)) { HttpWire.sendError(socket.getOutputStream(), 401, "Unauthorized"); return }
                    val port = udpPort(request.query)
                    if (port == null) { HttpWire.sendError(socket.getOutputStream(), 400, "Bad Request"); return }
                    // Over USB the desktop is adb's forward on the phone itself: UDP can't get there
                    if (socket.inetAddress.isLoopbackAddress) { HttpWire.sendError(socket.getOutputStream(), 409, "UDP needs Wi-Fi"); return }
                    if (!takeStreamSlot()) return
                    trackViewer(socket, true)
                    try {
                        val client = UdpVideoClient(socket, java.net.InetSocketAddress(socket.inetAddress, port),
                            stillAuthorized = { running.get() && stillPaired(request) }, requestKeyFrame = requestKeyFrame)
                        try {
                            socket.getOutputStream().apply {
                                write(("HTTP/1.1 200 OK\r\nContent-Type: application/x-telescope-udp\r\n" +
                                    "Cache-Control: no-store\r\n\r\n" + client.hello() + "\n").toByteArray())
                                flush()
                            }
                            udpClients.add(client)
                            onVideoClient(H264Stream.CODEC_H264, HttpWire.bearerToken(request), socket.inetAddress.isLoopbackAddress)
                            requestKeyFrame()  // a decoder can only start at one
                            client.run { h264Config }
                        } finally { udpClients.remove(client); client.close() }
                    } finally { streamSlots.release() }
                }
                "/v1/audio" -> {
                    if (request.method != "GET") { HttpWire.sendError(socket.getOutputStream(), 405, "Method Not Allowed"); return }
                    if (!isAuthorized(request)) { HttpWire.sendError(socket.getOutputStream(), 401, "Unauthorized"); return }
                    if (!takeStreamSlot()) return
                    try {
                        val client = AudioClient(socket, request)
                        val problem = synchronized(audioLock) {
                            if (!running.get()) MicProblem.NOT_STREAMING
                            else (if (audioClients.isEmpty()) startAudio() else null).also { if (it == null) audioClients.add(client) }
                        }
                        if (problem != null) { HttpWire.sendError(socket.getOutputStream(), 403, problem.text, problem.code); return }
                        trackViewer(socket, true)
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
                "/v1/sensors/alive" -> {
                    if (sensors == null) { HttpWire.sendError(socket.getOutputStream(), 404, "Not Found"); return }
                    if (request.method != "GET") { HttpWire.sendError(socket.getOutputStream(), 405, "Method Not Allowed"); return }
                    if (!isAuthorized(request)) { HttpWire.sendError(socket.getOutputStream(), 401, "Unauthorized"); return }
                    HttpWire.sendJson(socket.getOutputStream(), "{\"ok\":true}")
                }
                "/v1/sensors" -> {
                    val feed = sensors
                    if (feed == null) { HttpWire.sendError(socket.getOutputStream(), 404, "Not Found"); return }
                    if (request.method != "GET") { HttpWire.sendError(socket.getOutputStream(), 405, "Method Not Allowed"); return }
                    if (!isAuthorized(request)) { HttpWire.sendError(socket.getOutputStream(), 401, "Unauthorized"); return }
                    if (!takeStreamSlot()) return
                    try {
                        val handle = feed.start(SensorKinds.asked(request.query))
                        try {
                            if (handle.kinds.isEmpty()) {
                                HttpWire.sendError(socket.getOutputStream(), 409, "This phone has none of those sensors", "sensors_none")
                                return
                            }
                            val client = SensorClient(socket, request, feed, handle.kinds)
                            sensorClients.add(client)
                            // Stopped meanwhile: stop() may have gone through the list before this one was in it
                            if (!running.get()) client.close()
                            client.stream()
                            sensorClients.remove(client)
                        } finally { feed.stop(handle) }
                    } finally { streamSlots.release() }
                }
                "/v1/still" -> {
                    if (!isAuthorized(request)) { HttpWire.sendError(socket.getOutputStream(), 401, "Unauthorized"); return }
                    when (request.method) {
                        "GET" -> {
                            val status = queryParam(request.query, "id")?.let(stillStatus)
                            if (status == null) HttpWire.sendError(socket.getOutputStream(), 404, "Not Found")
                            else HttpWire.sendJson(socket.getOutputStream(), status)
                        }
                        "POST" -> {
                            if (!HttpWire.isJsonBody(request)) { HttpWire.sendError(socket.getOutputStream(), 400, "Bad Request"); return }
                            val body = HttpWire.readBody(socket, request) ?: return
                            val params = HttpWire.parseJsonParams(body)
                            if (params == null) HttpWire.sendError(socket.getOutputStream(), 400, "Bad Request")
                            else HttpWire.sendJson(socket.getOutputStream(), handleStill(params))
                        }
                        else -> HttpWire.sendError(socket.getOutputStream(), 405, "Method Not Allowed")
                    }
                }
                "/v1/still/file" -> {
                    if (request.method != "GET") { HttpWire.sendError(socket.getOutputStream(), 405, "Method Not Allowed"); return }
                    if (!isAuthorized(request)) { HttpWire.sendError(socket.getOutputStream(), 401, "Unauthorized"); return }
                    val id = queryParam(request.query, "id")
                    val n = queryParam(request.query, "n")?.toIntOrNull()
                    val found = if (id != null && n != null) stillFile(id, n) else null
                    if (found == null) { HttpWire.sendError(socket.getOutputStream(), 404, "Not Found"); return }
                    if (!takeStreamSlot()) return
                    try { sendFile(socket, found.first, found.second) } finally { streamSlots.release() }
                }
                else -> HttpWire.sendError(socket.getOutputStream(), 404, "Not Found")
            }
        } catch (_: SocketTimeoutException) {
            // Client opened a connection but never finished sending a request.
        } catch (_: Exception) {
        } finally {
            releasePending()
            trackViewer(socket, false)
            try { socket.close() } catch (_: Exception) {}
        }
    }

    private fun isAuthorized(request: HttpWire.Request): Boolean {
        val ok = HttpWire.bearerMatchesAny(tokens(), request)
        if (ok) {
            lastAuthorizedRequestAtMs = System.currentTimeMillis()
            HttpWire.bearerToken(request)?.let { lastSeenByToken[it] = lastAuthorizedRequestAtMs }
        }
        return ok
    }

    /** Milliseconds since the last request that passed token auth. */
    fun idleForMs(): Long = System.currentTimeMillis() - lastAuthorizedRequestAtMs

    // A computer taking the video or the mic right now; only a recent write counts, as a dead link can stay open for minutes.
    fun hasActiveViewer(now: Long = System.currentTimeMillis()): Boolean =
        clients.any { now - it.lastWriteAtMs < VIEWER_STALE_MS } ||
            h264Clients.any { now - it.lastWriteAtMs < VIEWER_STALE_MS } ||
            udpClients.any { it.hasActiveViewer() } ||
            audioClients.any { now - it.lastWriteAtMs < VIEWER_STALE_MS }

    private fun pollMs(): Long = minOf(2_000L, viewerGiveUpMs)

    // A photo, which can be 50 MB and more: streamed in TLS-record-sized pieces with its length up front
    private fun sendFile(socket: Socket, file: java.io.File, name: String) {
        socket.soTimeout = fileTimeoutMs
        val out = socket.getOutputStream()
        // A write blocked on a computer that went away never times out on its own, so the socket is closed instead
        val lastWrite = java.util.concurrent.atomic.AtomicLong(System.nanoTime())
        val guard = fileGuards.scheduleWithFixedDelay({
            if (System.nanoTime() - lastWrite.get() > fileTimeoutMs * 1_000_000L) try { socket.close() } catch (_: Exception) {}
        }, fileTimeoutMs / 2L, fileTimeoutMs / 2L, TimeUnit.MILLISECONDS)
        try { sendFileBody(file, name, out) { lastWrite.set(System.nanoTime()) } } finally { guard.cancel(false) }
    }

    private fun sendFileBody(file: java.io.File, name: String, out: java.io.OutputStream, wrote: () -> Unit) {
        file.inputStream().use { input ->
            val length = file.length()
            out.write(("HTTP/1.1 200 OK\r\nContent-Type: ${StillJobs.type(name)}\r\nContent-Length: $length\r\n" +
                "Content-Disposition: attachment; filename=\"$name\"\r\nCache-Control: no-store\r\n" +
                "Connection: close\r\n\r\n").toByteArray(Charsets.UTF_8))
            val buf = ByteArray(WRITE_CHUNK)
            var left = length
            while (left > 0) {
                val n = input.read(buf, 0, minOf(buf.size.toLong(), left).toInt())
                if (n <= 0) break
                out.write(buf, 0, n)
                wrote()
                left -= n
            }
            out.flush()
        }
    }

    // Checked again while a viewer streams, so removing its computer on the phone cuts off video and mic it already has
    private fun stillPaired(request: HttpWire.Request): Boolean = HttpWire.bearerMatchesAny(tokens(), request)

    // Also checked by the senders: a viewer whose write is blocked on a dead link never polls, and closing unblocks it
    private fun givenUp(lastWriteAtMs: Long): Boolean =
        !holding && System.currentTimeMillis() - maxOf(lastWriteAtMs, heldUntilMs) >= viewerGiveUpMs

    // A long-lived stream socket. Nagle off: otherwise the last piece of each frame can sit waiting for the
    // computer's ACK, which Windows delays by up to 200 ms.
    private fun openStream(socket: Socket): java.io.OutputStream {
        socket.soTimeout = 0
        socket.tcpNoDelay = true
        return socket.getOutputStream()
    }

    companion object {
        // The port=N a /v1/video.udp request names for the desktop's UDP socket, or null
        fun udpPort(query: String): Int? = query.split('&').firstNotNullOfOrNull { part ->
            part.removePrefix("port=").takeIf { part.startsWith("port=") }?.toIntOrNull()?.takeIf { it in 1..65535 }
        }

        // name=value from a query string, or null
        fun queryParam(query: String, name: String): String? = query.split('&').firstNotNullOfOrNull { part ->
            part.removePrefix("$name=").takeIf { part.startsWith("$name=") }
        }

        private const val MAX_CONCURRENT_STREAMS = 16
        const val FILE_TIMEOUT_MS = 30_000
        private val fileGuards = java.util.concurrent.Executors.newSingleThreadScheduledExecutor { r ->
            Thread(r, "still-file-guard").apply { isDaemon = true }
        }
        const val VIEWER_STALE_MS = 5_000L
        const val VIEWER_GIVE_UP_MS = 10_000L
        const val ACCEPT_RETRY_MS = 200L
        const val LINK_SAMPLE_MS = 250L
        const val WRITE_CHUNK = 16 * 1024  // a TLS record's worth
        const val DYNAMIC_MAX_WAIT_MS = 1_000L
        // About 100 ms of video once the kernel doubles it: enough for Wi-Fi's round trips, little to hide in.
        fun dynamicSendBuffer(bps: Double): Int = (bps / 8 * 0.05).toInt().coerceIn(8 * 1024, 640 * 1024)
        const val UNLIMITED_SEND_BUFFER = 1 shl 20    // back from Dynamic; the kernel caps it at its own maximum
        const val SENSOR_TICK_MS = 33L          // about 30 lines a second at most
        const val SENSOR_LIGHT_EVERY_MS = 500L
        const val SENSOR_HEARTBEAT_MS = 1_000L  // a line even when nothing changed, so a computer that's gone shows up as a failed write
        const val SENSOR_STALL_MS = 5_000L
        const val SENSOR_ALIVE_MS = 15_000L
        // A few seconds of lines: a computer that vanished without closing (Wi-Fi gone, laptop asleep) stops taking
        // them and the writes get stuck soon after, instead of filling a big buffer for minutes
        const val SENSOR_SEND_BUFFER = 8 * 1024
    }

    inner class MjpegClient(private val socket: Socket, private val request: HttpWire.Request) {
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
                while (alive.get() && stillPaired(request)) {
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

    inner class H264Client(private val socket: Socket, private val request: HttpWire.Request) {
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
                while (alive.get() && stillPaired(request)) {
                    val packet = queue.poll(pollMs()) ?: if (givenUp(lastWriteAtMs)) break else continue
                    // In pieces, so a big keyframe on a slow link still shows as bytes going out, not as a stall.
                    var off = 0
                    while (off < packet.size) {
                        val n = minOf(WRITE_CHUNK, packet.size - off)
                        out.write(packet, off, n)
                        off += n
                        queue.written(n, done = off == packet.size)
                    }
                    out.flush()
                    lastWriteAtMs = System.currentTimeMillis()
                }
            } catch (_: Exception) {}
            finally { alive.set(false); try { socket.close() } catch (_: Exception) {} }
        }

        // The kernel's send buffer would otherwise grow to seconds of video before a write ever waits. 0 = no limit.
        private var sendBuffer = 0
        fun limitBuffers(bytes: Int) {
            // Resized only on a real change: a keyframe's worth of rate shouldn't mean a syscall every sample.
            if (bytes == sendBuffer || (bytes > 0 && sendBuffer > 0 && bytes in sendBuffer * 4 / 5..sendBuffer * 5 / 4)) return
            sendBuffer = bytes
            queue.maxWaitMs = if (bytes > 0) DYNAMIC_MAX_WAIT_MS else Long.MAX_VALUE
            try { socket.sendBufferSize = if (bytes > 0) bytes else UNLIMITED_SEND_BUFFER } catch (_: Exception) {}
        }

        fun close() { alive.set(false); try { socket.close() } catch (_: Exception) {} }
    }

    inner class AudioClient(private val socket: Socket, private val request: HttpWire.Request) {
        val queue = PcmClientQueue()
        private val alive = AtomicBoolean(true)
        @Volatile var lastWriteAtMs: Long = System.currentTimeMillis()
            private set

        fun stream() {
            try {
                val out = openStream(socket)
                out.write(("HTTP/1.1 200 OK\r\nContent-Type: ${AudioStream.CONTENT_TYPE}\r\n" +
                    "Cache-Control: no-cache\r\nConnection: close\r\n\r\n").toByteArray(Charsets.UTF_8))
                out.flush()
                while (alive.get() && stillPaired(request)) {
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

    // Writes the newest readings of the kinds it started, only the ones that changed, until the computer goes away.
    inner class SensorClient(private val socket: Socket, private val request: HttpWire.Request,
                             private val feed: SensorFeed, private val kinds: Set<String>) {
        private val alive = AtomicBoolean(true)
        @Volatile private var writingSinceMs = 0L  // 0 while not in a write

        fun stream() {
            try {
                val out = openStream(socket)
                try { socket.sendBufferSize = SENSOR_SEND_BUFFER } catch (_: Exception) {}
                thread(name = "sensor-watch", isDaemon = true) { watch() }
                write(out, "HTTP/1.1 200 OK\r\nContent-Type: ${SensorWire.CONTENT_TYPE}\r\n" +
                    "Cache-Control: no-cache\r\nConnection: close\r\n\r\n" + SensorWire.hello(kinds, sensorAliveMs))
                val token = HttpWire.bearerToken(request)
                val sent = mutableMapOf<String, Long>()
                var lightAt = 0L
                var lastWrite = System.currentTimeMillis()
                while (alive.get() && stillPaired(request)) {
                    Thread.sleep(SENSOR_TICK_MS)
                    val now = System.currentTimeMillis()
                    // A computer that vanished without closing sends no keepalive: noticed even while the lines still fit
                    if (now - (token?.let { lastSeenByToken[it] } ?: 0L) >= sensorAliveMs) {
                        runCatching { android.util.Log.i("MjpegServer", "No keepalive from a computer, stopping its sensors") }
                        break
                    }
                    val fresh = feed.latest().filter { (kind, value) ->
                        kind in kinds && sent[kind] != value.seq &&
                            (kind != SensorKinds.LIGHT || now - lightAt >= SENSOR_LIGHT_EVERY_MS)
                    }
                    if (fresh.isEmpty() && now - lastWrite < SENSOR_HEARTBEAT_MS) continue
                    fresh.forEach { (kind, value) -> sent[kind] = value.seq }
                    if (SensorKinds.LIGHT in fresh) lightAt = now
                    write(out, SensorWire.line(now, fresh))
                    lastWrite = now
                }
            } catch (_: Exception) {}
            finally { close() }
        }

        private fun write(out: java.io.OutputStream, text: String) {
            writingSinceMs = System.currentTimeMillis()
            out.write(text.toByteArray(Charsets.UTF_8))
            out.flush()
            writingSinceMs = 0L
        }

        // Closing the socket is the only way to get a stuck write back
        private fun watch() {
            while (alive.get()) {
                try { Thread.sleep(minOf(500L, sensorStallMs / 4)) } catch (_: InterruptedException) { return }
                val since = writingSinceMs
                if (since != 0L && System.currentTimeMillis() - since >= sensorStallMs) {
                    close()
                    runCatching { android.util.Log.i("MjpegServer", "A computer stopped taking sensor readings, stopping them") }
                }
            }
        }

        fun close() { alive.set(false); try { socket.close() } catch (_: Exception) {} }
    }
}
