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
    // Runs Dynamic over UDP from the desktop's reports (UdpLink); a desktop keeps Dynamic on TCP for a phone without it.
    val udpDynamic: Boolean = false,
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

// The live stream service as far as a remote stop is concerned.
interface StopTarget {
    fun otherOwner(caller: PairedComputer?): PairedComputer?
    fun stop(op: String)
}

// Runs where the stop actually happens (the main thread): a start queued for another computer may have reached the
// service since the request was answered, and that stream isn't this computer's to stop.
internal fun stopForComputer(service: StopTarget?, computer: PairedComputer?): Boolean {
    if (service == null || service.otherOwner(computer) != null) return false
    service.stop("remoteStop")
    return true
}

// Counts the app screens on show: the first one up and the last one gone are the only changes anyone needs to hear of.
class VisibleScreens(private val onFirst: () -> Unit, private val onNone: () -> Unit) {
    private var count = 0

    @Synchronized
    fun started() { if (count++ == 0) onFirst() }

    @Synchronized
    fun stopped() { if (count > 0 && --count == 0) onNone() }

    @Synchronized
    fun any(): Boolean = count > 0
}

// Refcount design: Activity, stream service and waiting service hold tags; first acquire binds port, last release closes.
object SessionEndpoint {
    const val OWNER_ACTIVITY = "activity"
    const val OWNER_SERVICE = "service"
    const val OWNER_WAITING = "waiting"  // WaitingService, while "Wait for my computer" is on
    const val OWNER_STOPPED = "stopped"  // briefly after a stream stops, so its computer hears it stopped rather than dropped

    private const val STOPPED_HOLD_MS = 6_000L  // the computer asks within a second or two of its stream ending

    private const val REBIND_MS = 3_000L  // while held, a server that isn't listening (port busy, say) is retried this often

    private val owners = mutableSetOf<String>()
    private var server: SessionServer? = null
    private var announcer: LanAnnouncer? = null
    private var watchdog: java.util.Timer? = null
    private var stoppedHold: java.util.TimerTask? = null

    @Synchronized
    fun acquire(context: Context, owner: String) {
        val app = context.applicationContext
        owners.add(owner)
        bind(app)
        if (watchdog == null) {
            watchdog = java.util.Timer("session-rebind", true).apply {
                schedule(object : java.util.TimerTask() {
                    override fun run() = rebind(app)
                }, REBIND_MS, REBIND_MS)
            }
        }
    }

    // A stream just stopped: keep answering /v1/ping for a bit even if the app leaves the screen right away.
    @Synchronized
    fun holdAfterStop(context: Context) {
        acquire(context, OWNER_STOPPED)
        stoppedHold?.cancel()
        stoppedHold = object : java.util.TimerTask() {
            override fun run() = release(OWNER_STOPPED)
        }.also { watchdog?.schedule(it, STOPPED_HOLD_MS) }
    }

    // The port was busy (the other Telescope build, an old socket after an update) or the accept loop died: try again
    @Synchronized
    private fun rebind(app: Context) {
        if (owners.isNotEmpty() && server?.listening != true) bind(app)
    }

    private fun bind(app: Context) {
        // A server whose bind failed or whose accept loop died is replaced, not kept forever.
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
        if (server?.listening == true) announcer = LanAnnouncer(app).also { it.start(SessionServer.DEFAULT_PORT) }
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
        stoppedHold = null
        watchdog?.cancel()
        watchdog = null
        server?.stop()
        server = null
        announcer?.stop()
        announcer = null
    }

    // Each app screen (main, settings) calls these from onStart/onStop, so moving between them never drops the endpoint.
    private var appContext: Context? = null
    private val screens = VisibleScreens(
        onFirst = { appContext?.let { acquire(it, OWNER_ACTIVITY) } },
        onNone = { release(OWNER_ACTIVITY) },
    )

    fun screenStarted(context: Context) {
        appContext = context.applicationContext
        // The first start reads (or makes) the TLS identity: do it off the main thread while the screen comes up
        kotlin.concurrent.thread(name = "tls-warm", isDaemon = true) { try { PhoneTls.identity(context) } catch (_: Exception) {} }
        screens.started()
    }

    fun screenStopped() = screens.stopped()

    @Synchronized
    fun isHeldBy(owner: String): Boolean = owner in owners

    // Test seam: drive refcount without binding port.
    @Synchronized
    fun isBound(): Boolean = server != null
}

// Busy from an accepted start until the service has had a moment to report on it. A start can fail within milliseconds
// (Android refusing the camera, say), before the computer's first poll; without this the computer never sees busy and
// waits out its whole timeout instead of saying the camera stopped before it finished starting.
open class StartWindow(private val clock: () -> Long) {
    @Volatile private var until = 0L
    private var issued = 0L
    // Starts accepted and not stopped yet, by ticket: whose each one is (null when nobody is named)
    private val queued = mutableMapOf<Long, String?>()
    private val cancelled = mutableSetOf<Long>()

    // The ticket goes to the service with the start, so abort() can tell it which starts were stopped before they ran.
    @Synchronized
    fun begin(owner: String? = null): Long {
        val now = clock()
        if (now >= until) queued.clear()  // the window had closed: those starts are long past
        if (cancelled.size > MAX_REMEMBERED) cancelled.removeAll { it <= issued - MAX_REMEMBERED }
        until = now + PENDING_MS
        queued[++issued] = owner
        return issued
    }

    // A stop arrived while a start was still queued: the starts it may cancel are cancelled, and the window closes unless
    // another computer's start is still waiting in it. A stop from a known computer leaves a start queued for another alone.
    @Synchronized
    fun abort(by: String? = null): Boolean {
        if (!open()) return false
        val mine = queued.filterValues { owner -> owner == null || by == null || owner == by }.keys
        cancelled.addAll(mine)
        queued.keys.removeAll(mine)
        if (queued.isEmpty()) until = 0L
        return mine.isNotEmpty()
    }

    @Synchronized
    fun aborted(ticket: Long): Boolean = ticket in cancelled

    // The service left StartingServer: stay busy just long enough for one poll to see it, then report its own state.
    fun settle() { until = minOf(until, clock() + SETTLE_MS) }

    fun open(): Boolean = clock() < until

    fun cancel() { until = 0L }

    companion object {
        const val PENDING_MS = 5_000L  // the service never got to onStartCommand
        const val SETTLE_MS = 1_500L   // three of the computer's 0.5 s polls
        private const val MAX_REMEMBERED = 32
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
        val service = CameraStreamService.instance
        // Another computer's stream carries on, and so does a start it has queued
        service?.otherOwner(computer)?.let { return StreamOwner.busyOther(it) }
        // A start still queued (no instance yet, or one before onStartCommand) must not open the camera after this stop
        SessionStartWindow.abort(computer?.id)
        if (service == null) return ControlResult(ok = true)
        // On the main thread like every other stop, not this socket thread racing them, and checked again there.
        android.os.Handler(android.os.Looper.getMainLooper()).post {
            val live = CameraStreamService.instance
            stopForComputer(live?.let { svc ->
                object : StopTarget {
                    override fun otherOwner(caller: PairedComputer?) = svc.otherOwner(caller)
                    override fun stop(op: String) = svc.stopStreaming(op)
                }
            }, computer)
        }
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
            udpDynamic = true,
        )
    }

    override fun unpair(computer: PairedComputer) {
        PairedComputers.remove(context, computer.id)
    }
}
