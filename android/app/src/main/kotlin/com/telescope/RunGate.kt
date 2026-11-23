package com.telescope

// Lets a callback from a run change shared state only while that run is the current one, atomically with the run ending
class RunGate<T : Any> {
    private var current: T? = null

    @Synchronized fun begin(run: T) { current = run }

    // Runs block when run is the current one; the run can't end until block is done. False when it isn't
    @Synchronized fun ifCurrent(run: T, block: () -> Unit): Boolean {
        if (current !== run) return false
        block()
        return true
    }

    // Ends the current run and runs block in the same step, so no late callback lands between the two
    @Synchronized fun end(block: () -> Unit) {
        current = null
        block()
    }
}
