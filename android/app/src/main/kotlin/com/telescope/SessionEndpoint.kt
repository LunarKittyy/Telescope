package com.telescope

import android.content.Context
import kotlinx.serialization.Serializable

// What `GET /v1/ping` answers with; the 200/401 status alone still tells an older desktop the pairing verdict, this body adds what the phone is actually doing.
@Serializable
data class SessionSnapshot(
    val protocol: Int,
    val streaming: Boolean,
    // Start in flight: desktop waits rather than retry.
    val busy: Boolean,
    // Local only: binds 127.0.0.1, reachable via adb not Wi-Fi (prevents desktop timeout).
    val localOnly: Boolean,
)

// Narrow interface: server owns HTTP, this owns camera lifecycle - they don't cross.
interface SessionCommands {
    fun start(): ControlResult
    fun stop(): ControlResult
    fun snapshot(): SessionSnapshot
}

// Refcount design: Activity and Service hold tags; first acquire binds port, last release closes; survives screen sleep.
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

    // Test seam: drive refcount without binding port.
    @Synchronized
    fun isBound(): Boolean = server != null
}

// Holds app Context only: socket thread may outlive component that acquired endpoint.
private class ServiceSessionCommands(private val context: Context) : SessionCommands {

    override fun start(): ControlResult {
        val service = CameraStreamService.instance
        if (service?.isStreaming == true) return ControlResult(ok = true)
        // Same guard as MainActivity.isBusy().
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
