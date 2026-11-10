package com.telescope

import android.content.Context
import android.content.res.ColorStateList
import android.os.Bundle
import android.util.TypedValue
import android.view.ContextThemeWrapper
import androidx.annotation.AttrRes
import androidx.annotation.ColorInt
import androidx.annotation.StringRes
import androidx.annotation.StyleRes
import androidx.appcompat.app.AppCompatActivity
import androidx.core.graphics.ColorUtils
import androidx.core.view.WindowCompat
import com.google.android.material.dialog.MaterialAlertDialogBuilder
import com.google.android.material.shape.MaterialShapeDrawable
import com.google.android.material.shape.ShapeAppearanceModel

// The four looks, in the order the desktop's Appearance lists them. Each is a theme (res/values/look_themes.xml,
// generated from the desktop's theme.slint) that gives the ts* attributes their colours.
enum class Look(val key: String, @StyleRes val theme: Int, @StringRes val label: Int) {
    DARK("dark", R.style.Theme_Telescope_Dark, R.string.look_dark),
    CLASSIC("classic", R.style.Theme_Telescope_Classic, R.string.look_classic),
    OLED("oled", R.style.Theme_Telescope_Oled, R.string.look_oled),
    LIGHT("light", R.style.Theme_Telescope_Light, R.string.look_light);

    // Dark icons in the navigation bar, as the theme gives the status bar's
    val light get() = this == LIGHT

    // The look a screen over live video or a black surface wears: Light's pale tiles and dark ink would vanish there, so it borrows Dark's
    val overVideo get() = if (this == LIGHT) DARK else this

    companion object {
        // What an install that never picked a look wears, as on the desktop (no look saved is Dark; the phone never had a classic_theme to carry over)
        val DEFAULT = DARK
        private const val KEY = "look"

        fun of(key: String?): Look = entries.firstOrNull { it.key == key } ?: DEFAULT

        fun current(context: Context): Look = of(StreamPrefs.of(context).getString(KEY, null))

        fun save(context: Context, look: Look) {
            StreamPrefs.of(context).edit().putString(KEY, look.key).apply()
            // A notification already up keeps the old colour until it is posted again
            val service = CameraStreamService.instance
            val streaming = service?.isStreaming == true
            if (streaming) service?.refreshNotification()
            if (waitingNotificationNeedsRefresh(streaming, service?.coveredByWaiting == true)) WaitingService.showStreaming(context, false)
        }

        // The waiting notification is its own unless a stream under it already refreshed it as the stream's
        fun waitingNotificationNeedsRefresh(streaming: Boolean, streamCovered: Boolean) = !(streaming && streamCovered)
    }
}

// Every screen of ours: wears the saved look, and rebuilds itself when the look changes while it is behind another screen
abstract class LookActivity : AppCompatActivity() {
    private lateinit var wearing: Look

    // The look this screen is drawn in for the saved one; screens over video or black override it with `overVideo`
    protected open fun dressedIn(saved: Look): Look = saved

    override fun onCreate(savedInstanceState: Bundle?) {
        wearing = Look.current(this)
        val dressed = dressedIn(wearing)
        setTheme(dressed.theme)
        super.onCreate(savedInstanceState)
        WindowCompat.getInsetsController(window, window.decorView).isAppearanceLightNavigationBars = dressed.light
    }

    override fun onResume() {
        super.onResume()
        if (Look.current(this) != wearing) recreate()
    }
}

// A theme colour or size for code that draws by hand
@ColorInt
fun Context.attrColor(@AttrRes attr: Int): Int {
    val value = TypedValue()
    check(theme.resolveAttribute(attr, value, true)) { "attribute $attr is not set in this theme" }
    return if (value.resourceId != 0) getColor(value.resourceId) else value.data
}

fun Context.attrColors(@AttrRes attr: Int): ColorStateList = ColorStateList.valueOf(attrColor(attr))

fun Context.attrPx(@AttrRes attr: Int): Int {
    val value = TypedValue()
    return if (theme.resolveAttribute(attr, value, true)) TypedValue.complexToDimensionPixelSize(value.data, resources.displayMetrics) else 0
}

