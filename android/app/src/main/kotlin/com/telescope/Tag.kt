package com.telescope

import android.content.Context
import android.graphics.drawable.GradientDrawable
import android.text.TextUtils
import android.view.Gravity
import android.widget.TextView
import androidx.annotation.ColorInt
import androidx.core.content.res.ResourcesCompat
import androidx.core.graphics.ColorUtils

// The desktop's Tag (ui-kit widgets/setting.slint): a small label in a colour at 14% under its text in that colour, with a control's corners
fun Context.lookTag(text: String, @ColorInt tint: Int): TextView {
    val d = resources.displayMetrics.density
    return TextView(this).apply {
        this.text = text
        textSize = 12f
        typeface = ResourcesCompat.getFont(context, R.font.ibm_plex_sans_semibold)
        setTextColor(ColorUtils.blendARGB(tint, 0xFFFFFFFF.toInt(), 0.08f))
        gravity = Gravity.CENTER_VERTICAL
        maxLines = 1
        ellipsize = TextUtils.TruncateAt.END
        minHeight = (24 * d).toInt()
        setPadding((9 * d).toInt(), 0, (9 * d).toInt(), 0)
        background = GradientDrawable().apply {
            cornerRadius = 8 * d
            setColor(ColorUtils.setAlphaComponent(tint, 36))
        }
    }
}
