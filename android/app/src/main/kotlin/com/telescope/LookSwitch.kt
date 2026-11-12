package com.telescope

import android.content.Context
import android.graphics.Canvas
import android.graphics.Paint
import android.graphics.Rect
import android.graphics.RectF
import android.util.AttributeSet
import android.view.Gravity
import android.view.accessibility.AccessibilityNodeInfo
import android.widget.CompoundButton
import androidx.core.content.res.ResourcesCompat
import androidx.core.graphics.ColorUtils

// The desktop's switch (ui-kit widgets/switch.slint) at touch size: a track with a square handle that says I or O, so the state never rests on colour alone
class LookSwitch @JvmOverloads constructor(context: Context, attrs: AttributeSet? = null) : CompoundButton(context, attrs) {

    private val d = resources.displayMetrics.density
    private val trackW = 52 * d
    private val trackH = 28 * d
    private val radius = 7 * d
    private val handleRadius = 4.5f * d
    private val margin = 3 * d
    private val stroke = 2 * d
    private var hue = context.attrColor(R.attr.tsAccent)
    private var danger = false
    // The room the layout left after the track, which sits at the end of the row
    private var endInset = 0f

    // 0 off to 1 on, as the colours follow it; and where the handle is, which passes either end a little while it flicks
    private val tone = Spring.settling(Motion.TONE_UP_MS)
    private val place = Spring.bouncing(Motion.FLICK_MS, 0.22f)
    private val frames = Frames(this) { dt ->
        tone.step(dt)
        place.step(dt)
        invalidate()
        !(tone.atRest && place.atRest)
    }

    private val paint = Paint(Paint.ANTI_ALIAS_FLAG)
    private val glyph = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        textSize = 11 * d
        typeface = ResourcesCompat.getFont(context, R.font.ibm_plex_sans_semibold)
    }
    private val rect = RectF()
    private val bounds = Rect()

    init {
        buttonDrawable = null
        gravity = Gravity.CENTER_VERTICAL or Gravity.START
        minHeight = (48 * d).toInt()
        minimumHeight = minHeight
        endInset = paddingEnd.toFloat()
        setPaddingRelative(paddingStart, paddingTop, paddingEnd + (trackW + 12 * d).toInt(), paddingBottom)
        isClickable = true
        val a = context.obtainStyledAttributes(attrs, R.styleable.LookSwitch)
        hue = a.getColor(R.styleable.LookSwitch_switchHue, hue)
        danger = a.getBoolean(R.styleable.LookSwitch_switchDanger, false)
        a.recycle()
        tone.snap(if (isChecked) 1f else 0f)
        place.snap(if (isChecked) 1f else 0f)
    }

    // The card's colour, for a switch made in code
    fun setHue(colour: Int) {
        hue = colour
        invalidate()
    }

    override fun setChecked(checked: Boolean) {
        val changed = checked != isChecked
        super.setChecked(checked)
        if (!changed) return
        val goal = if (checked) 1f else 0f
        tone.target = goal
        place.target = goal
        if (isLaidOut && Motion.on()) {
            // Going on takes the quicker colour change, going off the slower, as the desktop's Tone does
            tone.settleIn(if (checked) Motion.TONE_UP_MS else Motion.TONE_DOWN_MS)
            frames.start()
        } else {
            tone.snap(goal)
            place.snap(goal)
            invalidate()
        }
    }

    override fun setPressed(pressed: Boolean) {
        super.setPressed(pressed)
        invalidate()
    }

    override fun drawableStateChanged() {
        super.drawableStateChanged()
        invalidate()
    }

    override fun onDraw(canvas: Canvas) {
        super.onDraw(canvas)
        val ctx = context
        val rtl = layoutDirection == LAYOUT_DIRECTION_RTL
        val x0 = if (rtl) endInset else width - trackW - endInset
        val y0 = (height - trackH) / 2f
        val on = tone.value.coerceIn(0f, 1f)
        val held = isEnabled && isPressed
        val lit = if (danger) ctx.attrColor(R.attr.tsErr) else hue
        val card = ctx.attrColor(R.attr.tsCard)

        // The surface
        val offFill = when {
            !isEnabled -> ctx.attrColor(R.attr.tsTrough)
            held -> ctx.attrColor(R.attr.tsPress)
            else -> ctx.attrColor(R.attr.tsRaise)
        }
        val onFill = ColorUtils.blendARGB(card, lit, if (held) 0.34f else 0.16f)
        val fill = if (!isEnabled) offFill else ColorUtils.blendARGB(offFill, onFill, on)
        rect.set(x0, y0, x0 + trackW, y0 + trackH)
        paint.style = Paint.Style.FILL
        paint.color = fill
        canvas.drawRoundRect(rect, radius, radius, paint)

        // The border: the outline OLED draws at rest, the thick one in the card's colour when on
        val offEdge = when {
            !isEnabled -> ctx.attrColor(R.attr.tsDisOutline)
            held -> ctx.attrColor(R.attr.tsOutlinePress)
            else -> ctx.attrColor(R.attr.tsOutline)
        }
        val offW = when {
            !isEnabled -> ctx.attrPx(R.attr.tsOutlineW)
            held -> ctx.attrPx(R.attr.tsOutlineWOver)
            else -> ctx.attrPx(R.attr.tsOutlineW)
        }.toFloat()
        val edgeW = if (!isEnabled) offW else offW + (stroke - offW) * on
        if (edgeW > 0.01f) {
            val from = if (ColorUtils.setAlphaComponent(offEdge, 0) == offEdge) ColorUtils.setAlphaComponent(lit, 0) else offEdge
            paint.style = Paint.Style.STROKE
            paint.strokeWidth = edgeW
            paint.color = if (!isEnabled) offEdge else ColorUtils.blendARGB(from, lit, on)
            val half = edgeW / 2f
            rect.set(x0 + half, y0 + half, x0 + trackW - half, y0 + trackH - half)
            canvas.drawRoundRect(rect, radius - half, radius - half, paint)
        }

        // The handle, which flicks between the ends (and goes a little past, so the move is seen)
        val size = trackH - 2 * margin
        val travel = trackW - size - 2 * margin
        val p = if (rtl) 1f - place.value else place.value
        val hx = x0 + margin + p * travel
        val hy = y0 + margin
        val handleColour = when {
            !isEnabled -> ctx.attrColor(R.attr.tsSel)
            else -> ColorUtils.blendARGB(
                if (held) ctx.attrColor(R.attr.tsInk2) else ctx.attrColor(R.attr.tsLineHover), lit, on)
        }
        rect.set(hx, hy, hx + size, hy + size)
        paint.style = Paint.Style.FILL
        paint.color = handleColour
        canvas.drawRoundRect(rect, handleRadius, handleRadius, paint)

        // I when on and O when off, changing as the handle passes the middle; centred on its ink, not its line box
        val over = place.value > 0.5f
        val text = if (over) "I" else "O"
        glyph.color = when {
            !isEnabled -> ctx.attrColor(R.attr.tsDis)
            over || held -> card
            else -> ctx.attrColor(R.attr.tsInk2)
        }
        glyph.getTextBounds(text, 0, 1, bounds)
        canvas.drawText(text, rect.centerX() - bounds.exactCenterX(), rect.centerY() - bounds.exactCenterY(), glyph)
    }

    override fun onInitializeAccessibilityNodeInfo(info: AccessibilityNodeInfo) {
        super.onInitializeAccessibilityNodeInfo(info)
        info.className = "android.widget.Switch"
    }
}
