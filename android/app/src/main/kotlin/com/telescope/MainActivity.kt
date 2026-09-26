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
    private lateinit var checkOis: CompoundButton
    private lateinit var checkLocalOnly: CompoundButton
    private lateinit var tvStatus: TextView
    private lateinit var tvCameraList: TextView
    private lateinit var layoutLinks: View
    private lateinit var tvLinkWifi: TextView
    private lateinit var tvLinkUsb: TextView
    private lateinit var btnScanPair: com.google.android.material.button.MaterialButton
    private lateinit var tvPairingTitle: TextView
    private lateinit var tvPairingHint: TextView
    private lateinit var layoutComputers: LinearLayout
    private lateinit var btnPreview: ImageButton
    private lateinit var cardPermissions: CardView
    private lateinit var layoutPermissionsContainer: LinearLayout
    private lateinit var btnCopyDiagnostics: MaterialButton
    private lateinit var cardUpdate: View
    private lateinit var tvUpdateTitle: TextView
    private lateinit var tvUpdateText: TextView
    private lateinit var btnUpdate: MaterialButton
    private lateinit var switchNightly: CompoundButton
    private lateinit var tvAppVersion: TextView
    private lateinit var btnCheckUpdates: com.google.android.material.button.MaterialButton
    // An update waiting for "Allow from this source" to be switched on in Settings.
    private var pendingUpdate: UpdateManifest? = null


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
        btnScanPair                = findViewById(R.id.btnScanPair)
        tvPairingTitle             = findViewById(R.id.tvPairingTitle)
        tvPairingHint              = findViewById(R.id.tvPairingHint)
        layoutComputers            = findViewById(R.id.layoutComputers)
        btnPreview                 = findViewById(R.id.btnPreview)
        cardPermissions            = findViewById(R.id.cardPermissions)
        layoutPermissionsContainer = findViewById(R.id.layoutPermissionsContainer)
        btnCopyDiagnostics         = findViewById(R.id.btnCopyDiagnostics)

        checkLocalOnly.isChecked = StreamPrefs.localOnly(this)
        checkLocalOnly.setOnCheckedChangeListener { _, checked ->
            StreamPrefs.setLocalOnly(this, checked)
            SessionEndpoint.refreshAnnouncement()
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
        btnScanPair.setOnClickListener {
            val opts = ScanOptions().apply {
                setPrompt("Point the camera at the pairing code on your computer")
                setBeepEnabled(false)
                setOrientationLocked(false)
                setBarcodeImageEnabled(false)
            }
            scanLauncher.launch(opts)
        }
        btnCopyDiagnostics.setOnClickListener { copyDiagnostics() }

        cardUpdate    = findViewById(R.id.cardUpdate)
        tvUpdateTitle = findViewById(R.id.tvUpdateTitle)
        tvUpdateText  = findViewById(R.id.tvUpdateText)
        btnUpdate     = findViewById(R.id.btnUpdate)
        switchNightly = findViewById(R.id.switchNightly)
        tvAppVersion  = findViewById(R.id.tvAppVersion)
        btnCheckUpdates = findViewById(R.id.btnCheckUpdates)
        btnUpdate.setOnClickListener { startUpdate() }
        btnCheckUpdates.setOnClickListener { Updater.check(this) }
        if (Updater.canUpdate()) {
            switchNightly.isChecked = Updater.channel(this) == UpdateLogic.NIGHTLY
            switchNightly.setOnCheckedChangeListener { _, checked ->
                Updater.setChannel(this, if (checked) UpdateLogic.NIGHTLY else UpdateLogic.STABLE)
            }
        } else {
            switchNightly.visibility = View.GONE
            findViewById<View>(R.id.tvNightlyHint).visibility = View.GONE
            btnCheckUpdates.visibility = View.GONE
        }

        spinnerCamera.onItemSelectedListener = cameraSpinnerListener

        checkPermissions()
    }

    override fun onResume() {
        super.onResume()
        checkPermissions()
        Updater.maybeCheck(this)
        val waiting = pendingUpdate
        if (waiting != null && canInstallUpdates()) {
            pendingUpdate = null
            Updater.downloadAndInstall(this, waiting)
        }
    }

    override fun onStart() {
        super.onStart()
        bindService(Intent(this, CameraStreamService::class.java), serviceConnection, 0)
        uiHandler.post(statusPoller)
        PairedComputers.addListener(pairingListener)
        Updater.addListener(updateListener)
        renderPairing()
        renderUpdate()
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
        PairedComputers.removeListener(pairingListener)
        Updater.removeListener(updateListener)
        if (bound) { unbindService(serviceConnection); bound = false }
        unregisterReceiver(pairReceiver)
        SessionEndpoint.release(SessionEndpoint.OWNER_ACTIVITY)
        super.onStop()
    }

    private fun handleQrScan(data: String) {
        when (val parsed = parsePairingOffer(data)) {
            is PairingParse.Invalid ->
                Toast.makeText(
                    this,
                    if (isDownloadLink(data))
                        "That code downloads the app. Scan the one under Add phone on the computer."
                    else "That's not a Telescope pairing code.",
                    Toast.LENGTH_LONG,
                ).show()
            is PairingParse.UnsupportedVersion ->
                Toast.makeText(
                    this,
                    "This code is from a different Telescope version. Update the app on your " +
                        "computer and this app to the same version, then try again.",
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
        val deviceName = PairedComputers.phoneName(this)
        val phoneId = PairedComputers.phoneId(this)

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
                    attemptPair(offer, route.candidate, network, deviceName, phoneId, myIps, timeout)
                if (problem == null) {
                    success = true
                    break
                }
                failures += PairingAttemptFailure(route.candidate.ip, route.via, problem)
            }
            if (success) {
                // One token per computer: pairing this one leaves every other paired computer working.
                // Servers read the list per request, so a running stream doesn't need restarting.
                PairedComputers.add(this, PairedComputer(
                    id = offer.computerId,
                    name = offer.computerName.ifBlank { "Computer" },
                    token = offer.token,
                    pairedAtMs = System.currentTimeMillis(),
                ))
            }
            runOnUiThread {
                if (success) {
                    Toast.makeText(
                        this, "Paired with ${offer.computerName.ifBlank { "your computer" }}.",
                        Toast.LENGTH_LONG,
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
        phoneId: String,
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
                put("phone_id", phoneId)
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

    private val pairingListener: () -> Unit = { runOnUiThread { renderPairing() } }
    private val updateListener: () -> Unit = { runOnUiThread { renderUpdate() } }

    // ── Updates ──────────────────────────────────────────────────────────

    private fun renderUpdate() {
        val state = Updater.state
        val suffix = when (state) {
            is Updater.State.UpToDate -> " · up to date"
            is Updater.State.Checking -> " · checking…"
            is Updater.State.CheckFailed -> " · couldn't check for updates"
            else -> ""
        }
        tvAppVersion.text = "Telescope ${BuildConfig.VERSION_NAME}$suffix"
        btnCheckUpdates.isEnabled = state !is Updater.State.Checking &&
            state !is Updater.State.Downloading && state !is Updater.State.Installing
        val streaming = service?.isStreaming == true || isBusy()
        val manifest = when (state) {
            is Updater.State.Available -> state.manifest
            is Updater.State.Downloading -> state.manifest
            is Updater.State.Installing -> state.manifest
            is Updater.State.Failed -> state.manifest
            else -> null
        }
        if (manifest == null) {
            cardUpdate.visibility = View.GONE
            return
        }
        cardUpdate.visibility = View.VISIBLE
        val version = "Telescope ${UpdateLogic.displayVersion(manifest)}"
        tvUpdateTitle.text = if (state is Updater.State.Failed) "Update didn't finish" else "Update available"
        when (state) {
            is Updater.State.Downloading -> {
                tvUpdateText.text = "$version · downloading ${state.percent}%"
                btnUpdate.text = "Updating…"
                btnUpdate.isEnabled = false
            }
            is Updater.State.Installing -> {
                tvUpdateText.text = "$version · installing"
                btnUpdate.text = "Updating…"
                btnUpdate.isEnabled = false
            }
            is Updater.State.Failed -> {
                tvUpdateText.text = state.message
                btnUpdate.text = "Try again"
                btnUpdate.isEnabled = !streaming
            }
            else -> {
                tvUpdateText.text = if (streaming) "$version. Stop streaming to update." else version
                btnUpdate.text = "Update"
                btnUpdate.isEnabled = !streaming
            }
        }
    }

    private fun canInstallUpdates(): Boolean = packageManager.canRequestPackageInstalls()

    private fun startUpdate() {
        val manifest = when (val state = Updater.state) {
            is Updater.State.Available -> state.manifest
            is Updater.State.Failed -> state.manifest
            else -> null
        } ?: return
        if (service?.isStreaming == true || isBusy()) return
        if (!canInstallUpdates()) {
            // Android asks once per app; after that, each update is a single confirm.
            pendingUpdate = manifest
            MaterialAlertDialogBuilder(this)
                .setTitle("Allow Telescope to install updates")
                .setMessage("In the next screen, turn on Allow from this source, then come back.")
                .setNegativeButton("Cancel") { _, _ -> pendingUpdate = null }
                .setPositiveButton("Open settings") { _, _ ->
                    startActivity(Intent(Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES,
                        Uri.parse("package:$packageName")))
                }
                .show()
            return
        }
        Updater.downloadAndInstall(this, manifest)
    }

    // The pairing card: how to pair when nothing is paired, otherwise the list of computers.
    private fun renderPairing() {
        val computers = PairedComputers.list(this).computers
        layoutComputers.removeAllViews()
        if (computers.isEmpty()) {
            tvPairingTitle.text = "Pair with your computer"
            tvPairingHint.visibility = View.VISIBLE
            btnScanPair.text = "Scan pairing code"
            styleScanButton(primary = true)
            return
        }
        tvPairingTitle.text = if (computers.size == 1) "Paired computer" else "Paired computers"
        tvPairingHint.visibility = View.GONE
        computers.sortedBy { it.pairedAtMs }.forEach { layoutComputers.addView(buildComputerRow(it)) }
        btnScanPair.text = "Pair another computer"
        styleScanButton(primary = false)
    }

    private fun styleScanButton(primary: Boolean) {
        val fill = if (primary) R.color.colorPrimary else android.R.color.transparent
        val text = if (primary) R.color.colorOnPrimary else R.color.colorOnSurface
        btnScanPair.backgroundTintList = ColorStateList.valueOf(resources.getColor(fill, theme))
        btnScanPair.setTextColor(resources.getColor(text, theme))
        btnScanPair.iconTint = ColorStateList.valueOf(resources.getColor(text, theme))
        btnScanPair.strokeColor = ColorStateList.valueOf(resources.getColor(R.color.colorOutline, theme))
        btnScanPair.strokeWidth = if (primary) 0 else dp(1)
    }

    private fun buildComputerRow(computer: PairedComputer): View {
        val row = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = android.view.Gravity.CENTER_VERTICAL
            setPadding(0, 0, 0, dp(12))
        }
        val textBlock = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            layoutParams = LinearLayout.LayoutParams(0, LinearLayout.LayoutParams.WRAP_CONTENT, 1f)
        }
        textBlock.addView(TextView(this).apply {
            text = computer.name
            setTextAppearance(R.style.TextAppearance_Telescope_Body)
        })
        textBlock.addView(TextView(this).apply {
            val date = java.text.DateFormat.getDateInstance(java.text.DateFormat.MEDIUM)
                .format(java.util.Date(computer.pairedAtMs))
            text = "Paired $date"
            setTextAppearance(R.style.TextAppearance_Telescope_Hint)
        })
        row.addView(textBlock)
        row.addView(com.google.android.material.button.MaterialButton(
            this, null, com.google.android.material.R.attr.materialButtonOutlinedStyle,
        ).apply {
            text = "Remove"
            setOnClickListener { confirmRemoveComputer(computer) }
        })
        return row
    }

    // Removing a computer revokes only its token; the others keep working.
    private fun confirmRemoveComputer(computer: PairedComputer) {
        MaterialAlertDialogBuilder(this)
            .setTitle("Remove ${computer.name}?")
            .setMessage("It won't be able to use this phone's camera until you pair it again.")
            .setNegativeButton("Cancel", null)
            .setPositiveButton("Remove") { _, _ -> PairedComputers.remove(this, computer.id) }
            .show()
    }

    // One step of the setup card. permission == null is the battery exemption, which has no prompt of
    // its own, only a system screen.
    private data class SetupStep(
        val permission: String?,
        val label: String,
        val reason: String,
        val done: Boolean,
    )

    private fun cameraGranted(): Boolean =
        ContextCompat.checkSelfPermission(this, Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED

    private fun setupSteps(): List<SetupStep> {
        val steps = mutableListOf(
            SetupStep(Manifest.permission.CAMERA, "Camera", "To stream from this phone's cameras.", cameraGranted()),
        )
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            steps += SetupStep(Manifest.permission.POST_NOTIFICATIONS, "Notifications",
                "Shows when the camera is streaming, and stops it from there.",
                ContextCompat.checkSelfPermission(this, Manifest.permission.POST_NOTIFICATIONS) ==
                    PackageManager.PERMISSION_GRANTED)
        }
        if (getSharedPreferences(PREFS_SETUP, MODE_PRIVATE).getBoolean(CameraStreamService.KEY_MIC_WANTED, false)) {
            steps += SetupStep(Manifest.permission.RECORD_AUDIO, "Microphone",
                "For the Telescope microphone on your computer.",
                AudioStreamer.permitted(this))
        }
        val pm = getSystemService(POWER_SERVICE) as PowerManager
        steps += SetupStep(null, "Battery", "Keeps Android from stopping the stream when the screen is off.",
            pm.isIgnoringBatteryOptimizations(packageName))
        return steps
    }

    // Nothing is asked for on its own: each step waits for its button, in order, with its reason next to it.
    private fun checkPermissions() {
        val steps = setupSteps()
        val current = SetupSteps.current(steps.map { it.done })
        layoutPermissionsContainer.removeAllViews()
        if (current < 0) {
            cardPermissions.visibility = View.GONE
        } else {
            cardPermissions.visibility = View.VISIBLE
            steps.forEachIndexed { i, step ->
                layoutPermissionsContainer.addView(buildStepRow(step, primary = i == current, last = i == steps.lastIndex))
            }
        }

        val camera = cameraGranted()
        if (camera && cameras.isEmpty()) loadCameras()
        if (!camera) {
            spinnerCamera.isEnabled = false
            spinnerResolution.isEnabled = false
        }
        updateStatusText()
    }

    private fun askedBefore(permission: String): Boolean =
        getSharedPreferences(PREFS_SETUP, MODE_PRIVATE).getBoolean(permission, false)

    private fun ask(permission: String) {
        getSharedPreferences(PREFS_SETUP, MODE_PRIVATE).edit().putBoolean(permission, true).apply()
        ActivityCompat.requestPermissions(this, arrayOf(permission), RC_PERMS)
    }

    private fun buildStepRow(step: SetupStep, primary: Boolean, last: Boolean): View {
        val row = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = android.view.Gravity.CENTER_VERTICAL
            setPadding(0, 0, 0, if (last) 0 else dp(14))
        }

        val textBlock = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            layoutParams = LinearLayout.LayoutParams(0, LinearLayout.LayoutParams.WRAP_CONTENT, 1f)
        }
        TextView(this).apply {
            text = step.label
            setTextAppearance(R.style.TextAppearance_Telescope_Body)
            setTypeface(null, android.graphics.Typeface.BOLD)
            if (step.done) setTextColor(resources.getColor(R.color.colorOnSurfaceDim, theme))
            textBlock.addView(this)
        }
        if (!step.done) {
            TextView(this).apply {
                text = step.reason
                setTextAppearance(R.style.TextAppearance_Telescope_Hint)
                textBlock.addView(this)
            }
        }
        row.addView(textBlock)

        if (step.done) {
            row.addView(android.widget.ImageView(this).apply {
                setImageResource(R.drawable.ic_check)
                imageTintList = ColorStateList.valueOf(resources.getColor(R.color.colorStreamingText, theme))
                contentDescription = "Allowed"
                layoutParams = LinearLayout.LayoutParams(dp(22), dp(22)).apply { marginStart = dp(12) }
            })
            return row
        }

        val perm = step.permission
        val action = if (perm == null) SetupSteps.Action.ASK else SetupSteps.action(
            granted = false,
            askedBefore = askedBefore(perm),
            showRationale = ActivityCompat.shouldShowRequestPermissionRationale(this, perm),
        )
        val btn = if (primary) {
            MaterialButton(this)
        } else {
            MaterialButton(this, null, com.google.android.material.R.attr.materialButtonOutlinedStyle)
        }
        btn.apply {
            text = if (action == SetupSteps.Action.SETTINGS) "Open settings" else "Allow"
            setOnClickListener {
                when {
                    perm == null -> startActivity(Intent(Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS)
                        .apply { data = Uri.parse("package:$packageName") })
                    action == SetupSteps.Action.SETTINGS -> openAppSettings()
                    else -> ask(perm)
                }
            }
            layoutParams = LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.WRAP_CONTENT,
                LinearLayout.LayoutParams.WRAP_CONTENT
            ).apply { marginStart = dp(12) }
        }
        row.addView(btn)
        return row
    }

    private fun dp(value: Int): Int = (value * resources.displayMetrics.density).toInt()

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
            if (!spinnerCamera.isEnabled && cameraGranted()) {
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
            Toast.makeText(this, "Streaming started from your computer", Toast.LENGTH_SHORT).show()
        }
    }

    private fun rebindToService() {
        if (bound) { unbindService(serviceConnection); bound = false }
        bindService(Intent(this, CameraStreamService::class.java), serviceConnection, 0)
    }

    private fun updateStatusText() {
        val streaming = service?.isStreaming == true
        val busy = isBusy()
        // Without camera access there's nothing to stream; the setup card above says so.
        btnToggle.isEnabled = !busy && (streaming || cameraGranted())
        btnToggle.text = if (streaming) "Stop Streaming" else if (busy) "Starting..." else "Start Streaming"
        btnToggle.backgroundTintList = ColorStateList.valueOf(
            resources.getColor(if (streaming) R.color.colorStop else R.color.colorPrimary, theme)
        )
        if (streaming) {
            val ip   = getDeviceIp()
            val port = service?.port ?: CameraStreamService.DEFAULT_PORT
            // The camera stays on while the computer reconnects; say so rather than claim it's getting video.
            val viewed = service?.hasViewer == true
            tvStatus.text = if (viewed) "● Streaming" else "● Waiting for the computer"
            tvStatus.setTextColor(resources.getColor(
                if (viewed) R.color.colorStreamingText else R.color.colorWarn, theme))
            // MjpegServer only answers /v1/video; the old /video links 404'd.
            tvLinkWifi.text = "Wi-Fi  http://$ip:$port/v1/video"
            tvLinkUsb.text  = "USB  http://localhost:$port/v1/video"
            tvLinkWifi.visibility = if (checkLocalOnly.isChecked) View.GONE else View.VISIBLE
            layoutLinks.visibility = View.VISIBLE
        } else {
            tvStatus.text = "○ Not streaming"
            tvStatus.setTextColor(resources.getColor(R.color.colorOnSurfaceDim, theme))
            tvLinkWifi.visibility = View.VISIBLE
            layoutLinks.visibility = View.GONE
        }
        if (::cardUpdate.isInitialized && cardUpdate.visibility == View.VISIBLE) renderUpdate()
    }

    private fun copyLink(pill: TextView) {
        val url = pill.text.toString().let { it.substring(it.indexOf("http")) }
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
        private const val PREFS_SETUP = "setup"  // permission -> asked once already
        // Lets the desktop app push a pairing payload straight over adb when
        // there's no camera-scannable QR code involved (USB pairing) - the
        // same JSON shape and handleQrScan() logic as the QR flow, just
        // delivered a different way.
        const val ACTION_PAIR = "com.telescope.action.PAIR"
        const val EXTRA_PAIR_PAYLOAD = "payload"
    }
}
