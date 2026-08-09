package com.telescope

import android.content.Context
import kotlinx.serialization.Serializable

/**
 * What `GET /v1/ping` answers with. The 200/401 status still carries the
 * pairing verdict on its own - an older desktop that only reads the status
 * code keeps working - and this body tells a newer one what the phone is
 * actually doing, so it can decide whether a remote start is needed and
 * explain a refusal precisely.
 */
@Serializable
data class SessionSnapshot(
    val protocol: Int,
    val streaming: Boolean,
    /** A start is in flight (camera opening, session configuring). The desktop
     *  waits this out rather than issuing a second start. */
    val busy: Boolean,
    /** The phone's "Local only" setting: the stream server binds 127.0.0.1 and
     *  is reachable over adb but not over Wi-Fi. Reported so the desktop can
     *  name that mismatch instead of timing out against an address nothing is
     *  listening on. */
    val localOnly: Boolean,
)

/** What [SessionServer] is allowed to do to the stream. Narrow on purpose: the
 *  server owns HTTP, this owns the camera lifecycle, and neither reaches into
 *  the other. */
interface SessionCommands {
    fun start(): ControlResult
    fun stop(): ControlResult
    fun snapshot(): SessionSnapshot
}

/**
 * Owns the single [SessionServer] instance and its port.
 *
 * Two components want that port bound - [MainActivity] while it is on screen,
 * and [CameraStreamService] while the camera is live - and two `ServerSocket`s
 * cannot share one, so ownership is tracked as a set of tags rather than left
 * to whichever component happened to stop last. The server binds on the first
 * [acquire] and closes on the last [release]; a repeated acquire or release
 * from the same owner is a no-op, which matters because
 * [CameraStreamService.stopStreaming] is reachable both directly and again via
 * `onDestroy`.
 *
 * The union of the two owners is what gives the desktop a channel that
 * survives the phone's screen going to sleep mid-stream, which is what makes
 * "restart the stream from the PC" work without walking back to the phone.
 */
object SessionEndpoint {
    const val OWNER_ACTIVITY = "activity"
    const val OWNER_SERVICE = "service"

    private val owners = mutableSetOf<String>()
    private var server: SessionServer? = null

    @Synchronized
    fun acquire(context: Context, owner: String) {
        val app = context.applicationContext
        if (!owners.add(owner)) return
        if (server != null) return
        server = SessionServer(
            port = SessionServer.DEFAULT_PORT,
            tokenProvider = { TokenStore.get(app) },
            commands = ServiceSessionCommands(app),
        ).also { it.start() }
    }

    @Synchronized
    fun release(owner: String) {
        if (!owners.remove(owner)) return
        if (owners.isNotEmpty()) return
        server?.stop()
        server = null
    }

    /** Test seam: lets a JVM test drive the refcount without binding a port. */
    @Synchronized
    fun isBound(): Boolean = server != null
}

/**
 * [SessionCommands] backed by the real service. Deliberately holds only an
 * application [Context]: it is called from a socket thread that may outlive
 * whichever component acquired the endpoint.
 */
private class ServiceSessionCommands(private val context: Context) : SessionCommands {

    override fun start(): ControlResult {
        val service = CameraStreamService.instance
        if (service?.isStreaming == true) return ControlResult(ok = true)
        // Prevent race; same guard as MainActivity.isBusy().
        if (service != null && service.state != StreamState.Idle && service.state != StreamState.Failed) {
            return ControlResult(ok = false, error = "busy")
        }
        return when (val result = StreamLauncher.startFromPrefs(context)) {
            is StreamLauncher.Result.Started -> ControlResult(ok = true)
            is StreamLauncher.Result.AlreadyStreaming -> ControlResult(ok = true)
            is StreamLauncher.Result.Rejected -> ControlResult(ok = false, error = result.reason)
        }
    }

    override fun stop(): ControlResult {
        val service = CameraStreamService.instance ?: return ControlResult(ok = true)
        service.stopStreaming()
        return ControlResult(ok = true)
    }

    override fun snapshot(): SessionSnapshot {
        val state = CameraStreamService.instance?.state ?: StreamState.Idle
        return SessionSnapshot(
            protocol = SessionServer.PROTOCOL_VERSION,
            streaming = state == StreamState.Streaming,
            busy = state != StreamState.Idle &&
                state != StreamState.Streaming &&
                state != StreamState.Failed,
            localOnly = StreamPrefs.localOnly(context),
        )
    }
}
