package com.telescope

import java.net.InetAddress
import java.net.InetSocketAddress
import java.net.ServerSocket
import java.net.Socket
import java.util.concurrent.atomic.AtomicBoolean
import kotlin.concurrent.thread
import kotlinx.serialization.json.Json

// Out-of-band session channel (always reachable, unlike MjpegServer). Routes: GET /v1/ping (status) and POST /v1/session (start/stop).
class SessionServer(
    private val port: Int,
    private val tokenProvider: () -> String?,
    private val commands: SessionCommands,
) {
    private var serverSocket: ServerSocket? = null
    private val running = AtomicBoolean(false)

    // Bind failures just report as unreachable, never fatal.
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

        // Bumped on shape changes; 404 /v1/session signals desktop to fall back.
        const val PROTOCOL_VERSION = 1

        private const val TAG = "SessionServer"

        // Extracted for testability without a socket.
        fun route(method: String, path: String): Route = when (path) {
            "/v1/ping" -> if (method == "GET") Route.Ping else Route.MethodNotAllowed
            "/v1/session" -> if (method == "POST") Route.Session else Route.MethodNotAllowed
            else -> Route.NotFound
        }
    }
}
