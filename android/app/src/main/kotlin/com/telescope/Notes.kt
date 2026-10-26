package com.telescope

import android.os.Handler
import android.os.Looper

// The computer's notes for the screen next to the lens (NotesActivity): the controls that set them and what they are now.
data class NotesState(
    val text: String = "",
    val sizeSp: Int = NotesCommand.SIZE_DEFAULT,
    val speed: Int = NotesCommand.SPEED_DEFAULT,  // 0 = scrolled by hand only
    val playing: Boolean = false,
) {
    val scrolls: Boolean get() = playing && speed > 0
}

sealed class NotesCommand {
    data class Text(val text: String) : NotesCommand()
    data class Style(val sizeSp: Int?, val speed: Int?, val playing: Boolean?) : NotesCommand()
    data class Scroll(val by: Int, val toTop: Boolean) : NotesCommand()
    data class Refused(val error: String) : NotesCommand()

    companion object {
        const val SIZE_MIN = 16
        const val SIZE_MAX = 120
        const val SIZE_DEFAULT = 44
        const val SPEED_MAX = 10
        const val SPEED_DEFAULT = 3
        const val TEXT_MAX_CHARS = 60_000  // the control body's cap is 64 KiB; this is only a second fence

        // null: not a notes action, for the camera's own controls to handle
        fun parse(params: Map<String, String>): NotesCommand? = when (params["action"]) {
            "notes" -> {
                val text = params["text"]
                when {
                    text == null -> Refused("no text")
                    text.length > TEXT_MAX_CHARS -> Refused("text too long")
                    else -> Text(text.replace("\r\n", "\n").replace('\r', '\n'))
                }
            }
            "notes_style" -> {
                val size = params["size"]?.let { it.toIntOrNull() ?: return Refused("bad size") }
                val speed = params["speed"]?.let { it.toIntOrNull() ?: return Refused("bad speed") }
                val playing = params["playing"]?.let {
                    when (it) { "1" -> true; "0" -> false; else -> return Refused("bad playing") }
                }
                Style(size?.coerceIn(SIZE_MIN, SIZE_MAX), speed?.coerceIn(0, SPEED_MAX), playing)
            }
            "notes_scroll" -> when {
                params["to"] == "top" -> Scroll(0, toTop = true)
                params["by"] != null -> params["by"]?.toIntOrNull()?.let { Scroll(it.coerceIn(-100, 100), toTop = false) }
                    ?: Refused("bad by")
                else -> Refused("no by or to")
            }
            else -> null
        }
    }
}

fun NotesState.after(command: NotesCommand): NotesState = when (command) {
    is NotesCommand.Text -> copy(text = command.text)
    is NotesCommand.Style -> copy(
        sizeSp = command.sizeSp ?: sizeSp, speed = command.speed ?: speed, playing = command.playing ?: playing)
    else -> this
}

// Where each typed line starts and how a "next" or "previous" moves between them, in pixels down the laid-out text
object NotesLines {
    // Character offsets where a line that isn't blank starts
    fun starts(text: String): List<Int> {
        val out = mutableListOf<Int>()
        var at = 0
        for (line in text.split('\n')) {
            if (line.isNotBlank()) out.add(at)
            at += line.length + 1
        }
        return out
    }

    // tops: where each start sits, in order. by > 0 moves that many starts down from scroll, by < 0 up; mid-line,
    // the first step up goes back to the start of the line being read.
    fun target(tops: List<Int>, scroll: Int, by: Int): Int {
        if (tops.isEmpty() || by == 0) return scroll
        if (by > 0) {
            val after = tops.filter { it > scroll + 1 }
            return if (after.isEmpty()) tops.last().coerceAtLeast(scroll) else after[minOf(by, after.size) - 1]
        }
        val before = tops.filter { it < scroll - 1 }
        return if (before.isEmpty()) 0 else before[maxOf(0, before.size + by)]
    }
}

// Process-wide, since the service gets the controls and the activity shows them, and either can come and go.
object NotesBoard {
    interface Listener {
        fun onNotes(state: NotesState)
        fun onScroll(by: Int, toTop: Boolean)
    }

    @Volatile var state = NotesState()
        private set
    // Back was pressed on the notes: the app doesn't open them again by itself until new text comes
    @Volatile var dismissed = false
    @Volatile var shown = false
        private set

    private val listeners = mutableListOf<Listener>()
    private val main by lazy { Handler(Looper.getMainLooper()) }
    // Listeners hear on the main thread; tests run them right away
    internal var post: (() -> Unit) -> Unit = { main.post(it) }

    val hasNotes: Boolean get() = state.text.isNotBlank()
    val wantsScreen: Boolean get() = hasNotes && !dismissed

    // From any thread. True when new text came that should go on screen.
    fun apply(command: NotesCommand): Boolean {
        val now: NotesState
        synchronized(this) {
            now = state.after(command)
            state = now
            if (command is NotesCommand.Text) dismissed = false
        }
        val fresh = command is NotesCommand.Text && now.text.isNotBlank()
        post {
            if (command is NotesCommand.Scroll) listeners.forEach { it.onScroll(command.by, command.toTop) }
            else listeners.forEach { it.onNotes(now) }
        }
        return fresh
    }

    // The stream ended: the notes go with it
    fun clear() {
        val now: NotesState
        synchronized(this) { now = state.copy(text = ""); state = now; dismissed = false }
        post { listeners.forEach { it.onNotes(now) } }
    }

    fun setShown(on: Boolean) { shown = on }

    fun addListener(listener: Listener) { listeners.add(listener) }
    fun removeListener(listener: Listener) { listeners.remove(listener) }
}
