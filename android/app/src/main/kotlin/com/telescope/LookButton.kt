package com.telescope

import android.content.Context
import android.content.res.ColorStateList
import android.util.AttributeSet
import androidx.annotation.AttrRes
import androidx.annotation.ColorInt
import androidx.core.content.res.ResourcesCompat
import androidx.core.graphics.ColorUtils
import com.google.android.material.button.MaterialButton

// The desktop's Button (ui-kit widgets/basics.slint) in its roles; OLED draws the outline where the other looks have a fill
class LookButton @JvmOverloads constructor(
    context: Context,
    attrs: AttributeSet? = null,
    defStyle: Int = com.google.android.material.R.attr.materialButtonStyle,
) : MaterialButton(context, attrs, defStyle) {

    enum class Role { PLAIN, QUIET, PRIMARY, DANGER, STOP, UPDATE }

    private var hue = context.attrColor(R.attr.tsAccent)

    var role: Role = Role.PLAIN
        set(value) {
            field = value
            dressAs(value, hue)
        }

    init {
        val a = context.obtainStyledAttributes(attrs, R.styleable.LookButton)
        hue = a.getColor(R.styleable.LookButton_buttonHue, hue)
        role = Role.entries[a.getInt(R.styleable.LookButton_role, 0)]
        a.recycle()
        isAllCaps = false
        typeface = ResourcesCompat.getFont(context, R.font.ibm_plex_sans_semibold)
        // The press is the fill going a step darker, as on the desktop, so no ripple
        rippleColor = ColorStateList.valueOf(0)
    }

    // The card's colour, for the main action of a card
    fun setRole(role: Role, @ColorInt hue: Int) {
        this.hue = hue
        this.role = role
    }
}


private fun states(disabled: Int, pressed: Int, normal: Int) = ColorStateList(
    arrayOf(intArrayOf(-android.R.attr.state_enabled), intArrayOf(android.R.attr.state_pressed), intArrayOf()),
    intArrayOf(disabled, pressed, normal),
)

// Gives a Material button a role's fill, ink and edge (LookButton does it for its own; a dialog's buttons come from Material and are dressed here)
fun MaterialButton.dressAs(role: LookButton.Role, @ColorInt hue: Int) {
    fun c(@AttrRes attr: Int) = context.attrColor(attr)
    val dis = c(R.attr.tsDis)
    val trough = c(R.attr.tsTrough)
    val disEdge = c(R.attr.tsDisOutline)
    var edgeW = context.attrPx(R.attr.tsOutlineW)
    val filled = role != LookButton.Role.PLAIN && role != LookButton.Role.QUIET
    val fill: ColorStateList
    val ink: Int
    val edge: ColorStateList
    when (role) {
        LookButton.Role.PLAIN, LookButton.Role.QUIET -> {
            fill = states(trough, c(R.attr.tsSel), c(R.attr.tsRaise))
            ink = if (role == LookButton.Role.QUIET) c(R.attr.tsInk2) else c(R.attr.tsInk)
            edge = states(disEdge, c(R.attr.tsOutlinePress), c(R.attr.tsOutline))
        }
        LookButton.Role.PRIMARY -> {
            val classic = Look.current(context) == Look.CLASSIC
            // Classic keeps the violet; the other looks fill with the card's own colour
            val face = if (classic) c(R.attr.tsFill) else hue
            fill = states(trough, if (classic) c(R.attr.tsFillPress) else ColorUtils.blendARGB(hue, c(R.attr.tsShade), 0.15f), face)
            ink = if (classic) c(R.attr.tsOnFill) else c(R.attr.tsOnHue)
            edge = states(disEdge, 0, 0)
            edgeW = 0
        }
        LookButton.Role.DANGER -> {
            fill = states(trough, c(R.attr.tsDangerPress), c(R.attr.tsDanger))
            ink = c(R.attr.tsOnFill)
            edge = states(disEdge, c(R.attr.tsDangerOutline), c(R.attr.tsDangerOutline))
        }
        LookButton.Role.STOP -> {
            fill = states(trough, c(R.attr.tsStopPress), c(R.attr.tsStopFill))
            ink = c(R.attr.tsStopInk)
            edge = states(disEdge, c(R.attr.tsStopEdge), c(R.attr.tsStopEdge))
            edgeW = context.attrPx(R.attr.tsStopEdgeW)
        }
        LookButton.Role.UPDATE -> {
            fill = states(trough, c(R.attr.tsUpdatePress), c(R.attr.tsUpdateFill))
            ink = c(R.attr.tsOnFill)
            edge = states(disEdge, 0, 0)
            edgeW = 0
        }
    }
    backgroundTintList = fill
    setTextColor(states(dis, ink, ink))
    iconTint = states(dis, ink, if (filled) ink else c(R.attr.tsInk2))
    strokeColor = edge
    strokeWidth = if (edgeW > 0) edgeW else 0
    rippleColor = ColorStateList.valueOf(0)
}
