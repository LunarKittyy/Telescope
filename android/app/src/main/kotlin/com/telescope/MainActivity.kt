package com.telescope

import android.Manifest
import android.content.ClipData
import android.content.ClipboardManager
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.ServiceConnection
import android.content.pm.PackageManager
import android.hardware.camera2.CameraManager
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import android.os.PowerManager
import android.provider.Settings
import android.view.View
import android.widget.*
import androidx.appcompat.app.AppCompatActivity
import androidx.cardview.widget.CardView
import com.google.android.material.button.MaterialButton
import com.google.android.material.dialog.MaterialAlertDialogBuilder
import androidx.core.app.ActivityCompat
import androidx.core.content.ContextCompat
import com.journeyapps.barcodescanner.ScanContract
import com.journeyapps.barcodescanner.ScanOptions

class MainActivity : AppCompatActivity() {

    private lateinit var spinnerCamera: Spinner
    private lateinit var spinnerResolution: Spinner
    private lateinit var btnToggle: MaterialButton
    private lateinit var checkOis: CheckBox
    private lateinit var checkLocalOnly: CheckBox
    private lateinit var tvStatus: TextView
    private lateinit var tvCameraList: TextView
    private lateinit var layoutLinks: View
    private lateinit var tvLinkWifi: TextView
    private lateinit var tvLinkUsb: TextView
    private lateinit var btnScanQr: ImageButton
    private lateinit var btnResetPairing: ImageButton
    private lateinit var btnPreview: ImageButton
    private lateinit var cardPermissions: CardView
    private lateinit var layoutPermissionsContainer: LinearLayout
    private lateinit var btnCopyDiagnostics: MaterialButton
    private var _permissionsRequested = false

    private val prefs by lazy { getSharedPreferences("telescope", MODE_PRIVATE) }

    private var service: CameraStreamService? = null
    private var bound = false
    private var cameras = listOf<CameraInfo>()

    private val uiHandler = Handler(Looper.getMainLooper())
    private val statusPoller = object : Runnable {
        override fun run() {
            updateStatusText()
            uiHandler.postDelayed(this, 1000)
        }
    }

    private val serviceConnection = object : ServiceConnection {
        override fun onServiceConnected(name: ComponentName?, binder: IBinder?) {
            service = (binder as CameraStreamService.LocalBinder).getService()
            bound = true
            updateStatusText()
        }
        override fun onServiceDisconnected(name: ComponentName?) {
            service = null
            bound = false
            updateStatusText()
        }
    }

    private val scanLauncher = registerForActivityResult(ScanContract()) { result ->
        result.contents?.let { handleQrScan(it) }
    }

