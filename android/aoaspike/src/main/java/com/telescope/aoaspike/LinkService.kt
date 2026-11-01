package com.telescope.aoaspike

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.hardware.usb.UsbAccessory
import android.hardware.usb.UsbManager
import android.os.IBinder
import android.os.ParcelFileDescriptor
import android.util.Log
import java.io.FileInputStream
import java.io.FileOutputStream
import java.net.InetAddress
import java.net.ServerSocket
import kotlin.concurrent.thread

// Foreground service holding the accessory and a loopback TCP twin of the same bench protocol
class LinkService : Service() {
    companion object {
        const val TAG = "AoaSpike"
        const val PERMISSION = "com.telescope.aoaspike.USB_PERMISSION"
    }

    private lateinit var usb: UsbManager
    private var pfd: ParcelFileDescriptor? = null
    private var tcpStarted = false

    private val events = object : BroadcastReceiver() {
        override fun onReceive(c: Context, i: Intent) {
            Log.i(TAG, "broadcast ${i.action} granted=${i.getBooleanExtra(UsbManager.EXTRA_PERMISSION_GRANTED, false)}")
            when (i.action) {
                UsbManager.ACTION_USB_ACCESSORY_DETACHED -> closeAccessory()
                PERMISSION -> tryOpen(i.accessoryExtra())
            }
        }
    }

    private fun Intent.accessoryExtra(): UsbAccessory? = getParcelableExtra(UsbManager.EXTRA_ACCESSORY)

    override fun onBind(i: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
        usb = getSystemService(UsbManager::class.java)
        val nm = getSystemService(NotificationManager::class.java)
        nm.createNotificationChannel(NotificationChannel("aoa", "AOA", NotificationManager.IMPORTANCE_LOW))
        startForeground(1, Notification.Builder(this, "aoa").setContentTitle("AoaSpike").setSmallIcon(android.R.drawable.stat_sys_data_bluetooth).build())
        val f = IntentFilter(UsbManager.ACTION_USB_ACCESSORY_DETACHED).apply { addAction(PERMISSION) }
        registerReceiver(events, f)
        Log.i(TAG, "service onCreate")
    }

    override fun onDestroy() {
        Log.i(TAG, "service onDestroy")
        closeAccessory()
        unregisterReceiver(events)
        super.onDestroy()
    }

    override fun onStartCommand(i: Intent?, flags: Int, id: Int): Int {
        if (!tcpStarted) {
            tcpStarted = true
            thread(name = "tcp") { tcpLoop() }
        }
        tryOpen(i?.accessoryExtra() ?: usb.accessoryList?.firstOrNull())
        return START_NOT_STICKY
    }

    @Synchronized
    private fun tryOpen(a: UsbAccessory?) {
        if (a == null) {
            Log.i(TAG, "no accessory present")
            return
        }
        if (pfd != null) {
            Log.i(TAG, "accessory already open")
            return
        }
        val perm = usb.hasPermission(a)
        Log.i(TAG, "accessory $a hasPermission=$perm")
        if (!perm) {
            val flags = PendingIntent.FLAG_MUTABLE or PendingIntent.FLAG_UPDATE_CURRENT
            val pi = PendingIntent.getBroadcast(this, 0, Intent(PERMISSION).setPackage(packageName), flags)
            usb.requestPermission(a, pi)
            return
        }
        val t0 = System.nanoTime()
        val fd = usb.openAccessory(a)
        if (fd == null) {
            Log.w(TAG, "openAccessory returned null")
            return
        }
        pfd = fd
        Log.i(TAG, "accessory open in ${(System.nanoTime() - t0) / 1000000} ms")
        thread(name = "acc") {
            val input = FileInputStream(fd.fileDescriptor)
            val output = FileOutputStream(fd.fileDescriptor)
            try {
                Session(input, output).run()
            } catch (e: Exception) {
                Log.i(TAG, "accessory session ended: $e")
            }
            closeAccessory()
        }
    }

    @Synchronized
    private fun closeAccessory() {
        val fd = pfd ?: return
        pfd = null
        Log.i(TAG, "closing accessory fd")
        try {
            fd.close()
        } catch (e: Exception) {
            Log.w(TAG, "close: $e")
        }
    }

    private fun tcpLoop() {
        val srv = ServerSocket(18777, 4, InetAddress.getByName("127.0.0.1"))
        Log.i(TAG, "tcp bench on 18777")
        while (true) {
            val s = srv.accept()
            s.tcpNoDelay = true
            thread(name = "tcp-session") {
                try {
                    Session(s.getInputStream(), s.getOutputStream()).run()
                } catch (e: Exception) {
                    Log.i(TAG, "tcp session ended: $e")
                }
                s.close()
            }
        }
    }
}
