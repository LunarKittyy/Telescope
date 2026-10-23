package com.telescope

import android.os.Bundle
import com.journeyapps.barcodescanner.CaptureActivity

// The library's scanner screen, kept out from under the system bars so its prompt stays visible
class ScanActivity : CaptureActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        SystemBars.pad(findViewById(android.R.id.content))
    }
}