    // Registered/unregistered with onStart()/onStop() rather than the manifest:
    // only needs to work while this screen is actually in front (same
    // requirement the QR flow already has - you have to be looking at the app
    // to scan a code). Gated on the DUMP permission at registration (see
    // onStart()) so it's reachable by adb but not by another app on the phone.
    private val pairReceiver = object : android.content.BroadcastReceiver() {
        override fun onReceive(context: Context, intent: Intent) {
            // Base64-encoded on the desktop side so the JSON's braces/quotes
            // never have to survive adb shell's remote command-line parsing.
            intent.getStringExtra(EXTRA_PAIR_PAYLOAD)?.let {
                runCatching { String(android.util.Base64.decode(it, android.util.Base64.DEFAULT)) }
                    .getOrNull()
                    ?.let(::handleQrScan)
            }
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_main)

        spinnerCamera     = findViewById(R.id.spinnerCamera)
        spinnerResolution = findViewById(R.id.spinnerResolution)
        btnToggle         = findViewById<MaterialButton>(R.id.btnToggle)
        checkOis          = findViewById(R.id.checkOis)
        tvStatus          = findViewById(R.id.tvStatus)
        tvCameraList      = findViewById(R.id.tvCameraList)
        layoutLinks       = findViewById(R.id.layoutLinks)
        tvLinkWifi        = findViewById(R.id.tvLinkWifi)
        tvLinkUsb         = findViewById(R.id.tvLinkUsb)
        checkLocalOnly             = findViewById(R.id.checkLocalOnly)
        btnScanQr                  = findViewById(R.id.btnScanQr)
        btnResetPairing            = findViewById(R.id.btnResetPairing)
        btnPreview                 = findViewById(R.id.btnPreview)
        cardPermissions            = findViewById(R.id.cardPermissions)
        layoutPermissionsContainer = findViewById(R.id.layoutPermissionsContainer)
        btnCopyDiagnostics         = findViewById(R.id.btnCopyDiagnostics)

        checkLocalOnly.isChecked = prefs.getBoolean("local_only", false)
        checkLocalOnly.setOnCheckedChangeListener { _, checked ->
            prefs.edit().putBoolean("local_only", checked).apply()
            if (service?.isStreaming == true) {
                service?.stopStreaming()
                if (bound) { unbindService(serviceConnection); bound = false; service = null }
                startStream()
            }
        }

        tvLinkWifi.setOnClickListener { copyLink(tvLinkWifi) }
        tvLinkUsb.setOnClickListener  { copyLink(tvLinkUsb) }

        btnToggle.setOnClickListener { onToggleClicked() }
        btnPreview.setOnClickListener { startActivity(Intent(this, PreviewActivity::class.java)) }
        btnScanQr.setOnClickListener {
            if (service?.isStreaming == true) {
                service?.stopStreaming()
                if (bound) { unbindService(serviceConnection); bound = false; service = null }
                updateStatusText()
            }
            val opts = ScanOptions().apply {
                setPrompt("Scan the Telescope QR code on your desktop")
                setBeepEnabled(false)
                setOrientationLocked(false)
                setBarcodeImageEnabled(false)
            }
            scanLauncher.launch(opts)
        }
        btnResetPairing.setOnClickListener { confirmResetPairing() }
        btnCopyDiagnostics.setOnClickListener { copyDiagnostics() }

        spinnerCamera.onItemSelectedListener = object : AdapterView.OnItemSelectedListener {
            override fun onItemSelected(p: AdapterView<*>?, v: android.view.View?, pos: Int, id: Long) {
                populateResolutionSpinner(pos)
            }
            override fun onNothingSelected(p: AdapterView<*>?) {}
        }

        checkPermissions()
    }

    override fun onResume() {
        super.onResume()
        checkPermissions()
    }

