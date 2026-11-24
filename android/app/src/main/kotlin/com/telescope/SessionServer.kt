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
    private val appVersion: String = BuildConfig.VERSION_NAME,
    private val appBuild: Int = BuildConfig.VERSION_CODE,
    private val requestDeadlineMs: Int = HttpWire.REQUEST_DEADLINE_MS,
    private val pending: PendingLimiter = PendingLimiter(),
    // TLS in the app (PhoneTls); plain sockets only in tests.
    private val socketFactory: javax.net.ServerSocketFactory = javax.net.ServerSocketFactory.getDefault(),
    // Read per request, so flipping Local only takes effect without rebinding.
    private val localOnly: () -> Boolean = { false },
    // adb forward connects from the phone's own loopback, so that's what USB looks like here.
    private val isUsbPeer: (Socket) -> Boolean = LOOPBACK_IS_USB,
    private val authenticatedContact: (PairedComputer, Map<String, String>, String?) -> Unit = { _, _, _ -> },
    // An old port: a computer with an older Telescope still comes here. It is told to update (426), whatever it asked,
    // and this is told its name when its token is known.
    private val oldPort: Boolean = false,
    private val onOldPortContact: (String?) -> Unit = {},
) {
    private var serverSocket: ServerSocket? = null
    private val running = AtomicBoolean(false)
    @Volatile private var busy = false

    // Bind failures just report as unreachable, never fatal.
    fun start() {
        try {
            running.set(true)
            val socket = socketFactory.createServerSocket()
            try {
                socket.reuseAddress = true
                socket.bind(InetSocketAddress(InetAddress.getByName("0.0.0.0"), port), 10)
            } catch (e: Exception) {
                socket.close()  // retried on every acquire while the port is busy, so don't leak one each time
                throw e
            }
            serverSocket = socket
            busy = false
        } catch (e: Exception) {
            busy = e is java.net.BindException
            running.set(false)
            runCatching { android.util.Log.w(TAG, "Could not bind port $port", e) }  // unit tests have no Log
            return
        }
        thread(name = "session-accept", isDaemon = true) {
            while (running.get()) {
                try {
                    val socket = serverSocket?.accept() ?: break
                    val address = socket.inetAddress?.hostAddress.orEmpty()
                    if (!pending.tryAcquire(address)) {
                        try { socket.close() } catch (_: Exception) {}
                        continue
                    }
                    thread(name = "session-client", isDaemon = true) {
                        try { handle(socket) } finally { pending.release(address) }
                    }
                } catch (e: Exception) {
                    if (!running.get()) break
                    android.util.Log.e(TAG, "Accept error", e)
                    // The socket went away underneath us: stop, so the next acquire binds again. Else (out of files, say) back off.
                    if (serverSocket?.isClosed != false) { running.set(false); break }
                    Thread.sleep(ACCEPT_RETRY_MS)
                }
            }
        }
    }

    // The last bind failed because something else holds the port (the other Telescope build, say).
    val portBusy: Boolean get() = busy

    // Bound and accepting; false after a failed bind or a dead accept loop.
    val listening: Boolean get() = running.get()

    fun stop() {
        running.set(false)
        try { serverSocket?.close() } catch (_: Exception) {}
    }

    private fun handle(socket: Socket) {
        try {
            val request = HttpWire.readRequest(socket, requestDeadlineMs) ?: return  // already responded/closed
            val out = socket.getOutputStream()
            val route = route(request.method, request.path)
            if (oldPort) {
                answerOldPort(socket, route, request); return
            }

            // Still listening on Wi-Fi in Local only on purpose: ping lets the desktop see the toggle is on and tell the user
            if (route !in REACHABLE_WHEN_LOCAL_ONLY && localOnly() && !isUsbPeer(socket)) {
                HttpWire.sendError(out, 403, "Forbidden"); return
            }

            if (route in setOf(Route.Hello, Route.Ping, Route.Session)) {
                computers().matchToken(HttpWire.bearerToken(request))?.let { computer ->
                    authenticatedContact(computer, request.headers, socket.inetAddress?.hostAddress)
                }
            }
            when (route) {
                Route.NotFound -> HttpWire.sendError(out, 404, "Not Found")
                Route.MethodNotAllowed -> HttpWire.sendError(out, 405, "Method Not Allowed")

                // Unauthenticated identity only (what the LAN announcement already broadcasts): lets a desktop
                // tell "a different phone is plugged in" from "your phone no longer recognizes this computer".
                Route.Hello -> {
                    val snap = commands.snapshot()
                    HttpWire.sendJson(out, Json.encodeToString(
                        Hello.serializer(), Hello(PROTOCOL_VERSION, snap.phoneId, snap.phoneName, appVersion, appBuild, presence = 1)))
                }

                Route.Ping -> {
                    val computer = computers().matchToken(HttpWire.bearerToken(request))
                    if (computer == null) {
                        HttpWire.sendError(out, 401, "Unauthorized"); return
                    }
                    HttpWire.sendJson(
                        out,
                        Json.encodeToString(SessionSnapshot.serializer(), commands.snapshot(computer).copy(presence = 1)),
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
                    val computer = computers().matchToken(HttpWire.bearerToken(request))
                    if (computer == null) {
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
                    // Unpaired while the body was arriving: the early check above is stale by now
                    if (computers().matchToken(HttpWire.bearerToken(request)) == null) {
                        HttpWire.sendError(out, 401, "Unauthorized"); return
                    }
                    val result = when (val action = params["action"]) {
                        "start" -> commands.start(StreamOpening.from(params), computer)
                        "stop" -> commands.stop(computer)
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

    // An older computer asks who this is first, then pings with its token: only that token proves it is one of the paired
    // computers, so only a ping that carries it shows the notice. Everything else, session commands included, is refused.
    private fun answerOldPort(socket: Socket, route: Route, request: HttpWire.Request) {
        val out = socket.getOutputStream()
        if (route == Route.Hello) {
            if (localOnly() && !isUsbPeer(socket)) {
                HttpWire.sendError(out, 403, "Forbidden"); return
            }
            val snap = commands.snapshot()
            HttpWire.sendJson(out, Json.encodeToString(
                Hello.serializer(), Hello(PROTOCOL_VERSION, snap.phoneId, snap.phoneName, appVersion, appBuild, presence = 1)))
            return
        }
        if (route == Route.Ping) {
            computers().matchToken(HttpWire.bearerToken(request))?.let { onOldPortContact(it.name) }
        }
        HttpWire.sendError(out, 426, "Upgrade Required")
    }

    enum class Route { Hello, Ping, Session, Unpair, NotFound, MethodNotAllowed }

    companion object {
        const val DEFAULT_PORT = 7480

        // Where the desktop's pairing server and the phone's session server were before the move to 7479 and 7480. Still
        // listened on for a couple of releases, only to tell an older Telescope on a computer to update.
        val OLD_PORTS = listOf(8765, 8766)

        // An old port's server; localOnly has no default, since the real port's Local only must hold here too.
        fun forOldPort(
            port: Int,
            computers: () -> PairedComputerList,
            commands: SessionCommands,
            socketFactory: javax.net.ServerSocketFactory,
            localOnly: () -> Boolean,
            onOldPortContact: (String?) -> Unit,
            isUsbPeer: (Socket) -> Boolean = LOOPBACK_IS_USB,
        ) = SessionServer(port, computers, commands, socketFactory = socketFactory, localOnly = localOnly, isUsbPeer = isUsbPeer,
            oldPort = true, onOldPortContact = onOldPortContact)

        private val LOOPBACK_IS_USB: (Socket) -> Boolean = { it.inetAddress?.isLoopbackAddress == true }

        // A dev build runs next to the real app, and holding 8766 would stop an older real app from opening its session port
        fun oldPortsFor(buildType: String): List<Int> = if (buildType == "dev") emptyList() else OLD_PORTS
        const val ACCEPT_RETRY_MS = 200L

        // Optional metadata and presence capability are additive and do not bump this strictly matched protocol.
        // Bumped on incompatible shape changes. 2: /v1/hello, ping carries phoneId/phoneName, /v1/unpair. 3: TLS, pinned at pairing.
        const val PROTOCOL_VERSION = 3

        private const val TAG = "SessionServer"

        // What a Local only phone still answers off USB: the status check, and a computer revoking its own pairing.
        private val REACHABLE_WHEN_LOCAL_ONLY = setOf(Route.Ping, Route.Unpair, Route.NotFound, Route.MethodNotAllowed)

        fun route(method: String, path: String): Route = when (path) {
            "/v1/hello" -> if (method == "GET") Route.Hello else Route.MethodNotAllowed
            "/v1/ping" -> if (method == "GET") Route.Ping else Route.MethodNotAllowed
            "/v1/session" -> if (method == "POST") Route.Session else Route.MethodNotAllowed
            "/v1/unpair" -> if (method == "POST") Route.Unpair else Route.MethodNotAllowed
            else -> Route.NotFound
        }
    }
}
