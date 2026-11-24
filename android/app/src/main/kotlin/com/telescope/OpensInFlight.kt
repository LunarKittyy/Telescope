package com.telescope

// Camera opens that haven't been answered yet, so a stop can wait for a device that arrives late and close it
class OpensInFlight {
    inner class Open internal constructor() {
        private var ended = false

        // The open was answered (opened, disconnected or failed) or never started; safe to call more than once
        fun end() {
            val run = synchronized(this@OpensInFlight) {
                if (ended) return
                ended = true
                pending--
                if (pending == 0) take() else null
            }
            run?.forEach { it() }
        }
    }

    private var pending = 0
    private val waiting = ArrayList<() -> Unit>()

    fun begin(): Open = synchronized(this) { pending++; Open() }

    // Runs block now when nothing is opening, else once the last open is answered or giveUp() is called; once only, and every waiting block runs
    fun whenSettled(block: () -> Unit) {
        val now = synchronized(this) {
            if (pending == 0) block else { waiting.add(block); null }
        }
        now?.invoke()
    }

    // Stops waiting: the camera service never answered
    fun giveUp() {
        val run = synchronized(this) { take() }
        run?.forEach { it() }
    }

    private fun take(): List<() -> Unit>? = if (waiting.isEmpty()) null else waiting.toList().also { waiting.clear() }
}
