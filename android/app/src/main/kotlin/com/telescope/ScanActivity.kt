package com.telescope

import android.os.Bundle
import android.view.Gravity
import android.view.ViewGroup
import android.widget.TextView
import com.journeyapps.barcodescanner.CaptureActivity

// The library's scanner screen, kept out from under the system bars so its prompt stays visible
class ScanActivity : CaptureActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        SystemBars.pad(findViewById(android.R.id.content))
        // Lifted off the bottom edge, where it sat flush against the navigation bar
        findViewById<TextView>(com.google.zxing.client.android.R.id.zxing_status_view).apply {
            (layoutParams as ViewGroup.MarginLayoutParams).bottomMargin = (48 * resources.displayMetrics.density).toInt()
            gravity = Gravity.CENTER
            val side = (24 * resources.displayMetrics.density).toInt()
            setPadding(side, 0, side, 0)
        }
    }
}
