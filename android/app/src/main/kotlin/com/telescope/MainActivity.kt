package com.telescope

import android.Manifest
import android.content.ClipData
import android.content.ClipboardManager
import android.content.ComponentName
import android.content.Context
import android.content.res.ColorStateList
import android.content.Intent
import android.content.IntentFilter
import android.content.ServiceConnection
import android.content.pm.PackageManager
import android.hardware.camera2.CameraManager
import android.net.ConnectivityManager
import android.net.NetworkCapabilities
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


    private var service: CameraStreamService? = null
    private var bound = false
    private var cameras = listOf<CameraInfo>()
    // Prevents double-start race; cleared once service connects.
    private var starting = false

    // Named so it can be detached when driving spinnerCamera programmatically.
    private val cameraSpinnerListener = object : AdapterView.OnItemSelectedListener {
        override fun onItemSelected(p: AdapterView<*>?, v: android.view.View?, pos: Int, id: Long) {
            populateResolutionSpinner(pos)
        }
        override fun onNothingSelected(p: AdapterView<*>?) {}
    }

    private val uiHandler = Handler(Looper.getMainLooper())
    private val statusPoller = object : Runnable {
        override fun run() {
            adoptRemoteStart()
            updateStatusText()
            syncLiveControlsToState()
            uiHandler.postDelayed(this, 1000)
        }
    }

    private val serviceConnection = object : ServiceConnection {
        override fun onServiceConnected(name: ComponentName?, binder: IBinder?) {
            service = (binder as CameraStreamService.LocalBinder).getService()
            bound = true
            starting = false
            updateStatusText()
        }
        override fun onServiceDisconnected(name: ComponentName?) {
            starting = false
            service = null
            bound = false
            updateStatusText()
        }
    }

    private val scanLauncher = registerForActivityResult(ScanContract()) { result ->
        result.contents?.let { handleQrScan(it) }
    }

    // Registered at runtime; gated on DUMP permission (adb-only, not other apps).
    private val pairReceiver = object : android.content.BroadcastReceiver() {
        override fun onReceive(context: Context, intent: Intent) {
            // Base64-encoded to survive adb shell command-line parsing.
            intent.getStringExtra(EXTRA_PAIR_PAYLOAD)?.let {
                runCatching { String(android.util.Base64.decode(it, android.util.Base64.DEFAULT)) }
                    .getOrNull()
                    ?.let(::handleQrScan)
            }
        }
    }

    // Toast "started from your desktop" only once per remote start.
    private var remoteStartAnnounced = false

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

        checkLocalOnly.isChecked = StreamPrefs.localOnly(this)
        checkLocalOnly.setOnCheckedChangeListener { _, checked ->
            StreamPrefs.setLocalOnly(this, checked)
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

        spinnerCamera.onItemSelectedListener = cameraSpinnerListener

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
        // Reachable while screen is up; service holds reference after screen goes dark
        SessionEndpoint.acquire(this, SessionEndpoint.OWNER_ACTIVITY)
        // RECEIVER_EXPORTED is required for adb, but gated on DUMP permission (shell-only)
        ContextCompat.registerReceiver(
            this, pairReceiver, IntentFilter(ACTION_PAIR),
            Manifest.permission.DUMP, null, ContextCompat.RECEIVER_EXPORTED,
        )
    }

    override fun onStop() {
        uiHandler.removeCallbacks(statusPoller)
        if (bound) { unbindService(serviceConnection); bound = false }
        unregisterReceiver(pairReceiver)
        SessionEndpoint.release(SessionEndpoint.OWNER_ACTIVITY)
        super.onStop()
    }

    private fun handleQrScan(data: String) {
        when (val parsed = parsePairingOffer(data)) {
            is PairingParse.Invalid ->
                Toast.makeText(this, "Invalid QR code.", Toast.LENGTH_SHORT).show()
            is PairingParse.UnsupportedVersion ->
                Toast.makeText(
                    this,
                    "This QR code comes from a different Telescope version. Update the " +
                        "desktop app and this app to the same release, then try again.",
                    Toast.LENGTH_LONG,
                ).show()
            is PairingParse.Ok -> startPairing(parsed.offer)
        }
    }

    // Tries pairing POST at each address; LAN tries first over Wi-Fi to work through VPNs
    private fun startPairing(offer: PairingOffer) {
        val wifi = wifiNetwork()
        val routes = pairingRoutes(offer.candidates, hasWifi = wifi != null)
        val myIps = getAllDeviceIps(wifi)
        val deviceName = Build.MODEL

        Thread {
            val failures = mutableListOf<PairingAttemptFailure>()
            val startedAt = android.os.SystemClock.elapsedRealtime()
            var success = false
            var untried = 0
            for ((index, route) in routes.withIndex()) {
                // Bound wait per candidate; many candidates can't cause long delays.
                val timeout = attemptTimeoutMs(android.os.SystemClock.elapsedRealtime() - startedAt)
                if (timeout == null) {
                    untried = routes.size - index
                    break
                }
                val network = if (route.via == PairingRouteKind.WIFI) wifi else null
                val problem =
                    attemptPair(offer, route.candidate, network, deviceName, myIps, timeout)
                if (problem == null) {
                    success = true
                    break
                }
                failures += PairingAttemptFailure(route.candidate.ip, route.via, problem)
            }
            if (success) {
                // Becomes this phone's only accepted bearer token for /v1/* -
                // replaces (revokes) whatever was paired before.
                TokenStore.save(this, offer.token)
            }
            runOnUiThread {
                // MjpegServer snapshots token at startup; stop to pick up new token.
                if (success) {
                    if (service?.isStreaming == true) {
                        service?.stopStreaming()
                        if (bound) { unbindService(serviceConnection); bound = false; service = null }
                        updateStatusText()
                    }
                    Toast.makeText(
                        this, "Paired! Desktop will add this device.", Toast.LENGTH_LONG,
                    ).show()
                } else {
                    // Show in dialog for readability (too long for toast).
                    if (!isFinishing && !isDestroyed) {
                        showPairingFailure(pairingFailureMessage(failures, untried))
                    }
                }
            }
        }.start()
    }

    /** Returns null on success, or a short description of what went wrong. */
    private fun attemptPair(
        offer: PairingOffer,
        candidate: PairingCandidate,
        network: android.net.Network?,
        deviceName: String,
        myIps: List<String>,
        timeoutMs: Int,
    ): String? {
        var conn: java.net.HttpURLConnection? = null
        return try {
            val url = java.net.URL("http://${candidate.ip}:${offer.port}/pair/${offer.nonce}")
            // Pin to Wi-Fi network if available; default otherwise.
            val opened = network?.openConnection(url) ?: url.openConnection()
            conn = (opened as java.net.HttpURLConnection).apply {
                requestMethod = "POST"
                setRequestProperty("Content-Type", "application/json")
                connectTimeout = timeoutMs
                readTimeout = timeoutMs
                doOutput = true
            }
            val body = org.json.JSONObject().apply {
                put("name", deviceName)
                put("ips", org.json.JSONArray(myIps))
                // Echoed back; defense-in-depth along with nonce in URL path.
                put("token", offer.token)
            }.toString()
            conn.outputStream.use { it.write(body.toByteArray()) }
            if (conn.responseCode == 200) null else "HTTP ${conn.responseCode}"
        } catch (e: Exception) {
            describeNetworkError(e)
        } finally {
            try { conn?.disconnect() } catch (_: Exception) {}
        }
    }

    private fun showPairingFailure(message: String) {
        MaterialAlertDialogBuilder(this)
            .setTitle("Pairing failed")
            .setMessage(message)
            .setPositiveButton("OK", null)
            .show()
    }

    // Connected Wi-Fi network (not validated/internet-filtered; uses NOT_VPN to exclude VPN)
    private fun wifiNetwork(): android.net.Network? = try {
        val cm = getSystemService(Context.CONNECTIVITY_SERVICE) as ConnectivityManager
        @Suppress("DEPRECATION")  // no non-deprecated way to enumerate networks
        cm.allNetworks.firstOrNull { network ->
            val caps = cm.getNetworkCapabilities(network)
            caps != null &&
                caps.hasTransport(NetworkCapabilities.TRANSPORT_WIFI) &&
                caps.hasCapability(NetworkCapabilities.NET_CAPABILITY_NOT_VPN)
        }
    } catch (_: Exception) { null }

    /** Confirm before wiping pairing token (destructive, easy to tap accidentally). */
    private fun confirmResetPairing() {
        MaterialAlertDialogBuilder(this)
            .setTitle("Unpair this phone?")
            .setMessage("The desktop app will need to pair again (QR code or USB) before it can reconnect.")
            .setNegativeButton("Cancel", null)
            .setPositiveButton("Unpair") { _, _ -> resetPairing() }
            .show()
    }

    // Clears the stored pairing token and, if streaming, restarts the service so MjpegServer picks up the cleared state - every further request 401s until re-paired.
    private fun resetPairing() {
        TokenStore.clear(this)
        if (service?.isStreaming == true) {
            service?.stopStreaming()
            if (bound) { unbindService(serviceConnection); bound = false; service = null }
            startStream()
        }
        updateStatusText()
        Toast.makeText(this, "Pairing reset. Pair again from the desktop app to reconnect.", Toast.LENGTH_LONG).show()
    }

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

    // While streaming: spinners mirror service state (disabled, synced live); pre-stream: editable config
    private fun syncLiveControlsToState() {
        val svc = service
        if (svc == null || !svc.isStreaming) {
            if (!spinnerCamera.isEnabled) {
                spinnerCamera.isEnabled = true
                spinnerResolution.isEnabled = true
                // Re-derive OIS enablement; streaming just ended.
                cameras.getOrNull(spinnerCamera.selectedItemPosition)?.let {
                    checkOis.isEnabled = it.hasOis
                }
            }
            return
        }
        spinnerCamera.isEnabled = false
        spinnerResolution.isEnabled = false
        checkOis.isEnabled = false

        val snap = svc.getControlSnapshot() ?: return
        val camId = snap.currentCamera?.id ?: return
        val camIdx = cameras.indexOfFirst { it.id == camId }
        if (camIdx < 0) return

        if (spinnerCamera.selectedItemPosition != camIdx) {
            // Detach listener so setSelection() doesn't trigger callback out of order.
            spinnerCamera.onItemSelectedListener = null
            spinnerCamera.setSelection(camIdx)
            spinnerCamera.onItemSelectedListener = cameraSpinnerListener
            populateResolutionSpinner(camIdx)
        }
        val cam = cameras[camIdx]
        val resIdx = cam.supportedSizes.indexOfFirst {
            it.width == snap.streamWidth && it.height == snap.streamHeight
        }
        if (resIdx >= 0 && spinnerResolution.selectedItemPosition != resIdx) {
            spinnerResolution.setSelection(resIdx)
        }
        val liveOis = snap.ois && cam.hasOis
        if (checkOis.isChecked != liveOis) checkOis.isChecked = liveOis
    }

    private fun onToggleClicked() {
        if (isBusy()) return
        if (service?.isStreaming == true) {
            service?.stopStreaming()
            if (bound) { unbindService(serviceConnection); bound = false; service = null }
            updateStatusText()
        } else {
            starting = startStream()
            updateStatusText()
        }
    }

    /** True while a start is in flight (includes [starting] flag and intermediate states). */
    private fun isBusy(): Boolean {
        if (starting) return true
        val state = service?.state ?: StreamState.Idle
        return state != StreamState.Idle && state != StreamState.Streaming && state != StreamState.Failed
    }

    /** Returns true if a start was actually kicked off, false if bailed immediately. */
    private fun startStream(): Boolean {
        val camIdx = spinnerCamera.selectedItemPosition
        val resIdx = spinnerResolution.selectedItemPosition
        if (cameras.isEmpty() || camIdx < 0 || camIdx >= cameras.size) return false
        val cam  = cameras[camIdx]
        val size = cam.supportedSizes.getOrNull(resIdx) ?: cam.supportedSizes.first()

        val selection = StreamPrefs.Selection(
            cameraId = cam.id,
            logicalId = cam.logicalId ?: "",
            width = size.width,
            height = size.height,
            ois = checkOis.isChecked && cam.hasOis,
        )
        // Saved for desktop-initiated start (no spinners to read); persisted before launch.
        StreamPrefs.saveSelection(this, selection)

        if (StreamLauncher.start(this, selection) !is StreamLauncher.Result.Started) return false

        rebindToService()
        return true
    }

    // Desktop can start stream while screen is up; bindService(flags=0) doesn't connect retroactively
    private fun adoptRemoteStart() {
        val service = CameraStreamService.instance
        if (service == null) {
            // Nothing running; arm the announcement for the next remote start.
            remoteStartAnnounced = false
            return
        }
        if (!bound) rebindToService()
        if (!remoteStartAnnounced && service.startedRemotely && service.isStreaming) {
            remoteStartAnnounced = true
            Toast.makeText(this, "Streaming started from your desktop", Toast.LENGTH_SHORT).show()
        }
    }

    private fun rebindToService() {
        if (bound) { unbindService(serviceConnection); bound = false }
        bindService(Intent(this, CameraStreamService::class.java), serviceConnection, 0)
    }

    private fun updateStatusText() {
        val streaming = service?.isStreaming == true
        val busy = isBusy()
        btnToggle.isEnabled = !busy
        btnToggle.text = if (streaming) "Stop Streaming" else if (busy) "Starting..." else "Start Streaming"
        btnToggle.backgroundTintList = ColorStateList.valueOf(
            resources.getColor(if (streaming) R.color.colorError else R.color.colorPrimary, theme)
        )
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

    // All IPv4 addresses with Wi-Fi ones first (reachable through VPN for desktop streaming)
    private fun getAllDeviceIps(wifi: android.net.Network? = wifiNetwork()): List<String> {
        val all = try {
            java.net.NetworkInterface.getNetworkInterfaces()
                ?.asSequence()
                ?.filter { it.isUp && !it.isLoopback }
                ?.flatMap { it.inetAddresses.asSequence() }
                ?.filter { it is java.net.Inet4Address && !it.isLoopbackAddress }
                ?.mapNotNull { it.hostAddress }
                ?.toList() ?: emptyList()
        } catch (_: Exception) { emptyList() }
        return (wifiIps(wifi) + all).distinct()
    }

    private fun wifiIps(wifi: android.net.Network?): List<String> {
        if (wifi == null) return emptyList()
        return try {
            val cm = getSystemService(Context.CONNECTIVITY_SERVICE) as ConnectivityManager
            cm.getLinkProperties(wifi)?.linkAddresses.orEmpty()
                .map { it.address }
                .filter { it is java.net.Inet4Address && !it.isLoopbackAddress && !it.isLinkLocalAddress }
                .mapNotNull { it.hostAddress }
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
