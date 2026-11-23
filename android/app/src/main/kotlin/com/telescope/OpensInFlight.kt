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
            run?.invoke()
        }
    }

    private var pending = 0
    private var waiting: (() -> Unit)? = null

    fun begin(): Open = synchronized(this) { pending++; Open() }

    // Runs block now when nothing is opening, else once the last open is answered or giveUp() is called; once only
    fun whenSettled(block: () -> Unit) {
        val now = synchronized(this) {
            if (pending == 0) block else { waiting = block; null }
        }
        now?.invoke()
    }

    // Stops waiting: the camera service never answered
    fun giveUp() {
        val run = synchronized(this) { take() }
        run?.invoke()
    }

    private fun take(): (() -> Unit)? = waiting.also { waiting = null }
}
