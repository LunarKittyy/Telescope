package com.telescope

// Why H.264 stopped and the stream went back to MJPEG. The state sends the code and the size so the desktop can
// word it in its own language; [text] is the English that desktops which don't know the code show.
data class CodecFailure(val code: String, val width: Int, val height: Int) {
    // Everything but a stopped encoder is the phone not doing this size or rate, which trying again won't change
    val unsupported: Boolean get() = code != ENCODER_STOPPED

    val text: String get() = when (code) {
        UNAVAILABLE -> "H.264 isn't available at ${width}x$height on this phone"
        CAMERA_CANT_FEED -> "The camera can't feed H.264 at ${width}x$height on this phone"
        CAMERA_STOPPED -> "H.264 at ${width}x$height stopped the camera on this phone"
        else -> "The phone's H.264 encoder stopped"
    }

    companion object {
        const val UNAVAILABLE = "h264_unavailable"
        const val CAMERA_CANT_FEED = "camera_cant_feed"
        const val CAMERA_STOPPED = "camera_stopped"
        const val ENCODER_STOPPED = "encoder_stopped"
    }
}
