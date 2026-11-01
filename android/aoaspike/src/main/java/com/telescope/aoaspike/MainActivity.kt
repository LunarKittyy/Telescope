package com.telescope.aoaspike

import android.app.Activity
import android.content.Intent
import android.hardware.usb.UsbManager
import android.os.Bundle
import android.util.Log
import android.widget.TextView

class MainActivity : Activity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(TextView(this).apply { text = "AoaSpike"; textSize = 32f })
        handle(intent)
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        handle(intent)
    }

    private fun handle(i: Intent) {
        Log.i(LinkService.TAG, "activity intent ${i.action} accessory=${i.getParcelableExtra<android.os.Parcelable>(UsbManager.EXTRA_ACCESSORY)}")
        // Rotates this activity alone, so the test needs no system rotation setting
        if (i.hasExtra("orient")) requestedOrientation = i.getIntExtra("orient", -1)
        startForegroundService(Intent(this, LinkService::class.java).apply { i.extras?.let { putExtras(it) } })
    }

    override fun onStart() {
        super.onStart()
        Log.i(LinkService.TAG, "activity onStart")
    }

    override fun onStop() {
        super.onStop()
        Log.i(LinkService.TAG, "activity onStop")
    }
}
