package com.telescope

// Why /v1/audio says no. The refusal carries the code so the desktop can word it in its own language; [text] is
// the English that desktops which don't know the code show.
enum class MicProblem(val code: String, val text: String) {
    NO_MIC("mic_none", "No microphone"),
    NOT_STREAMING("mic_not_streaming", "Not streaming"),
    PERMISSION("mic_permission", "Allow the microphone in Telescope on the phone"),
    OPEN_APP("mic_open_app", "Open Telescope on the phone once, then try again"),
    IN_USE("mic_in_use", "The phone's microphone is in use"),
}
