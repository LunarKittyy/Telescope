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
import android.widget.ImageView
import android.view.WindowManager
import android.widget.*
import androidx.cardview.widget.CardView
import androidx.core.content.res.ResourcesCompat
import com.google.android.material.button.MaterialButton
import androidx.core.app.ActivityCompat
import androidx.core.content.ContextCompat
import com.journeyapps.barcodescanner.ScanContract
import com.journeyapps.barcodescanner.ScanOptions

class MainActivity : LookActivity() {

    private lateinit var btnToggle: MaterialButton
    private lateinit var checkLocalOnly: CompoundButton
    private lateinit var checkWaitForComputer: CompoundButton
    private lateinit var tvStatus: TextView
    private lateinit var tvPairingLine: TextView
    private lateinit var btnPairFirst: LookButton
    private lateinit var rowPairedButtons: View
    private lateinit var btnPreview: com.google.android.material.button.MaterialButton
    private lateinit var btnBlackScreen: MaterialButton
    private lateinit var blackScreen: BlackScreen
    private lateinit var cardPermissions: CardView
    private lateinit var layoutPermissionsContainer: LinearLayout
    private lateinit var cardUpdate: View
    private lateinit var tvUpdateTitle: TextView
    private lateinit var tvUpdateText: TextView
    private lateinit var btnUpdate: MaterialButton
    // An update waiting for "Allow from this source" to be switched on in Settings.
    private var pendingUpdate: UpdateManifest? = null


    private var service: CameraStreamService? = null
    private var bound = false
    private var bindRequested = false
    // Prevents double-start race; cleared once service connects.
    private var starting = false

