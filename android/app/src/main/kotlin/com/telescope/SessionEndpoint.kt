package com.telescope

import android.content.Context
import kotlinx.serialization.Serializable

// What `GET /v1/ping` answers with: the 200/401 status is the pairing verdict, this body is what the phone is doing.
@Serializable
data class SessionSnapshot(
    val protocol: Int,
    val streaming: Boolean,
    // Start in flight: desktop waits rather than retry.
    val busy: Boolean,
    // Local only: binds 127.0.0.1, reachable via adb not Wi-Fi (prevents desktop timeout).
    val localOnly: Boolean,
    // Stable per install; lets the desktop tell which paired phone answered, e.g. over a USB forward.
    val phoneId: String,
    val phoneName: String,
    // The other paired computer a running or starting stream belongs to, when it isn't the one asking.
    val streamingFor: String? = null,
)

// appVersion/build: lets the desktop say which app needs updating instead of just "unreachable".
@Serializable
data class Hello(
    val protocol: Int,
    val phoneId: String,
    val phoneName: String,
    val appVersion: String = "",
    val build: Int = 0,
)

// Narrow interface: server owns HTTP, this owns camera lifecycle - they don't cross.
// computer: the paired computer asking, or null for a request without one (hello).
interface SessionCommands {
    fun start(opening: StreamOpening? = null, computer: PairedComputer? = null): ControlResult
    fun stop(computer: PairedComputer? = null): ControlResult
    fun snapshot(computer: PairedComputer? = null): SessionSnapshot
    fun unpair(computer: PairedComputer)
}

// A stream belongs to the computer whose start opened it, or to the first one to watch a stream started on the phone.
// Another computer can't stop it or change its codec. An owner that has been unpaired no longer counts.
object StreamOwner {
    const val BUSY_OTHER = "busy_other"

    // The paired computer that has the stream instead of caller; null when caller may have it.
    fun other(ownerId: String?, caller: PairedComputer?, computers: PairedComputerList): PairedComputer? {
        if (ownerId == null || caller == null || ownerId == caller.id) return null
        return computers.computers.find { it.id == ownerId }
    }

    fun busyOther(owner: PairedComputer) = ControlResult(ok = false, error = BUSY_OTHER, computer = owner.name)
}

// Refcount design: Activity, stream service and waiting service hold tags; first acquire binds port, last release closes.
object SessionEndpoint {
    const val OWNER_ACTIVITY = "activity"
    const val OWNER_SERVICE = "service"
    const val OWNER_WAITING = "waiting"  // WaitingService, while "Wait for my computer" is on

    private val owners = mutableSetOf<String>()
    private var server: SessionServer? = null
    private var announcer: LanAnnouncer? = null

    @Synchronized
    fun acquire(context: Context, owner: String) {
        val app = context.applicationContext
        owners.add(owner)
        // A server whose bind failed or whose accept loop died is replaced on the next acquire, not kept forever.
        if (server?.listening == true) {
            announcer?.start(SessionServer.DEFAULT_PORT)  // retries an announcement that failed; a no-op otherwise
            return
        }
        server?.stop()
        announcer?.stop()
        server = try {
            SessionServer(
                port = SessionServer.DEFAULT_PORT,
                computers = { PairedComputers.list(app) },
                commands = ServiceSessionCommands(app),
                socketFactory = PhoneTls.identity(app).serverSocketFactory(),
                localOnly = { StreamPrefs.localOnly(app) },
            ).also { it.start() }
        } catch (e: Exception) {
            android.util.Log.e("SessionEndpoint", "Could not start the session server", e)  // storage full saving the identity, say
            null
        }
        announcer = LanAnnouncer(app).also { it.start(SessionServer.DEFAULT_PORT) }
    }

    // Local only was toggled: announce or go quiet to match.
    @Synchronized
    fun refreshAnnouncement() {
        val a = announcer ?: return
        a.stop()
        a.start(SessionServer.DEFAULT_PORT)
    }

    @Synchronized
    fun release(owner: String) {
        if (!owners.remove(owner)) return
        if (owners.isNotEmpty()) return
        server?.stop()
        server = null
        announcer?.stop()
        announcer = null
    }

    // Test seam: drive refcount without binding port.
    @Synchronized
    fun isBound(): Boolean = server != null
}

// Busy from an accepted start until the service has had a moment to report on it. A start can fail within milliseconds
// (Android refusing the camera, say), before the computer's first poll; without this the computer never sees busy and
// waits out its whole timeout instead of saying the camera stopped before it finished starting.
open class StartWindow(private val clock: () -> Long) {
    @Volatile private var until = 0L

    fun begin() { until = clock() + PENDING_MS }

    // The service left StartingServer: stay busy just long enough for one poll to see it, then report its own state.
    fun settle() { until = minOf(until, clock() + SETTLE_MS) }

    fun open(): Boolean = clock() < until

    fun cancel() { until = 0L }

    companion object {
        const val PENDING_MS = 5_000L  // the service never got to onStartCommand
        const val SETTLE_MS = 1_500L   // three of the computer's 0.5 s polls
    }
}

object SessionStartWindow : StartWindow({ android.os.SystemClock.elapsedRealtime() })

// Holds app Context only: socket thread may outlive component that acquired endpoint.
private class ServiceSessionCommands(private val context: Context) : SessionCommands {

    override fun start(opening: StreamOpening?, computer: PairedComputer?): ControlResult {
        val service = CameraStreamService.instance
        service?.otherOwner(computer)?.let { return StreamOwner.busyOther(it) }
        if (service?.isStreaming == true) {
            computer?.let { service.claim(it) }  // one started on the phone is this computer's from now on
            return ControlResult(ok = true)
        }
        // Same guard as MainActivity.isBusy().
        if (service != null && service.state != StreamState.Idle && service.state != StreamState.Failed) {
            return ControlResult(ok = false, error = "busy")
        }
        return when (val result = StreamLauncher.startFromPrefs(context, opening, owner = computer?.id)) {
            is StreamLauncher.Result.Started -> ControlResult(ok = true)
            is StreamLauncher.Result.AlreadyStreaming -> ControlResult(ok = true)
            is StreamLauncher.Result.Rejected -> ControlResult(ok = false, error = result.reason)
        }
    }

    override fun stop(computer: PairedComputer?): ControlResult {
        val service = CameraStreamService.instance ?: return ControlResult(ok = true)
        // Another computer's stream carries on
        service.otherOwner(computer)?.let { return StreamOwner.busyOther(it) }
        // On the main thread like every other stop, not this socket thread racing them.
        android.os.Handler(android.os.Looper.getMainLooper()).post { CameraStreamService.instance?.stopStreaming("remoteStop") }
        return ControlResult(ok = true)
    }

    override fun snapshot(computer: PairedComputer?): SessionSnapshot {
        val service = CameraStreamService.instance
        val state = service?.state ?: StreamState.Idle
        return SessionSnapshot(
            protocol = SessionServer.PROTOCOL_VERSION,
            streaming = state == StreamState.Streaming,
            busy = SessionStartWindow.open() || (
                state != StreamState.Idle &&
                    state != StreamState.Streaming &&
                    state != StreamState.Failed
                ),
            localOnly = StreamPrefs.localOnly(context),
            phoneId = PairedComputers.phoneId(context),
            phoneName = PairedComputers.phoneName(context),
            streamingFor = service?.otherOwner(computer)?.name,
        )
    }

    override fun unpair(computer: PairedComputer) {
        PairedComputers.remove(context, computer.id)
    }
}
