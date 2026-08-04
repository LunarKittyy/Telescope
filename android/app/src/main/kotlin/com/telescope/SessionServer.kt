package com.telescope

import java.net.InetAddress
import java.net.InetSocketAddress
import java.net.ServerSocket
import java.net.Socket
import java.util.concurrent.atomic.AtomicBoolean
import kotlin.concurrent.thread
import kotlinx.serialization.json.Json

/**
 * The phone's out-of-band channel to the desktop, on its own port, reachable
 * when [MjpegServer] is not - that one is created in
 * [CameraStreamService.startServer] and destroyed in
 * [CameraStreamService.stopStreaming], so it does not exist in exactly the
 * state the desktop needs to reach the phone from: idle.
 *
 * Two routes, both bearer-authenticated against the current pairing token:
 *
 *   GET  /v1/ping     - is this token still the paired one, and what is the
 *                       phone doing right now
 *   POST /v1/session  - start or stop the camera from the desktop
 *
 * [tokenProvider] is read fresh on every request rather than captured once, so
 * a re-pair while this server is already running (no restart in between) is
 * reflected on the very next check.
 *
 * Lifetime is managed by [SessionEndpoint], not by this class: it stays bound
 * while [MainActivity] is visible *or* [CameraStreamService] is running. That
 * pairing of owners is also the safety boundary for remote start - there is no
 * window in which a fully backgrounded, non-streaming app can be told to open
 * the camera.
 */
class SessionServer(
    private val port: Int,
    private val tokenProvider: () -> String?,
    private val commands: SessionCommands,
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
            android.util.Log.w(TAG, "Could not bind port $port", e)
            running.set(false)
            return
        }
        thread(name = "session-accept", isDaemon = true) {
            while (running.get()) {
                try {
                    val socket = serverSocket?.accept() ?: break
                    thread(name = "session-client", isDaemon = true) { handle(socket) }
                } catch (e: Exception) {
                    if (running.get()) android.util.Log.e(TAG, "Accept error", e)
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
            socket.soTimeout = HttpWire.READ_TIMEOUT_MS
            val request = HttpWire.readRequest(socket) ?: return  // already responded/closed
            val out = socket.getOutputStream()

            when (route(request.method, request.path)) {
                Route.NotFound -> HttpWire.sendError(out, 404, "Not Found")
                Route.MethodNotAllowed -> HttpWire.sendError(out, 405, "Method Not Allowed")

                Route.Ping -> {
                    if (!HttpWire.bearerMatches(tokenProvider(), request)) {
                        HttpWire.sendError(out, 401, "Unauthorized"); return
                    }
                    HttpWire.sendJson(
                        out,
                        Json.encodeToString(SessionSnapshot.serializer(), commands.snapshot()),
                    )
                }

                Route.Session -> {
                    if (!HttpWire.bearerMatches(tokenProvider(), request)) {
                        HttpWire.sendError(out, 401, "Unauthorized"); return
                    }
                    if (!HttpWire.isJsonBody(request)) {
                        HttpWire.sendError(out, 400, "Bad Request"); return
                    }
                    val body = HttpWire.readBody(socket, request) ?: return  // already responded
                    val params = HttpWire.parseJsonParams(body)
                    if (params == null) {
                        HttpWire.sendError(out, 400, "Bad Request"); return
                    }
                    val result = when (val action = params["action"]) {
                        "start" -> commands.start()
                        "stop" -> commands.stop()
                        else -> ControlResult(ok = false, error = "unknown action '$action'")
                    }
                    HttpWire.sendJson(out, Json.encodeToString(ControlResult.serializer(), result))
                }
            }
        } catch (_: Exception) {
        } finally {
            try { socket.close() } catch (_: Exception) {}
        }
    }

    enum class Route { Ping, Session, NotFound, MethodNotAllowed }

    companion object {
        const val DEFAULT_PORT = 8766

        /** Bumped when the shape of [SessionSnapshot] or the `/v1/session`
         *  action set changes in a way an older desktop would misread. The
         *  desktop treats a 404 on `/v1/session` as "old app, fall back to
         *  connect-only", so the version alone never has to gate the feature. */
        const val PROTOCOL_VERSION = 1

        private const val TAG = "SessionServer"

        /** Split out from [handle] so the routing table is testable without a
         *  socket, the same way [parsePairingOffer] is. */
        fun route(method: String, path: String): Route = when (path) {
            "/v1/ping" -> if (method == "GET") Route.Ping else Route.MethodNotAllowed
            "/v1/session" -> if (method == "POST") Route.Session else Route.MethodNotAllowed
            else -> Route.NotFound
        }
    }
}
