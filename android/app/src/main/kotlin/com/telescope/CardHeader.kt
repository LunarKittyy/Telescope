package com.telescope

import android.content.Context
import android.content.res.ColorStateList
import android.util.AttributeSet
import android.view.View
import android.view.Gravity
import android.widget.ImageView
import android.widget.LinearLayout
import android.widget.TextView

// A card's title row as the desktop draws it (ui-kit widgets/card.slint): the icon bare in the card's hue, then the title, in a row 44dp tall
class CardHeader @JvmOverloads constructor(context: Context, attrs: AttributeSet? = null) : LinearLayout(context, attrs) {

    val title: TextView

    init {
        orientation = HORIZONTAL
        gravity = Gravity.CENTER_VERTICAL
        val a = context.obtainStyledAttributes(attrs, R.styleable.CardHeader)
        val hue = a.getColor(R.styleable.CardHeader_headerHue, context.attrColor(R.attr.tsAccent))
        val icon = a.getResourceId(R.styleable.CardHeader_headerIcon, 0)
        val text = a.getString(R.styleable.CardHeader_headerTitle)
        a.recycle()

        val density = resources.displayMetrics.density
        fun dp(v: Int) = (v * density).toInt()
        minimumHeight = dp(44)
        addView(ImageView(context).apply {
            if (icon != 0) setImageResource(icon)
            imageTintList = ColorStateList.valueOf(hue)
            importantForAccessibility = IMPORTANT_FOR_ACCESSIBILITY_NO
        }, LayoutParams(dp(20), dp(20)))

        title = TextView(context).apply {
            this.text = text
            setTextAppearance(R.style.TextAppearance_Telescope_CardTitle)
        }
        addView(title, LayoutParams(0, LayoutParams.WRAP_CONTENT, 1f).apply { marginStart = dp(11) })
    }

    // The card's one button, at the end of the title row like the desktop's header action
    fun setAction(action: View) {
        addView(action, LayoutParams(LayoutParams.WRAP_CONTENT, LayoutParams.WRAP_CONTENT).apply {
            marginStart = (8 * resources.displayMetrics.density).toInt()
        })
    }
}