    override fun onStart() {
        super.onStart()
        bindService(Intent(this, CameraStreamService::class.java), serviceConnection, 0)
        uiHandler.post(statusPoller)
        // RECEIVER_NOT_EXPORTED silently drops this broadcast entirely - "not
        // exported" means only senders sharing this app's own UID qualify,
        // and adb shell (uid 2000, "shell") doesn't. Exported is required for
        // adb to reach it at all, so it's gated on the DUMP permission
        // instead: shell holds it by default, but no ordinary third-party
        // app can acquire it, so another app on the phone still can't
        // trigger a silent re-pair.
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            registerReceiver(
                pairReceiver, IntentFilter(ACTION_PAIR),
                Manifest.permission.DUMP, null, Context.RECEIVER_EXPORTED,
            )
        } else {
            @Suppress("DEPRECATION")
            registerReceiver(pairReceiver, IntentFilter(ACTION_PAIR), Manifest.permission.DUMP, null)
        }
    }

    override fun onStop() {
        uiHandler.removeCallbacks(statusPoller)
        if (bound) { unbindService(serviceConnection); bound = false }
        unregisterReceiver(pairReceiver)
        super.onStop()
    }

    // ── QR pairing ────────────────────────────────────────────────────────────

    private fun handleQrScan(data: String) {
        try {
            val json = org.json.JSONObject(data)
            val version = json.optInt("version", 0)
            if (version != 1) {
                Toast.makeText(this, "Invalid QR code.", Toast.LENGTH_SHORT).show()
                return
            }
            val port = json.getInt("port")
            val nonce = json.getString("nonce")
            val token = json.getString("token")
            val ipsJson = json.getJSONArray("ips")
            val desktopIps = (0 until ipsJson.length()).map { ipsJson.getString(it) }
            val myIps = getAllDeviceIps()
            val deviceName = Build.MODEL

            Thread {
                var success = false
                val errors = mutableListOf<String>()
                for (ip in desktopIps) {
                    try {
                        val url = java.net.URL("http://$ip:$port/pair/$nonce")
                        val conn = url.openConnection() as java.net.HttpURLConnection
                        conn.requestMethod = "POST"
                        conn.setRequestProperty("Content-Type", "application/json")
                        conn.connectTimeout = 2000
                        conn.readTimeout = 2000
                        conn.doOutput = true
                        val body = org.json.JSONObject().apply {
                            put("name", deviceName)
                            put("ips", org.json.JSONArray(myIps))
                            // Echoed back so the desktop can confirm this POST actually
                            // came from a phone that read the current QR code, on top
                            // of the one-shot nonce already baked into the URL path.
                            put("token", token)
                        }.toString()
                        conn.outputStream.write(body.toByteArray())
                        if (conn.responseCode == 200) {
                            success = true
                            break
                        } else {
                            errors += "$ip: HTTP ${conn.responseCode}"
                        }
                    } catch (e: Exception) {
                        errors += "$ip: ${e.javaClass.simpleName}: ${e.message}"
                    }
                }
                if (success) {
                    // Becomes this phone's only accepted bearer token for /v1/* -
                    // replaces (revokes) whatever was paired before.
                    TokenStore.save(this, token)
                }
                val msg = if (success)
                    "Paired! Desktop will add this device."
                else
                    "Could not reach desktop.\nTried: ${errors.joinToString(", ")}"
                runOnUiThread {
                    Toast.makeText(this, msg, Toast.LENGTH_LONG).show()
                }
            }.start()
        } catch (_: Exception) {
            Toast.makeText(this, "Invalid QR code.", Toast.LENGTH_SHORT).show()
        }
    }

    /** A single tap used to instantly wipe the pairing token with no way back
     *  short of re-pairing from scratch - one stray touch on this button (it
     *  sits right next to others on the same row) was enough to lock a user
     *  out of their own desktop until they could get back on the same
     *  network. Confirm first. */
    private fun confirmResetPairing() {
        MaterialAlertDialogBuilder(this)
            .setTitle("Unpair this phone?")
            .setMessage("The desktop app will need to pair again (QR code or USB) before it can reconnect.")
            .setNegativeButton("Cancel", null)
            .setPositiveButton("Unpair") { _, _ -> resetPairing() }
            .show()
    }

    /** Clears the stored pairing token and, if currently streaming, restarts the
     *  service so its MjpegServer picks up the cleared state - every further
     *  request 401s until the phone is paired again. */
    private fun resetPairing() {
        TokenStore.clear(this)
        if (service?.isStreaming == true) {
            service?.stopStreaming()
            if (bound) { unbindService(serviceConnection); bound = false; service = null }
            startStream()
        }
        updateStatusText()
        Toast.makeText(this, "Pairing reset. Scan a new QR code to reconnect.", Toast.LENGTH_LONG).show()
    }

    // ── Permissions ────────────────────────────────────────────────────────────

    private data class PermInfo(
        val permission: String?,   // null = battery optimization
        val label: String,
        val reason: String
    )

    private fun checkPermissions() {
        val missing = mutableListOf<PermInfo>()

        if (ContextCompat.checkSelfPermission(this, Manifest.permission.CAMERA)
                != PackageManager.PERMISSION_GRANTED)
            missing += PermInfo(Manifest.permission.CAMERA, "Camera",
                "Required to access your phone's cameras.")

        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU &&
                ContextCompat.checkSelfPermission(this, Manifest.permission.POST_NOTIFICATIONS)
                != PackageManager.PERMISSION_GRANTED)
            missing += PermInfo(Manifest.permission.POST_NOTIFICATIONS, "Notifications",
                "Required to show the persistent streaming notification.")

        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.M) {
            val pm = getSystemService(POWER_SERVICE) as PowerManager
            if (!pm.isIgnoringBatteryOptimizations(packageName))
                missing += PermInfo(null, "Battery optimization",
                    "Disable battery restrictions so the stream isn't killed in the background.")
        }

        val cameraGranted = ContextCompat.checkSelfPermission(this, Manifest.permission.CAMERA) ==
                PackageManager.PERMISSION_GRANTED

        layoutPermissionsContainer.removeAllViews()
        if (missing.isEmpty()) {
            cardPermissions.visibility = android.view.View.GONE
        } else {
            // On first call, proactively request runtime permissions via system dialog.
            // Battery optimization has no requestPermissions() path - stays as a manual button.
            if (!_permissionsRequested) {
                _permissionsRequested = true
                val requestable = missing.mapNotNull { it.permission }
                if (requestable.isNotEmpty()) {
                    ActivityCompat.requestPermissions(this, requestable.toTypedArray(), RC_PERMS)
                    return
                }
            }
            cardPermissions.visibility = android.view.View.VISIBLE
            missing.forEach { info -> layoutPermissionsContainer.addView(buildPermRow(info)) }
        }

        // Camera permission is the only hard requirement to use the app.
        if (cameraGranted && cameras.isEmpty()) loadCameras()
    }

    private fun buildPermRow(info: PermInfo): android.view.View {
        val row = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = android.view.Gravity.CENTER_VERTICAL
            setPadding(0, 0, 0, 16)
        }

        val textBlock = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            layoutParams = LinearLayout.LayoutParams(0,
                LinearLayout.LayoutParams.WRAP_CONTENT, 1f)
        }
        TextView(this).apply {
            text = info.label
            textSize = 13f
            setTextColor(resources.getColor(R.color.colorOnSurface, theme))
            setTypeface(null, android.graphics.Typeface.BOLD)
            textBlock.addView(this)
        }
        TextView(this).apply {
            text = info.reason
            textSize = 12f
            setTextColor(resources.getColor(R.color.colorOnSurfaceDim, theme))
            textBlock.addView(this)
        }
        row.addView(textBlock)

        val btn = com.google.android.material.button.MaterialButton(
            this, null, com.google.android.material.R.attr.materialButtonOutlinedStyle
        ).apply {
            val perm = info.permission
            if (perm == null) {
                // Battery optimization
                text = "Allow"
                setOnClickListener {
                    startActivity(Intent(Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS)
                        .apply { data = Uri.parse("package:$packageName") })
                }
            } else if (ActivityCompat.shouldShowRequestPermissionRationale(this@MainActivity, perm)) {
                text = "Grant"
                setOnClickListener {
                    ActivityCompat.requestPermissions(this@MainActivity,
                        arrayOf(perm), RC_PERMS)
                }
            } else {
                text = "Open Settings"
                setOnClickListener { openAppSettings() }
            }
            layoutParams = LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.WRAP_CONTENT,
                LinearLayout.LayoutParams.WRAP_CONTENT
            ).apply { marginStart = 12 }
        }
        row.addView(btn)
        return row
    }

    private fun openAppSettings() {
        startActivity(Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS)
            .apply { data = Uri.parse("package:$packageName") })
    }

    override fun onRequestPermissionsResult(
        requestCode: Int, permissions: Array<String>, grantResults: IntArray
    ) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults)
        if (requestCode == RC_PERMS) checkPermissions()
    }

    // ── Camera enumeration ─────────────────────────────────────────────────────

    private fun loadCameras() {
        val manager = getSystemService(CAMERA_SERVICE) as CameraManager
        val sb      = StringBuilder()

        cameras = CameraCatalog.enumerate(manager, sb)
        tvCameraList.text = sb.toString().trimEnd()

        val adapter = ArrayAdapter(this,
            R.layout.spinner_item, cameras.map { it.label }
        ).also { it.setDropDownViewResource(R.layout.spinner_dropdown_item) }
        spinnerCamera.adapter = adapter

        if (cameras.isNotEmpty()) populateResolutionSpinner(0)
    }

    private fun populateResolutionSpinner(cameraIndex: Int) {
        if (cameraIndex < 0 || cameraIndex >= cameras.size) return
        val cam = cameras[cameraIndex]
        checkOis.isEnabled = cam.hasOis
        checkOis.isChecked = cam.hasOis

        val labels = cam.supportedSizes.map { "${it.width} x ${it.height}" }
        val adapter = ArrayAdapter(this,
            R.layout.spinner_item, labels
        ).also { it.setDropDownViewResource(R.layout.spinner_dropdown_item) }
        spinnerResolution.adapter = adapter

        val default1080 = cam.supportedSizes.indexOfFirst { it.width == 1920 && it.height == 1080 }
        spinnerResolution.setSelection(if (default1080 >= 0) default1080 else 0)
    }

    // ── Controls ───────────────────────────────────────────────────────────────

    private fun onToggleClicked() {
        if (service?.isStreaming == true) {
            service?.stopStreaming()
            if (bound) { unbindService(serviceConnection); bound = false; service = null }
            updateStatusText()
        } else {
            startStream()
        }
    }

    private fun startStream() {
        val camIdx = spinnerCamera.selectedItemPosition
        val resIdx = spinnerResolution.selectedItemPosition
        if (cameras.isEmpty() || camIdx < 0 || camIdx >= cameras.size) return
        val cam  = cameras[camIdx]
        val size = cam.supportedSizes.getOrNull(resIdx) ?: cam.supportedSizes.first()

        val intent = Intent(this, CameraStreamService::class.java).apply {
            putExtra(CameraStreamService.EXTRA_CAMERA_ID,   cam.id)
            putExtra(CameraStreamService.EXTRA_LOGICAL_ID,  cam.logicalId ?: "")
            putExtra(CameraStreamService.EXTRA_WIDTH,        size.width)
            putExtra(CameraStreamService.EXTRA_HEIGHT,       size.height)
            putExtra(CameraStreamService.EXTRA_OIS,          checkOis.isChecked && cam.hasOis)
            putExtra(CameraStreamService.EXTRA_LOCAL_ONLY,   checkLocalOnly.isChecked)
        }
        ContextCompat.startForegroundService(this, intent)

        if (bound) { unbindService(serviceConnection); bound = false }
        bindService(Intent(this, CameraStreamService::class.java), serviceConnection, 0)
    }

    // ── Status ─────────────────────────────────────────────────────────────────

    private fun updateStatusText() {
        val streaming = service?.isStreaming == true
        btnToggle.text = if (streaming) "Stop Streaming" else "Start Streaming"
        if (streaming) {
            val ip   = getDeviceIp()
            val port = service?.port ?: CameraStreamService.DEFAULT_PORT
            tvStatus.text = "● Streaming"
            tvStatus.setTextColor(resources.getColor(R.color.colorStreamingText, theme))
            tvLinkWifi.text = "WiFi  http://$ip:$port/video"
            tvLinkUsb.text  = "USB   http://localhost:$port/video"
            tvLinkWifi.visibility = if (checkLocalOnly.isChecked) View.GONE else View.VISIBLE
            layoutLinks.visibility = View.VISIBLE
        } else {
            tvStatus.text = "○ Not streaming"
            tvStatus.setTextColor(resources.getColor(R.color.colorOnSurfaceDim, theme))
            tvLinkWifi.visibility = View.VISIBLE
            layoutLinks.visibility = View.GONE
        }
    }

    private fun copyLink(pill: TextView) {
        val url = pill.text.toString().substringAfter("  ")
        val clipboard = getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
        clipboard.setPrimaryClip(ClipData.newPlainText("Telescope URL", url))

        val original = pill.text
        pill.text = "✓ Copied"
        pill.setBackgroundResource(R.drawable.pill_link_copied)
        pill.setTextColor(resources.getColor(R.color.colorPrimary, theme))

        uiHandler.postDelayed({
            pill.text = original
            pill.setBackgroundResource(R.drawable.pill_link)
            pill.setTextColor(resources.getColor(R.color.colorOnSurface, theme))
        }, 1200)
    }

    private fun copyDiagnostics() {
        val report = service?.buildDiagnosticsReport() ?: "Telescope diagnostics\n(not running)"
        val clipboard = getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
        clipboard.setPrimaryClip(ClipData.newPlainText("Telescope diagnostics", report))
        Toast.makeText(this, "Diagnostics copied to clipboard", Toast.LENGTH_SHORT).show()
    }

    private fun getAllDeviceIps(): List<String> {
        return try {
            java.net.NetworkInterface.getNetworkInterfaces()
                ?.asSequence()
                ?.filter { it.isUp && !it.isLoopback }
                ?.flatMap { it.inetAddresses.asSequence() }
                ?.filter { it is java.net.Inet4Address && !it.isLoopbackAddress }
                ?.mapNotNull { it.hostAddress }
                ?.toList() ?: emptyList()
        } catch (_: Exception) { emptyList() }
    }

    private fun getDeviceIp(): String = getAllDeviceIps().firstOrNull() ?: "unknown"

    companion object {
        private const val RC_PERMS = 100
        // Lets the desktop app push a pairing payload straight over adb when
        // there's no camera-scannable QR code involved (USB pairing) - the
        // same JSON shape and handleQrScan() logic as the QR flow, just
        // delivered a different way.
        const val ACTION_PAIR = "com.telescope.action.PAIR"
        const val EXTRA_PAIR_PAYLOAD = "payload"
    }
}
