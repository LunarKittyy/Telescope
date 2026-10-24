package com.telescope

import android.view.View
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat

// Android 15+ draws every app behind the status and navigation bars. top takes the status bar's height
// (its background shows behind it), root takes the navigation bar and display cutouts.
object SystemBars {
    fun pad(root: View, top: View = root) {
        val topBase = top.paddingTop
        val left = root.paddingLeft
        val right = root.paddingRight
        val bottom = root.paddingBottom
        ViewCompat.setOnApplyWindowInsetsListener(root) { _, insets ->
            val bars = insets.getInsets(WindowInsetsCompat.Type.systemBars() or WindowInsetsCompat.Type.displayCutout())
            top.setPadding(top.paddingLeft, topBase + bars.top, top.paddingRight, top.paddingBottom)
            root.setPadding(left + bars.left, root.paddingTop, right + bars.right, bottom + bars.bottom)
            insets
        }
    }
}
