package com.telescope

import android.content.Context
import android.content.res.ColorStateList
import android.graphics.drawable.GradientDrawable
import android.util.AttributeSet
import android.view.View
import android.view.Gravity
import android.widget.FrameLayout
import android.widget.ImageView
import android.widget.LinearLayout
import android.widget.TextView
import androidx.core.graphics.ColorUtils

// A card's title row as the desktop draws it (add_card_header in desktop/telescope/widgets/common.py): the icon in
// the card's hue on a square of the same hue, then the title
class CardHeader @JvmOverloads constructor(context: Context, attrs: AttributeSet? = null) : LinearLayout(context, attrs) {

    val title: TextView

    init {
        orientation = HORIZONTAL
        gravity = Gravity.CENTER_VERTICAL
        val a = context.obtainStyledAttributes(attrs, R.styleable.CardHeader)
        val hue = a.getColor(R.styleable.CardHeader_headerHue, context.getColor(R.color.colorAccent))
        val icon = a.getResourceId(R.styleable.CardHeader_headerIcon, 0)
        val text = a.getString(R.styleable.CardHeader_headerTitle)
        a.recycle()

        val density = resources.displayMetrics.density
        fun dp(v: Int) = (v * density).toInt()
        val chip = FrameLayout(context).apply {
            background = GradientDrawable().apply {
                cornerRadius = 9 * density
                setColor(ColorUtils.setAlphaComponent(hue, (0.14f * 255).toInt()))
            }
            importantForAccessibility = IMPORTANT_FOR_ACCESSIBILITY_NO
        }
        chip.addView(ImageView(context).apply {
            if (icon != 0) setImageResource(icon)
            imageTintList = ColorStateList.valueOf(hue)
        }, FrameLayout.LayoutParams(dp(18), dp(18), Gravity.CENTER))
        addView(chip, LayoutParams(dp(34), dp(34)))

        title = TextView(context).apply {
            this.text = text
            setTextAppearance(R.style.TextAppearance_Telescope_CardTitle)
        }
        addView(title, LayoutParams(0, LayoutParams.WRAP_CONTENT, 1f).apply { marginStart = dp(12) })
    }

    // The card's one button, at the end of the title row like the desktop's header action
    fun setAction(action: View) {
        addView(action, LayoutParams(LayoutParams.WRAP_CONTENT, LayoutParams.WRAP_CONTENT).apply {
            marginStart = (8 * resources.displayMetrics.density).toInt()
        })
    }
}
