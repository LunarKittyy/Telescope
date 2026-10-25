package com.telescope

// "Streaming started from your computer" once per remote start, however often the app is opened while it runs. The
// stream service owns it, since it outlives every MainActivity.
class RemoteStartNotice {
    private var pending = false

    // A new start: one from the computer gets announced once, one from the phone never
    @Synchronized
    fun started(remotely: Boolean) {
        pending = remotely
    }

    // True the first time it's asked while streaming after a remote start, then false until the next one
    @Synchronized
    fun take(streaming: Boolean): Boolean {
        if (!pending || !streaming) return false
        pending = false
        return true
    }
}
