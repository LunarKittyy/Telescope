package com.telescope

import android.animation.ValueAnimator
import android.content.Context
import android.content.Intent
import android.graphics.Color
import android.graphics.Typeface
import android.graphics.drawable.GradientDrawable
import android.hardware.display.DisplayManager
import android.os.Build
import android.os.Bundle
import android.util.TypedValue
import android.view.Choreographer
import android.view.Display
import android.view.Gravity
import android.view.MotionEvent
import android.view.View
import android.view.WindowManager
import android.view.animation.DecelerateInterpolator
import android.widget.FrameLayout
import android.widget.ImageButton
import android.widget.TextView
import androidx.activity.OnBackPressedCallback
import androidx.appcompat.app.AppCompatActivity
import androidx.core.view.ViewCompat
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat

// The computer's notes on a black screen, the line being read right under the front camera; the stream runs on in the service
class NotesActivity : AppCompatActivity(), NotesBoard.Listener {

    private lateinit var root: FrameLayout
    private lateinit var column: FrameLayout
    private lateinit var text: TextView
    private lateinit var fadeTop: View
    private lateinit var fadeBottom: View

    private var shownText = ""
    private var appliedSize = 0
    private var state = NotesState()
    private var scroll = 0f  // pixels of the laid-out text above the reading line
    private var cutouts = emptyList<NotesPlacement.Box>()
    private var touching = false
    private var lastTouchY = 0f
    private var lastFrameNs = 0L
    private var jump: ValueAnimator? = null
    private var jumpTo = 0f  // where a running jump ends, so a second Next goes on from there
    private var ticking = false
    private var rotation = -1

    private val tick = object : Choreographer.FrameCallback {
        override fun doFrame(frameTimeNanos: Long) {
            if (!ticking) return
            val dt = if (lastFrameNs == 0L) 0f else (frameTimeNanos - lastFrameNs) / 1e9f
            lastFrameNs = frameTimeNanos
            if (state.scrolls && !touching && jump == null) {
                scrollTo(scroll + dt.coerceAtMost(0.1f) * pixelsPerSecond())
                // At the end there's nothing to move: new text, a size change, a drag or a jump starts it again
                if (text.layout != null && scroll >= maxScroll()) { stopTicking(); return }
            }
            Choreographer.getInstance().postFrameCallback(this)
        }
    }