// The saved look's colour for code with no screen in front (a notification's accent)
@ColorInt
fun Context.lookColor(@AttrRes attr: Int): Int = ContextThemeWrapper(this, Look.current(this).theme).attrColor(attr)

// The desktop's tint(hue): a little of a card's colour over the card, for the inside of an active control
@ColorInt
fun Context.tint(@ColorInt hue: Int): Int = ColorUtils.blendARGB(attrColor(R.attr.tsCard), hue, 0.16f)

// A dialog in the look: a card's surface and corners, with the outline OLED gives a popup. It comes in with a short fade and scale (none with animations off).
fun Context.lookDialog(): MaterialAlertDialogBuilder {
    val shape = ShapeAppearanceModel.builder(this, R.style.ShapeAppearance_Telescope_Dialog, 0).build()
    val background = MaterialShapeDrawable(shape).apply {
        fillColor = attrColors(R.attr.tsCard)
        strokeColor = attrColors(R.attr.tsPopOutline)
        strokeWidth = attrPx(R.attr.tsPopOutlineW).toFloat()
    }
    return MaterialAlertDialogBuilder(this).setBackground(background)
}

// The desktop's held(hue): more of the card's colour over the card, for a pressed active control
@ColorInt
fun Context.held(@ColorInt hue: Int): Int = ColorUtils.blendARGB(attrColor(R.attr.tsCard), hue, 0.34f)

// Shows the dialog with its buttons as the desktop's: Cancel and the like raised, the action (primary, or danger for a removal) last
fun MaterialAlertDialogBuilder.showLook(action: LookButton.Role = LookButton.Role.PRIMARY): androidx.appcompat.app.AlertDialog {
    val dialog = create()
    dialog.setOnShowListener {
        val ctx = dialog.context
        dialog.window?.decorView?.let { popIn(it) }
        val d = ctx.resources.displayMetrics.density
        val accent = ctx.attrColor(R.attr.tsAccent)
        listOf(androidx.appcompat.app.AlertDialog.BUTTON_POSITIVE, androidx.appcompat.app.AlertDialog.BUTTON_NEGATIVE, androidx.appcompat.app.AlertDialog.BUTTON_NEUTRAL)
            .mapNotNull { dialog.getButton(it) as? com.google.android.material.button.MaterialButton }
            .forEach { button ->
                val positive = button === dialog.getButton(androidx.appcompat.app.AlertDialog.BUTTON_POSITIVE)
                button.apply {
                    isAllCaps = false
                    typeface = androidx.core.content.res.ResourcesCompat.getFont(ctx, R.font.ibm_plex_sans_semibold)
                    textSize = 14f
                    cornerRadius = (8 * d).toInt()
                    insetTop = (2 * d).toInt()
                    insetBottom = (2 * d).toInt()
                    minHeight = (48 * d).toInt()
                    minWidth = (80 * d).toInt()
                    stateListAnimator = null
                    dressAs(if (positive) action else LookButton.Role.PLAIN, accent)
                }
            }
        // The row of buttons sits as far in from the dialog's edge as the text does, and the buttons a card's gap apart
        (dialog.getButton(androidx.appcompat.app.AlertDialog.BUTTON_POSITIVE)?.parent as? android.view.ViewGroup)?.apply {
            setPaddingRelative((20 * d).toInt(), (8 * d).toInt(), (20 * d).toInt(), (18 * d).toInt())
            for (i in 0 until childCount) (getChildAt(i).layoutParams as? android.view.ViewGroup.MarginLayoutParams)?.apply {
                marginStart = (6 * d).toInt()
                marginEnd = 0
            }
        }
    }
    dialog.show()
    return dialog
}

// The desktop's dialog arrives with a fade and a small scale; at most 200 ms, and nothing at all when Android's animations are off
fun popIn(view: android.view.View) {
    if (!Motion.on()) return
    view.alpha = 0f
    view.scaleX = POP_FROM
    view.scaleY = POP_FROM
    view.animate().alpha(1f).scaleX(1f).scaleY(1f).setDuration(POP_MS)
        .setInterpolator(android.view.animation.DecelerateInterpolator()).start()
}

private const val POP_MS = 180L
private const val POP_FROM = 0.94f
