package com.telescope

import java.net.InetAddress
import java.net.InetSocketAddress
import java.net.ServerSocket
import java.net.Socket
import java.util.concurrent.atomic.AtomicBoolean
import kotlin.concurrent.thread
import kotlinx.serialization.json.Json

// Out-of-band channel: always reachable (unlike MjpegServer). Routes: /v1/hello, /v1/ping, /v1/session, /v1/unpair
class SessionServer(
    private val port: Int,
    private val computers: () -> PairedComputerList,
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

                // Unauthenticated identity only (what the LAN announcement already broadcasts): lets a desktop
                // tell "a different phone is plugged in" from "your phone no longer recognizes this computer".
                Route.Hello -> {
                    val snap = commands.snapshot()
                    HttpWire.sendJson(out, Json.encodeToString(
                        Hello.serializer(), Hello(PROTOCOL_VERSION, snap.phoneId, snap.phoneName)))
                }

                Route.Ping -> {
                    if (computers().matchToken(HttpWire.bearerToken(request)) == null) {
                        HttpWire.sendError(out, 401, "Unauthorized"); return
                    }
                    HttpWire.sendJson(
                        out,
                        Json.encodeToString(SessionSnapshot.serializer(), commands.snapshot()),
                    )
                }

                Route.Unpair -> {
                    // A computer can only revoke its own pairing: the token it authenticates with.
                    val computer = computers().matchToken(HttpWire.bearerToken(request))
                    if (computer == null) {
                        HttpWire.sendError(out, 401, "Unauthorized"); return
                    }
                    commands.unpair(computer)
                    HttpWire.sendJson(out, Json.encodeToString(ControlResult.serializer(), ControlResult(ok = true)))
                }

                Route.Session -> {
                    if (computers().matchToken(HttpWire.bearerToken(request)) == null) {
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

    enum class Route { Hello, Ping, Session, Unpair, NotFound, MethodNotAllowed }

    companion object {
        const val DEFAULT_PORT = 8766

        // Bumped on shape changes. 2: /v1/hello, ping carries phoneId/phoneName, /v1/unpair.
        const val PROTOCOL_VERSION = 2

        private const val TAG = "SessionServer"

        fun route(method: String, path: String): Route = when (path) {
            "/v1/hello" -> if (method == "GET") Route.Hello else Route.MethodNotAllowed
            "/v1/ping" -> if (method == "GET") Route.Ping else Route.MethodNotAllowed
            "/v1/session" -> if (method == "POST") Route.Session else Route.MethodNotAllowed
            "/v1/unpair" -> if (method == "POST") Route.Unpair else Route.MethodNotAllowed
            else -> Route.NotFound
        }
    }
}