    private val uiHandler = Handler(Looper.getMainLooper())
    private val statusPoller = object : Runnable {
        override fun run() {
            // The restarted service died before it connected: nothing is starting any more.
            if (starting && CameraStreamService.instance == null && !SessionStartWindow.open()) starting = false
            adoptRemoteStart()
            updateStatusText()
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

    // Manage's "Pair new" closes it and asks for the scanner from here, where pairing lives
    private val manageLauncher = registerForActivityResult(androidx.activity.result.contract.ActivityResultContracts.StartActivityForResult()) { result ->
        if (result.resultCode == ManageActivity.RESULT_PAIR_NEW) launchScan()
    }

    private fun launchScan() {
        val opts = ScanOptions().apply {
            setPrompt(getString(R.string.scan_prompt))
            setBeepEnabled(false)
            setOrientationLocked(false)
            setBarcodeImageEnabled(false)
            setCaptureActivity(ScanActivity::class.java)
        }
        scanLauncher.launch(opts)
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

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        RecentRuns.recordCrashes(this)
        setContentView(R.layout.activity_main)
        SystemBars.pad(findViewById(R.id.root), top = findViewById(R.id.header))
        // The screen stays on while the app is open (the Wi-Fi locks only work with it on); the power button still turns it off
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)

        btnToggle         = findViewById<MaterialButton>(R.id.btnToggle)
        tvStatus          = findViewById(R.id.tvStatus)
        blackScreen = BlackScreen(this) { tvStatus.text }
        checkLocalOnly             = findViewById(R.id.checkLocalOnly)
        checkWaitForComputer       = findViewById(R.id.checkWaitForComputer)
        tvPairingLine              = findViewById(R.id.tvPairingLine)
        btnPairFirst               = findViewById(R.id.btnPairFirst)
        rowPairedButtons           = findViewById(R.id.rowPairedButtons)
        btnPairFirst.setOnClickListener { launchScan() }
        findViewById<View>(R.id.btnPairNew).setOnClickListener { launchScan() }
        findViewById<View>(R.id.btnManage).setOnClickListener { manageLauncher.launch(Intent(this, ManageActivity::class.java)) }
        btnPreview                 = findViewById(R.id.btnPreview)
        btnBlackScreen             = findViewById(R.id.btnBlackScreen)
        cardPermissions            = findViewById(R.id.cardPermissions)
        layoutPermissionsContainer = findViewById(R.id.layoutPermissionsContainer)

        checkLocalOnly.isChecked = StreamPrefs.localOnly(this)
        checkLocalOnly.setOnCheckedChangeListener { _, checked ->
            StreamPrefs.setLocalOnly(this, checked)
            SessionEndpoint.refreshAnnouncement()
            checkPermissions()
            val svc = service
            // Any live session, not just a settled stream: a lens or size change in flight is still bound to the old address
            if (svc != null && svc.state.holdsSession) {
                val cameraOff = svc.cameraOff
                val owner = svc.owner
                svc.stopStreaming()
                unbind(); service = null
                starting = restartStream(cameraOff, owner)
                updateStatusText()
            }
        }


        checkWaitForComputer.setOnCheckedChangeListener(waitListener)

        btnToggle.setOnClickListener { onToggleClicked() }
        btnPreview.setOnClickListener { startActivity(Intent(this, PreviewActivity::class.java)) }
        findViewById<View>(R.id.btnSettings).setOnClickListener { startActivity(Intent(this, SettingsActivity::class.java)) }
        btnBlackScreen.setOnClickListener { blackScreen.enter() }
        // Icon only, so a long press says what it does
        androidx.appcompat.widget.TooltipCompat.setTooltipText(btnBlackScreen, getString(R.string.black_screen_description))
        cardUpdate    = findViewById(R.id.cardUpdate)
        tvUpdateTitle = findViewById<CardHeader>(R.id.headerUpdate).title
        tvUpdateText  = findViewById(R.id.tvUpdateText)
        btnUpdate     = findViewById(R.id.btnUpdate)
        btnUpdate.setOnClickListener { startUpdate() }
        checkPermissions()
    }

    override fun onResume() {
        super.onResume()
        checkPermissions()
        Updater.maybeCheck(this)
        val waiting = pendingUpdate
        if (shouldResumePendingUpdate(waiting != null, canInstallUpdates(), service?.isStreaming == true || isBusy())) {
            pendingUpdate = null
            Updater.downloadAndInstall(this, waiting!!)
        }
    }

    override fun onStart() {
        super.onStart()
        bindToService()
        uiHandler.post(statusPoller)
        PairedComputers.addListener(pairingListener)
        Updater.addListener(updateListener)
        setupPrefs().registerOnSharedPreferenceChangeListener(setupPrefsListener)
        renderPairing()
        renderUpdate()
        // Reachable while screen is up; service holds reference after screen goes dark
        SessionEndpoint.screenStarted(this)
        syncWaitForComputer()
        // Notes the computer sent while the app wasn't on screen, and that weren't closed since
        if (NotesBoard.wantsScreen && CameraStreamService.instance?.isStreaming == true) startActivity(NotesActivity.intent(this))
        // RECEIVER_EXPORTED is required for adb, but gated on DUMP permission (shell-only)
        ContextCompat.registerReceiver(
            this, pairReceiver, IntentFilter(ACTION_PAIR),
            Manifest.permission.DUMP, null, ContextCompat.RECEIVER_EXPORTED,
        )
    }

    // The notification's "Stop waiting" may have switched it off; if it's on, (re)start the service, which Android
    // may have stopped. Only here, with the activity visible, so waiting never starts from the background.
    private fun syncWaitForComputer() {
        val on = StreamPrefs.waitForComputer(this)
        if (checkWaitForComputer.isChecked != on) {
            checkWaitForComputer.setOnCheckedChangeListener(null)  // only showing the saved value
            checkWaitForComputer.isChecked = on
            checkWaitForComputer.setOnCheckedChangeListener(waitListener)
        }
        if (on) WaitingService.start(this)
    }

    private val waitListener = CompoundButton.OnCheckedChangeListener { _, checked ->
        StreamPrefs.setWaitForComputer(this, checked)
        if (checked) WaitingService.start(this) else WaitingService.stop(this)
    }

    // A computer asking for the mic sets mic_wanted from the service: the setup card shows its row right away.
    // A field, since SharedPreferences only holds its listeners weakly.
    private val setupPrefsListener = android.content.SharedPreferences.OnSharedPreferenceChangeListener { _, key ->
        if (key == CameraStreamService.KEY_MIC_WANTED) checkPermissions()
    }

    private fun setupPrefs() = getSharedPreferences(CameraStreamService.PREFS_SETUP, MODE_PRIVATE)

    override fun dispatchTouchEvent(ev: android.view.MotionEvent): Boolean =
        blackScreen.swallow(ev) || super.dispatchTouchEvent(ev)

    override fun onStop() {
        // Coming back (or the power button turning the screen on again) shows the app, not a black screen nobody remembers
        blackScreen.leave()
        setupPrefs().unregisterOnSharedPreferenceChangeListener(setupPrefsListener)
        uiHandler.removeCallbacks(statusPoller)
        PairedComputers.removeListener(pairingListener)
        Updater.removeListener(updateListener)
        unbind()
        unregisterReceiver(pairReceiver)
        SessionEndpoint.screenStopped()
        super.onStop()
    }

    private fun handleQrScan(data: String) {
        when (val parsed = parsePairingOffer(data)) {
            is PairingParse.Invalid ->
                Toast.makeText(
                    this,
                    if (isDownloadLink(data)) R.string.scan_download_link else R.string.scan_not_pairing_code,
                    Toast.LENGTH_LONG,
                ).show()
            is PairingParse.UnsupportedVersion ->
                Toast.makeText(
                    this,
                    R.string.scan_other_version,
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
                    name = offer.computerName.ifBlank { getString(R.string.default_computer_name) },
                    token = offer.token,
                    pairedAtMs = System.currentTimeMillis(),
                ))
            }
            runOnUiThread {
                if (success) {
                    Toast.makeText(
                        this,
                        if (offer.computerName.isBlank()) getString(R.string.paired_with_your_computer)
                        else getString(R.string.paired_with, offer.computerName),
                        Toast.LENGTH_LONG,
                    ).show()
                } else {
                    // Show in dialog for readability (too long for toast).
                    if (!isFinishing && !isDestroyed) {
                        showPairingFailure(pairingFailureMessage(failures, strings(), untried))
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
            val certSha256 = PhoneTls.identity(this).fingerprint
            val body = org.json.JSONObject().apply {
                put("name", deviceName)
                put("phone_id", phoneId)
                put("ips", org.json.JSONArray(myIps))
                put("cert_sha256", certSha256)
                put("proof", pairingProof(offer.token, offer.nonce, phoneId, certSha256))
            }.toString()
            conn.outputStream.use { it.write(body.toByteArray()) }
            if (conn.responseCode == 200) null else "HTTP ${conn.responseCode}"
        } catch (e: Exception) {
            describeNetworkError(e, strings())
        } finally {
            try { conn?.disconnect() } catch (_: Exception) {}
        }
    }

    private fun showPairingFailure(message: String) {
        lookDialog()
            .setTitle(R.string.pairing_failed)
            .setMessage(message)
            .setPositiveButton(R.string.ok, null)
            .showLook()
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
        tvUpdateTitle.setText(if (state is Updater.State.Failed) R.string.update_failed_title else R.string.update_available)
        when (state) {
            is Updater.State.Downloading -> {
                tvUpdateText.text = getString(R.string.update_downloading, version, state.percent)
                btnUpdate.setText(R.string.updating)
                btnUpdate.isEnabled = false
            }
            is Updater.State.Installing -> {
                tvUpdateText.text = getString(R.string.update_installing, version)
                btnUpdate.setText(R.string.updating)
                btnUpdate.isEnabled = false
            }
            is Updater.State.Failed -> {
                tvUpdateText.text = state.message
                btnUpdate.setText(R.string.try_again)
                btnUpdate.isEnabled = !streaming
            }
            else -> {
                tvUpdateText.text = if (streaming) getString(R.string.update_stop_streaming_first, version) else version
                btnUpdate.setText(R.string.update)
                btnUpdate.isEnabled = !streaming
            }
        }
    }

    private fun startUpdate() {
        val manifest = when (val state = Updater.state) {
            is Updater.State.Available -> state.manifest
            is Updater.State.Failed -> state.manifest
            else -> null
        } ?: return
        if (service?.isStreaming == true || isBusy()) return
        installUpdate(manifest) { pendingUpdate = it }
    }

    // The pairing card: what pairing is and one button while nothing is paired, otherwise how many are and two buttons
    private fun renderPairing() {
        val count = PairedComputers.list(this).computers.size
        btnPairFirst.visibility = if (count == 0) View.VISIBLE else View.GONE
        rowPairedButtons.visibility = if (count == 0) View.GONE else View.VISIBLE
        tvPairingLine.text = if (count == 0) getString(R.string.pair_intro) else resources.getQuantityString(R.plurals.computers_paired, count, count)
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
            SetupStep(Manifest.permission.CAMERA, getString(R.string.setup_camera),
                getString(R.string.setup_camera_reason), cameraGranted()),
        )
        // Android 17 blocks Wi-Fi traffic without it; USB goes through localhost and works either way
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.CINNAMON_BUN && !StreamPrefs.localOnly(this)) {
            steps += SetupStep(Manifest.permission.ACCESS_LOCAL_NETWORK, getString(R.string.setup_local_network),
                getString(R.string.setup_local_network_reason),
                ContextCompat.checkSelfPermission(this, Manifest.permission.ACCESS_LOCAL_NETWORK) ==
                    PackageManager.PERMISSION_GRANTED)
        }
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            steps += SetupStep(Manifest.permission.POST_NOTIFICATIONS, getString(R.string.setup_notifications),
                getString(R.string.setup_notifications_reason),
                ContextCompat.checkSelfPermission(this, Manifest.permission.POST_NOTIFICATIONS) ==
                    PackageManager.PERMISSION_GRANTED)
        }
        if (getSharedPreferences(CameraStreamService.PREFS_SETUP, MODE_PRIVATE).getBoolean(CameraStreamService.KEY_MIC_WANTED, false)) {
            steps += SetupStep(Manifest.permission.RECORD_AUDIO, getString(R.string.setup_microphone),
                getString(R.string.setup_microphone_reason),
                AudioStreamer.permitted(this))
        }
        val pm = getSystemService(POWER_SERVICE) as PowerManager
        steps += SetupStep(null, getString(R.string.setup_battery), getString(R.string.setup_battery_reason),
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

        updateStatusText()
    }

