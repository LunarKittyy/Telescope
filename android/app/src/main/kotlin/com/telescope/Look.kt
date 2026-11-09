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

    companion object {
        // What a new install wears, as on the desktop
        val DEFAULT = DARK
        private const val KEY = "look"

        fun of(key: String?): Look = entries.firstOrNull { it.key == key } ?: DEFAULT

        fun current(context: Context): Look = of(StreamPrefs.of(context).getString(KEY, null))

        fun save(context: Context, look: Look) {
            StreamPrefs.of(context).edit().putString(KEY, look.key).apply()
        }
    }
}

// Every screen of ours: wears the saved look, and rebuilds itself when the look changes while it is behind another screen
abstract class LookActivity : AppCompatActivity() {
    private lateinit var wearing: Look

    override fun onCreate(savedInstanceState: Bundle?) {
        wearing = Look.current(this)
        setTheme(wearing.theme)
        super.onCreate(savedInstanceState)
        WindowCompat.getInsetsController(window, window.decorView).isAppearanceLightNavigationBars = wearing.light
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

// A dialog in the look: a card's surface and corners, with the outline OLED gives a popup
fun Context.lookDialog(): MaterialAlertDialogBuilder {
    val shape = ShapeAppearanceModel.builder(this, R.style.ShapeAppearance_Telescope_Dialog, 0).build()
    val background = MaterialShapeDrawable(shape).apply {
        fillColor = attrColors(R.attr.tsCard)
        strokeColor = attrColors(R.attr.tsPopOutline)
        strokeWidth = attrPx(R.attr.tsPopOutlineW).toFloat()
    }
    return MaterialAlertDialogBuilder(this).setBackground(background)
}
