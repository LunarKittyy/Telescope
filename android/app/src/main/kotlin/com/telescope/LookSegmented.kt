package com.telescope

import android.content.Context
import android.graphics.Canvas
import android.graphics.Paint
import android.graphics.RectF
import android.util.AttributeSet
import android.view.Gravity
import android.view.ViewGroup
import android.widget.LinearLayout
import android.widget.TextView
import androidx.core.content.res.ResourcesCompat
import androidx.core.graphics.ColorUtils
import androidx.core.view.AccessibilityDelegateCompat
import androidx.core.view.ViewCompat
import androidx.core.view.accessibility.AccessibilityNodeInfoCompat

// The desktop's Segmented (ui-kit widgets/choices.slint): a raised square per choice, the picked one outlined in the card's colour by one shape that stretches onto it
class LookSegmented @JvmOverloads constructor(context: Context, attrs: AttributeSet? = null) : LinearLayout(context, attrs) {

    private val d = resources.displayMetrics.density
    private val radius = 8 * d
    private val stroke = 2 * d
    private val surface = RectF()
    private val paint = Paint(Paint.ANTI_ALIAS_FLAG)
    private val medium = ResourcesCompat.getFont(context, R.font.ibm_plex_sans_medium)
    private val semibold = ResourcesCompat.getFont(context, R.font.ibm_plex_sans_semibold)

    var hue = context.attrColor(R.attr.tsAccent)
    var onPicked: (Int) -> Unit = {}
    var selected = -1
        private set

    // The outline's two edges: the one it travels towards is quick and lands exactly, the other trails and passes its place a little
    private val left = Spring(0f, 5.83f / (Motion.WORM_LEAD_MS / 1000f), 1f)
    private val right = Spring(0f, 5.83f / (Motion.WORM_LEAD_MS / 1000f), 1f)
    private var placed = false
    private val frames = Frames(this) { dt ->
        left.step(dt)
        right.step(dt)
        invalidate()
        !(left.atRest && right.atRest)
    }

    init {
        orientation = HORIZONTAL
        setWillNotDraw(false)
        isFocusable = false
    }

    fun setChoices(labels: List<String>, pick: Int) {
        removeAllViews()
        labels.forEachIndexed { i, label ->
            val tile = Tile(context).apply {
                text = label
                gravity = Gravity.CENTER
                maxLines = 1
                ellipsize = android.text.TextUtils.TruncateAt.END
                textSize = 14f
                minHeight = (48 * d).toInt()
                isClickable = true
                isFocusable = true
                setPadding((6 * d).toInt(), 0, (6 * d).toInt(), 0)
                setOnClickListener { pick(i, fromUser = true) }
                // A radio choice to screen readers, so the picked one is told apart
                ViewCompat.setAccessibilityDelegate(this, object : AccessibilityDelegateCompat() {
                    override fun onInitializeAccessibilityNodeInfo(host: android.view.View, info: AccessibilityNodeInfoCompat) {
                        super.onInitializeAccessibilityNodeInfo(host, info)
                        info.className = android.widget.RadioButton::class.java.name
                        info.isCheckable = true
                        info.isChecked = host.isSelected
                    }
                })
            }
            addView(tile, LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f).apply {
                if (i > 0) marginStart = (7 * d).toInt()
            })
        }
        selected = -1
        placed = false
        pick(pick, fromUser = false)
    }

    private fun pick(i: Int, fromUser: Boolean) {
        if (i == selected) return
        selected = i
        for (k in 0 until childCount) {
            val tile = getChildAt(k) as TextView
            val on = k == i
            tile.isSelected = on
            tile.typeface = if (on) semibold else medium
            tile.setTextColor(context.attrColor(if (on) R.attr.tsInk else R.attr.tsInk2))
        }
        retarget()
        // The tile's own state change is not always spoken, so say the new pick
        if (fromUser) (getChildAt(i) as TextView).let { it.announceForAccessibility(context.getString(R.string.choice_selected, it.text)) }
        if (fromUser) onPicked(i)
    }

    // Where the picked square is: the outline goes there, at once the first time and when the screen is made, springing after
    private fun retarget() {
        val tile = getChildAt(selected) ?: return
        if (tile.width == 0) return
        val goesRight = tile.left.toFloat() > left.target
        // The leading edge is the side it is going towards
        left.settleIn(if (goesRight) Motion.WORM_TRAIL_MS else Motion.WORM_LEAD_MS)
        right.settleIn(if (goesRight) Motion.WORM_LEAD_MS else Motion.WORM_TRAIL_MS)
        left.target = tile.left.toFloat()
        right.target = tile.right.toFloat()
        if (placed && Motion.on()) {
            frames.start()
        } else {
            left.snap(left.target)
            right.snap(right.target)
            placed = true
            invalidate()
        }
    }

    override fun onLayout(changed: Boolean, l: Int, t: Int, r: Int, b: Int) {
        super.onLayout(changed, l, t, r, b)
        if (selected < 0) return
        val tile = getChildAt(selected) ?: return
        if (!placed || (changed && left.atRest)) {
            left.snap(tile.left.toFloat())
            right.snap(tile.right.toFloat())
            placed = true
        }
    }

    private fun inset(): Float = 2 * d

    // A square draws no surface of its own (the row does, under the outline), so a press has to ask the row to draw again
    private class Tile(context: Context) : androidx.appcompat.widget.AppCompatTextView(context) {
        override fun setPressed(pressed: Boolean) {
            super.setPressed(pressed)
            (parent as? android.view.View)?.invalidate()
        }
    }

    override fun dispatchDraw(canvas: Canvas) {
        val ctx = context
        val outlineW = ctx.attrPx(R.attr.tsOutlineW).toFloat()
        // The squares
        for (k in 0 until childCount) {
            val tile = getChildAt(k)
            surface.set(tile.left.toFloat(), tile.top + inset(), tile.right.toFloat(), tile.bottom - inset())
            paint.style = Paint.Style.FILL
            paint.color = if (tile.isPressed && k != selected) ctx.attrColor(R.attr.tsPress) else ctx.attrColor(R.attr.tsRaise)
            canvas.drawRoundRect(surface, radius, radius, paint)
            if (outlineW > 0f) {
                paint.style = Paint.Style.STROKE
                paint.strokeWidth = outlineW
                paint.color = ctx.attrColor(if (tile.isPressed && k != selected) R.attr.tsOutlinePress else R.attr.tsOutline)
                surface.inset(outlineW / 2f, outlineW / 2f)
                canvas.drawRoundRect(surface, radius - outlineW / 2f, radius - outlineW / 2f, paint)
            }
        }
        val tile = getChildAt(selected)
        if (tile != null) {
            val top = tile.top + inset()
            val bottom = tile.bottom - inset()
            val l = minOf(left.value, right.value)
            val r = maxOf(left.value, right.value)
            // The wash inside, under the words
            surface.set(l, top, r, bottom)
            paint.style = Paint.Style.FILL
            val pressed = tile.isPressed
            paint.color = ColorUtils.blendARGB(ctx.attrColor(R.attr.tsCard), hue, if (pressed) 0.34f else 0.16f)
            canvas.drawRoundRect(surface, radius, radius, paint)
            super.dispatchDraw(canvas)
            // The outline, over everything
            paint.style = Paint.Style.STROKE
            paint.strokeWidth = stroke
            paint.color = hue
            surface.inset(stroke / 2f, stroke / 2f)
            canvas.drawRoundRect(surface, radius - stroke / 2f, radius - stroke / 2f, paint)
        } else {
            super.dispatchDraw(canvas)
        }
    }
}
