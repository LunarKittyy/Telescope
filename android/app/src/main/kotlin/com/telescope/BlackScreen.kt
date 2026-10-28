package com.telescope

import android.os.SystemClock
import android.view.MotionEvent
import android.view.View
import android.view.ViewGroup
import android.view.WindowManager
import android.widget.TextView
import androidx.activity.OnBackPressedCallback
import androidx.appcompat.app.AppCompatActivity
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat

// Covers the main screen in pure black while the stream keeps going, so the app stays in front with the screen on.
// Only the window changes (bars, brightness); the camera and encoder never hear about it.
class BlackScreen(private val activity: AppCompatActivity, private val status: () -> CharSequence) {
    private val state = BlackScreenState()
    private val content: ViewGroup = activity.findViewById(android.R.id.content)
    private val overlay: View = activity.layoutInflater.inflate(R.layout.black_screen, content, false)
    private val hint: View = overlay.findViewById(R.id.blackHint)
    private val hintStatus: TextView = overlay.findViewById(R.id.blackStatus)
    private val fadeHint = Runnable {
        state.hintGone()
        hint.animate().alpha(0f).setDuration(FADE_MS)
    }
    private val back = object : OnBackPressedCallback(false) {
        override fun handleOnBackPressed() = leave()
    }
    private var swallowing = false

    init {
        overlay.visibility = View.GONE
        overlay.setOnClickListener {
            if (state.tap(SystemClock.uptimeMillis()) == BlackScreenState.Tap.Wake) leave() else showHint()
        }
        content.addView(overlay)
        activity.onBackPressedDispatcher.addCallback(activity, back)
    }

    fun enter() {
        if (state.black) return
        state.enter()
        overlay.visibility = View.VISIBLE
        back.isEnabled = true
        bars().apply {
            systemBarsBehavior = WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
            hide(WindowInsetsCompat.Type.systemBars())
        }
        // Android raises this to the lowest brightness the screen allows; the screen stays on (FLAG_KEEP_SCREEN_ON)
        setBrightness(WindowManager.LayoutParams.BRIGHTNESS_OVERRIDE_OFF)
        showHint()
    }

    fun leave() {
        if (overlay.visibility == View.GONE) return
        state.leave()
        overlay.visibility = View.GONE
        overlay.removeCallbacks(fadeHint)
        hint.animate().cancel()
        back.isEnabled = false
        bars().show(WindowInsetsCompat.Type.systemBars())
        setBrightness(WindowManager.LayoutParams.BRIGHTNESS_OVERRIDE_NONE)
    }

    fun onStreamState(streaming: Boolean, busy: Boolean) {
        if (state.shouldLeave(streaming, busy)) leave()
    }

    // Whole gestures that start just after a double tap woke the screen are dropped, so a third tap can't press Stop
    fun swallow(ev: MotionEvent): Boolean {
        if (ev.actionMasked == MotionEvent.ACTION_DOWN) swallowing = state.justWoke(SystemClock.uptimeMillis())
        return swallowing
    }

    private fun showHint() {
        hintStatus.text = status()
        hint.animate().cancel()
        hint.alpha = 1f
        overlay.removeCallbacks(fadeHint)
        overlay.postDelayed(fadeHint, BlackScreenState.HINT_MS)
    }

    private fun bars() = WindowCompat.getInsetsController(activity.window, activity.window.decorView)

    private fun setBrightness(value: Float) {
        val window = activity.window
        window.attributes = window.attributes.apply { screenBrightness = value }
    }

    companion object {
        private const val FADE_MS = 600L
    }
}