    private fun askedBefore(permission: String): Boolean =
        getSharedPreferences(CameraStreamService.PREFS_SETUP, MODE_PRIVATE).getBoolean(permission, false)

    private fun ask(permission: String) {
        getSharedPreferences(CameraStreamService.PREFS_SETUP, MODE_PRIVATE).edit().putBoolean(permission, true).apply()
        ActivityCompat.requestPermissions(this, arrayOf(permission), RC_PERMS)
    }

    private fun buildStepRow(step: SetupStep, primary: Boolean, last: Boolean): View {
        // At a large font size the button goes under its text, which keeps the whole width instead of breaking mid-word
        val stacked = !step.done && resources.configuration.fontScale >= STACK_FONT_SCALE
        val row = LinearLayout(this).apply {
            orientation = if (stacked) LinearLayout.VERTICAL else LinearLayout.HORIZONTAL
            gravity = if (stacked) android.view.Gravity.START else android.view.Gravity.CENTER_VERTICAL
            setPadding(0, 0, 0, if (last) 0 else dp(14))
        }

        val textBlock = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            layoutParams = if (stacked) LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.MATCH_PARENT, LinearLayout.LayoutParams.WRAP_CONTENT)
            else LinearLayout.LayoutParams(0, LinearLayout.LayoutParams.WRAP_CONTENT, 1f)
        }
        TextView(this).apply {
            text = step.label
            setTextAppearance(R.style.TextAppearance_Telescope_Body)
            typeface = ResourcesCompat.getFont(this@MainActivity, R.font.ibm_plex_sans_semibold)
            if (step.done) setTextColor(attrColor(R.attr.tsInk2))
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
            row.addView(ImageView(this).apply {
                setImageResource(R.drawable.ic_check)
                imageTintList = attrColors(R.attr.tsOk)
                contentDescription = getString(R.string.setup_allowed)
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
        val btn = LookButton(this)
        btn.apply {
            setRole(if (primary) LookButton.Role.PRIMARY else LookButton.Role.PLAIN, attrColor(R.attr.tsAccent))
            setText(if (action == SetupSteps.Action.SETTINGS) R.string.open_settings else R.string.allow)
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
            ).apply { if (stacked) topMargin = dp(6) else marginStart = dp(12) }
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

    // The computer starts streams; the phone can only stop one, or cancel one that's starting.
    private fun onToggleClicked() {
        val svc = service ?: return
        if (!svc.isStreaming && !isBusy()) return
        starting = false
        svc.stopStreaming(if (svc.isStreaming) "stopButton" else "cancelButton")
        unbind(); service = null
        updateStatusText()
    }

    /** True while a start is in flight (includes [starting] flag and intermediate states). */
    private fun isBusy(): Boolean {
        if (starting) return true
        val state = service?.state ?: StreamState.Idle
        return state != StreamState.Idle && state != StreamState.Streaming && state != StreamState.Failed
    }

    // Reopens the stream after Local only changes, with what the computer last picked and whether the camera was off.
    private fun restartStream(cameraOff: Boolean, owner: String?): Boolean {
        if (StreamLauncher.start(this, StreamPrefs.lastSelection(this), cameraOff = cameraOff, owner = owner)
            !is StreamLauncher.Result.Started) return false
        rebindToService()
        return true
    }

    // Desktop can start stream while screen is up; bindService(flags=0) doesn't connect retroactively
    private fun adoptRemoteStart() {
        val service = CameraStreamService.instance
        if (service == null) return
        if (!bound) rebindToService()
        // Toast "started from your desktop" only once per remote start
        if (service.remoteStartNotice.take(service.isStreaming)) {
            Toast.makeText(this, R.string.streaming_started_remotely, Toast.LENGTH_SHORT).show()
        }
    }

    // Registered even when nothing is running to connect to, so unbind by what was asked, not by `bound`
    private fun bindToService() {
        bindRequested = true
        bindService(Intent(this, CameraStreamService::class.java), serviceConnection, 0)
    }

    private fun unbind() {
        if (!bindRequested) return
        bindRequested = false
        bound = false
        try { unbindService(serviceConnection) } catch (_: IllegalArgumentException) {}
    }

    private fun rebindToService() {
        unbind()
        bindToService()
    }

    private fun updateStatusText() {
        val streaming = service?.isStreaming == true
        val busy = isBusy()
        btnToggle.visibility = if (streaming || busy) View.VISIBLE else View.GONE
        btnToggle.isEnabled = streaming || (busy && service != null)
        btnToggle.setText(if (busy) R.string.cancel else R.string.stop_streaming)
        btnBlackScreen.visibility = if (streaming) View.VISIBLE else View.GONE
        blackScreen.onStreamState(streaming, busy)
        if (streaming) {
            // The camera stays on while the computer reconnects; say so rather than claim it's getting video.
            val viewed = service?.hasViewer == true
            val taken = service?.cameraTaken == true
            tvStatus.setText(when {
                taken -> R.string.status_camera_taken
                !viewed -> R.string.status_waiting_for_computer
                service?.cameraOff == true -> R.string.status_mic_only
                else -> R.string.status_streaming
            })
            val tone = if (viewed && !taken) R.attr.tsOk else R.attr.tsWarn
            tvStatus.setTextColor(attrColor(tone))
            showStatusDetail(if (viewed && !taken) statusDetail() else emptyList(), tone)
        } else {
            tvStatus.setText(if (busy) R.string.status_starting else R.string.status_not_streaming)
            tvStatus.setTextColor(attrColor(R.attr.tsInk2))
            showStatusDetail(emptyList(), R.attr.tsInk2)
        }
        if (::cardUpdate.isInitialized && cardUpdate.visibility == View.VISIBLE) renderUpdate()
    }

    // "To newmoon over USB" and "At 1080p 30 fps", each its own tag; the size and rate only with the camera on
    private fun statusDetail(): List<String> {
        val svc = service ?: return emptyList()
        val quality = if (svc.cameraOff) null else svc.liveQuality()?.let { (p, fps) -> getString(R.string.status_quality, p, fps) }
        return listOfNotNull(CameraStreamService.routeText(this), quality)
    }

    private fun showStatusDetail(lines: List<String>, tone: Int) {
        val tags = findViewById<LinearLayout>(R.id.layoutStatusTags)
        if (tags.tag == lines.toString() + tone) return
        tags.tag = lines.toString() + tone
        tags.removeAllViews()
        lines.forEach { line ->
            tags.addView(lookTag(line, attrColor(tone)), LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.WRAP_CONTENT, LinearLayout.LayoutParams.WRAP_CONTENT).apply { topMargin = dp(if (tags.childCount == 0) 0 else 6) })
        }
        tags.visibility = if (lines.isEmpty()) View.GONE else View.VISIBLE
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


    companion object {
        private const val RC_PERMS = 100
        // Android's "Large" font is 1.15 and its largest 1.3 or more; from there a setup step stacks its button
        private const val STACK_FONT_SCALE = 1.3f
        // Lets the desktop app push a pairing payload straight over adb when
        // there's no camera-scannable QR code involved (USB pairing) - the
        // same JSON shape and handleQrScan() logic as the QR flow, just
        // delivered a different way.
        const val ACTION_PAIR = "com.telescope.action.PAIR"
        const val EXTRA_PAIR_PAYLOAD = "payload"
    }
}
