package com.telescope

// Phases of one stream attempt; Recovering is in-flight lens switch (session rebuild without service restart).
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

// Pure state/history for streaming lifecycle (no Camera2/Service state for JVM testability); callers decide when to transition.
class StreamStateMachine(private val now: () -> Long = System::currentTimeMillis) {
    @Volatile var state: StreamState = StreamState.Idle
        private set

    val isStreaming: Boolean get() = state == StreamState.Streaming

    // Guard against concurrent mutation from Camera2 HandlerThread and main thread.
    private val history = ArrayDeque<StateTransition>()
    private val historyLock = Any()

    // Records transition; error is reduced to class name + message (no stack trace/headers/URLs/tokens).
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

    // Records non-fatal event (no state change); emitted as self-transition so it surfaces in diagnostics.
    fun record(op: String, error: Throwable? = null): StateTransition {
        val errMsg = error?.let { "${it.javaClass.simpleName}: ${it.message}" }
        val record = StateTransition(now(), state, state, op, errMsg)
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