    // A 180° turn keeps the size and, without a cutout, the insets, so only the display says the lens moved
    private val displays = object : DisplayManager.DisplayListener {
        override fun onDisplayAdded(displayId: Int) {}
        override fun onDisplayRemoved(displayId: Int) {}
        override fun onDisplayChanged(displayId: Int) {
            val shown = currentDisplay() ?: return
            if (displayId != shown.displayId || shown.rotation == rotation) return
            rotation = shown.rotation
            root.post { place() }
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        // The stream's own screen-on rules don't reach here: reading needs the screen, so it stays on while this shows
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        if (Build.VERSION.SDK_INT >= 27) setShowWhenLocked(true)
        else @Suppress("DEPRECATION") window.addFlags(WindowManager.LayoutParams.FLAG_SHOW_WHEN_LOCKED)
        if (Build.VERSION.SDK_INT >= 30) {
            window.attributes.layoutInDisplayCutoutMode = WindowManager.LayoutParams.LAYOUT_IN_DISPLAY_CUTOUT_MODE_ALWAYS
        } else if (Build.VERSION.SDK_INT >= 28) {
            window.attributes.layoutInDisplayCutoutMode =
                WindowManager.LayoutParams.LAYOUT_IN_DISPLAY_CUTOUT_MODE_SHORT_EDGES
        }
        WindowCompat.setDecorFitsSystemWindows(window, false)
        window.setBackgroundDrawable(null)

        root = FrameLayout(this).apply { setBackgroundColor(Color.BLACK) }
        column = TallColumn(this)
        text = TextView(this).apply {
            setTextColor(TEXT_COLOUR)
            typeface = Typeface.create("sans-serif-medium", Typeface.NORMAL)
            setLineSpacing(0f, 1.12f)
            includeFontPadding = false
            importantForAccessibility = View.IMPORTANT_FOR_ACCESSIBILITY_YES
        }
        column.addView(text, FrameLayout.LayoutParams(FrameLayout.LayoutParams.MATCH_PARENT,
                                                      FrameLayout.LayoutParams.WRAP_CONTENT))
        root.addView(column, FrameLayout.LayoutParams(0, 0))
        fadeTop = View(this).apply {
            background = GradientDrawable(GradientDrawable.Orientation.TOP_BOTTOM, intArrayOf(Color.BLACK, 0x99000000.toInt()))
        }
        fadeBottom = View(this).apply {
            background = GradientDrawable(GradientDrawable.Orientation.TOP_BOTTOM, intArrayOf(Color.TRANSPARENT, Color.BLACK))
        }
        root.addView(fadeTop, FrameLayout.LayoutParams(0, 0))
        root.addView(fadeBottom, FrameLayout.LayoutParams(0, 0))
        val close = ImageButton(this).apply {
            setImageResource(R.drawable.ic_close)
            setBackgroundResource(R.drawable.bg_overlay_tile)
            scaleType = android.widget.ImageView.ScaleType.FIT_CENTER
            contentDescription = getString(R.string.notes_close)
            alpha = 0.6f
            val pad = dp(12)
            setPadding(pad, pad, pad, pad)
            setOnClickListener { dismiss() }
        }
        root.addView(close, FrameLayout.LayoutParams(dp(44), dp(44), Gravity.BOTTOM or Gravity.END).apply {
            setMargins(dp(16), dp(16), dp(16), dp(16))
        })
        setContentView(root)

        WindowInsetsControllerCompat(window, root).apply {
            hide(WindowInsetsCompat.Type.systemBars())
            systemBarsBehavior = WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
        }
        ViewCompat.setOnApplyWindowInsetsListener(root) { _, insets ->
            cutouts = insets.displayCutout?.boundingRects.orEmpty().map {
                NotesPlacement.Box(it.left, it.top, it.right, it.bottom)
            }
            place()
            insets
        }
        root.addOnLayoutChangeListener { _, l, t, r, b, ol, ot, or, ob ->
            if (r - l != or - ol || b - t != ob - ot) root.post { place() }
        }
        text.addOnLayoutChangeListener { _, _, t, _, b, _, ot, _, ob -> if (b - t != ob - ot) resumeTicking() }
        root.setOnTouchListener { _, e -> drag(e) }
        onBackPressedDispatcher.addCallback(this, object : OnBackPressedCallback(true) {
            override fun handleOnBackPressed() = dismiss()
        })
    }

    override fun onStart() {
        super.onStart()
        NotesBoard.addListener(this)
        NotesBoard.setShown(true)
        rotation = currentDisplay()?.rotation ?: -1
        getSystemService(DisplayManager::class.java)?.registerDisplayListener(displays, null)
        onNotes(NotesBoard.state)
    }

    override fun onStop() {
        NotesBoard.setShown(false)
        NotesBoard.removeListener(this)
        getSystemService(DisplayManager::class.java)?.unregisterDisplayListener(displays)
        jump?.cancel()
        stopTicking()
        super.onStop()
    }

    private fun dismiss() {
        NotesBoard.dismissed = true
        finish()
    }

    // ── What the computer sent ────────────────────────────────────────────

    override fun onNotes(state: NotesState) {
        if (state.text.isBlank()) { finish(); return }
        this.state = state
        if (state.text != shownText) {
            shownText = state.text
            text.text = state.text
            jump?.cancel()
            scroll = 0f
            text.translationY = 0f
        }
        if (state.sizeSp != appliedSize) {
            appliedSize = state.sizeSp
            keepingPlace { text.setTextSize(TypedValue.COMPLEX_UNIT_SP, state.sizeSp.toFloat()); place() }
        }
        if (state.scrolls) startTicking() else stopTicking()
    }

    override fun onScroll(by: Int, toTop: Boolean) {
        val layout = text.layout ?: return
        val target = if (toTop) 0f else {
            val tops = NotesLines.starts(shownText).map { layout.getLineTop(layout.getLineForOffset(it)) }
            NotesLines.target(tops, (if (jump != null) jumpTo else scroll).toInt(), by).toFloat()
        }
        jump?.cancel()
        jumpTo = target.coerceIn(0f, maxScroll())
        jump = ValueAnimator.ofFloat(scroll, jumpTo).apply {
            duration = JUMP_MS
            interpolator = DecelerateInterpolator()
            addUpdateListener { scrollTo(it.animatedValue as Float) }
            addListener(object : android.animation.AnimatorListenerAdapter() {
                override fun onAnimationEnd(animation: android.animation.Animator) {
                    if (jump === animation) { jump = null; resumeTicking() }
                }
                override fun onAnimationCancel(animation: android.animation.Animator) { if (jump === animation) jump = null }
            })
            start()
        }
    }

    // ── Scrolling ─────────────────────────────────────────────────────────

    private fun scrollTo(y: Float) {
        scroll = y.coerceIn(0f, maxScroll())
        text.translationY = -scroll  // only moves the drawn text, nothing is laid out again
    }

    private fun maxScroll(): Float {
        val layout = text.layout ?: return 0f
        return layout.getLineTop(maxOf(0, layout.lineCount - 1)).toFloat()
    }

    private fun lineHeight(): Int = text.lineHeight.takeIf { it > 0 } ?: dp(state.sizeSp)

    // Speed 1 to 10 is 0.12 to 1.2 lines a second, about 7 to 70 lines a minute
    private fun pixelsPerSecond(): Float = state.speed * 0.12f * lineHeight()

    private fun startTicking() {
        if (ticking) return
        ticking = true
        lastFrameNs = 0L
        Choreographer.getInstance().postFrameCallback(tick)
    }

    private fun stopTicking() {
        ticking = false
        Choreographer.getInstance().removeFrameCallback(tick)
    }

    // Not once stopped: a jump ending or a late layout would scroll a screen nobody sees
    private fun resumeTicking() {
        if (state.scrolls && lifecycle.currentState.isAtLeast(androidx.lifecycle.Lifecycle.State.STARTED)) startTicking()
    }

    private fun drag(e: MotionEvent): Boolean {
        when (e.actionMasked) {
            MotionEvent.ACTION_DOWN -> { touching = true; lastTouchY = e.y; jump?.cancel() }
            MotionEvent.ACTION_MOVE -> { scrollTo(scroll + lastTouchY - e.y); lastTouchY = e.y }
            MotionEvent.ACTION_UP -> { touching = false; lastFrameNs = 0L; resumeTicking(); root.performClick() }
            MotionEvent.ACTION_CANCEL -> { touching = false; lastFrameNs = 0L; resumeTicking() }
        }
        return true
    }

    // Size or screen changes lay the text out again: the line being read stays the one at the reading line
    private fun keepingPlace(change: () -> Unit) {
        val layout = text.layout
        val offset = layout?.getLineStart(layout.getLineForVertical(scroll.toInt())) ?: 0
        change()
        text.post {
            val now = text.layout ?: return@post
            scrollTo(now.getLineTop(now.getLineForOffset(offset)).toFloat())
        }
    }

    // ── Where it goes ─────────────────────────────────────────────────────

    private fun place() {
        val w = root.width
        val h = root.height
        if (w == 0 || h == 0) return
        val lens = NotesPlacement.lens(w, h, cutouts, currentDisplay()?.rotation ?: 0)
        val p = NotesPlacement.place(w, h, lens, dp(MARGIN_DP), lineHeight())
        val c = p.column
        column.layoutParams = (column.layoutParams as FrameLayout.LayoutParams).apply {
            width = c.right - c.left; height = c.bottom - c.top; leftMargin = c.left; topMargin = c.top
            gravity = Gravity.TOP or Gravity.START
        }
        // The first line starts at the reading line, and the last one can scroll up to it
        text.setPadding(0, p.readingTop - c.top, 0, c.bottom - p.readingTop)
        fadeTop.layoutParams = (fadeTop.layoutParams as FrameLayout.LayoutParams).apply {
            width = c.right - c.left; leftMargin = c.left; topMargin = c.top
            height = p.readingTop - c.top; gravity = Gravity.TOP or Gravity.START
        }
        fadeTop.visibility = if (p.readingTop > c.top) View.VISIBLE else View.GONE
        val bottomFade = (c.bottom - c.top) / 5
        fadeBottom.layoutParams = (fadeBottom.layoutParams as FrameLayout.LayoutParams).apply {
            width = c.right - c.left; height = bottomFade; leftMargin = c.left; topMargin = c.bottom - bottomFade
            gravity = Gravity.TOP or Gravity.START
        }
        root.requestLayout()
        resumeTicking()
    }

    private fun currentDisplay(): Display? =
        if (Build.VERSION.SDK_INT >= 30) display else @Suppress("DEPRECATION") windowManager.defaultDisplay

    private fun dp(v: Int): Int = (v * resources.displayMetrics.density).toInt()

    companion object {
        private const val TEXT_COLOUR = 0xFFEDEFF2.toInt()  // not pure white: easier on the eyes in a dim room
        private const val MARGIN_DP = 14
        private const val JUMP_MS = 280L

        fun intent(context: Context): Intent = Intent(context, NotesActivity::class.java)
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP)
    }
}

// Lets the text be as tall as it is, not as tall as the column: the column only cuts off what's drawn outside it
private class TallColumn(context: Context) : FrameLayout(context) {
    override fun onMeasure(widthMeasureSpec: Int, heightMeasureSpec: Int) {
        super.onMeasure(widthMeasureSpec, heightMeasureSpec)
        val width = MeasureSpec.makeMeasureSpec(measuredWidth, MeasureSpec.EXACTLY)
        val tall = MeasureSpec.makeMeasureSpec(0, MeasureSpec.UNSPECIFIED)
        for (i in 0 until childCount) getChildAt(i).measure(width, tall)
    }
}
