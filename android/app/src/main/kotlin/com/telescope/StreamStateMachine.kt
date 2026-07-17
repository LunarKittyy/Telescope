package com.telescope

/**
 * Phases of one streaming attempt. [Recovering] covers an in-flight lens
 * switch on an already-open camera - the one case today that tears down
 * and rebuilds a session without a full service restart.
 */
enum class StreamState {
    Idle, StartingServer, OpeningCamera, ConfiguringSession, Streaming, Recovering, Failed, Stopping
}

data class StateTransition(
    val timestampMs: Long,
    val from: StreamState,
    val to: StreamState,
    val op: String,
    val error: String? = null,
)

/**
 * Pure state/history bookkeeping for [CameraStreamService]'s streaming
 * lifecycle, kept free of any CameraDevice/Service state (same rationale as
 * [CameraRequestSelection]) so it can be unit tested on a plain JVM. Callers
 * decide *when* to transition (from Camera2 callbacks, generation-guard
 * checks, etc.) - this class only records the result and bounds history.
 */
class StreamStateMachine(private val now: () -> Long = System::currentTimeMillis) {
    @Volatile var state: StreamState = StreamState.Idle
        private set

    val isStreaming: Boolean get() = state == StreamState.Streaming

    // Transitions can come from either the service's Camera2 HandlerThread
    // (async open/session callbacks) or the main thread (onStartCommand,
    // stopStreaming called directly on a bound service) - guard the shared
    // history buffer against concurrent mutation from both.
    private val history = ArrayDeque<StateTransition>()
    private val historyLock = Any()

    /** Records the transition to [newState] and returns it. [error], if
     *  given, is reduced to its class name + message - never a raw stack
     *  trace or anything from request headers/URLs/tokens. */
    fun transition(newState: StreamState, op: String, error: Throwable? = null): StateTransition {
        val old = state
        state = newState
        val errMsg = error?.let { "${it.javaClass.simpleName}: ${it.message}" }
        val record = StateTransition(now(), old, newState, op, errMsg)
        synchronized(historyLock) {
            history.addLast(record)
            while (history.size > MAX_HISTORY) history.removeFirst()
        }
        return record
    }

    fun recentTransitions(): List<StateTransition> = synchronized(historyLock) { history.toList() }

    companion object {
        const val MAX_HISTORY = 20
    }
}
