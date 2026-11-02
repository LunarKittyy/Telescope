package com.telescope

// What a rebuild of the stream means for a photo in flight, kept apart from Camera2 so it can be tested
object StillRebuild {
    enum class Cause {
        SESSION,  // the same camera's session again (a setting, the preview, Dynamic's rate): its photo outputs stay
        LENS,     // another lens
        REOPEN,   // the camera closed and opened again (a new size, codec or encoder)
        OFF,      // the camera turned off, was taken, failed, or the stream stopped
    }

    // Only losing the camera or its outputs ends a photo; a rebuilt session keeps them and the photo carries on
    fun interrupts(cause: Cause): Boolean = cause != Cause.SESSION

    // Text for a photo a rebuild ended
    fun text(cause: Cause): String = when (cause) {
        Cause.LENS -> "The lens changed during the photo"
        Cause.OFF -> "The camera stopped during the photo"
        else -> "The stream restarted during the photo"
    }

    enum class Lost { RETRY, ALONE, FAIL }

    // Captures the stream's session dropped as it was rebuilt: asked again once in the new session if the photo
    // outputs are still in it, or in a session of its own if they were dropped; a second loss fails the photo
    fun afterLostSession(retried: Boolean, outputsInSession: Boolean, sessionUp: Boolean): Lost = when {
        retried || !sessionUp -> Lost.FAIL
        outputsInSession -> Lost.RETRY
        else -> Lost.ALONE
    }
}

// Automatic session changes (Dynamic's frame rate, the encoder cap check, the heat) wait while a photo is set up
// or taken, so the photo's own hitch can't set off rebuilds under it; they run once it ends
class PhotoHold<T> {
    var busy = false
        private set
    private val held = LinkedHashMap<String, T>()

    fun start() { busy = true }

    // True when the change waits for the photo; a newer change of the same kind replaces the older one
    fun hold(kind: String, change: T): Boolean {
        if (!busy) return false
        held.remove(kind)
        held[kind] = change
        return true
    }

    // The photo ended: the changes that waited, oldest first
    fun end(): List<T> {
        busy = false
        return held.values.toList().also { held.clear() }
    }
}
