package com.telescope

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.hardware.camera2.CameraManager
import android.net.Uri
import android.os.Bundle
import android.text.method.LinkMovementMethod
import android.view.View
import android.widget.CompoundButton
import android.widget.TextView
import android.widget.Toast
import androidx.core.text.HtmlCompat
import com.google.android.material.button.MaterialButton
import com.google.android.material.button.MaterialButtonToggleGroup

// The phone's Settings, like the computer's: Look (the theme, applied the moment it is picked), Updates and About
class SettingsActivity : LookActivity() {

    private lateinit var tvAppVersion: TextView
    private lateinit var btnCheckUpdates: MaterialButton

    private val updateListener: () -> Unit = { runOnUiThread { renderVersion() } }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_settings)
        SystemBars.pad(findViewById(R.id.root), top = findViewById(R.id.header))
        findViewById<View>(R.id.btnBack).setOnClickListener { finish() }

        buildLookChoices(findViewById(R.id.lookGroup))

        tvAppVersion = findViewById(R.id.tvAppVersion)
        btnCheckUpdates = findViewById(R.id.btnCheckUpdates)
        val nightly = findViewById<CompoundButton>(R.id.switchNightly)
        if (Updater.canUpdate()) {
            nightly.isChecked = Updater.channel(this) == UpdateLogic.NIGHTLY
            nightly.setOnCheckedChangeListener { _, checked ->
                Updater.setChannel(this, if (checked) UpdateLogic.NIGHTLY else UpdateLogic.STABLE)
            }
            btnCheckUpdates.setOnClickListener { Updater.check(this) }
        } else {
            findViewById<View>(R.id.cardUpdates).visibility = View.GONE
        }

        findViewById<View>(R.id.btnCopyDiagnostics).setOnClickListener { copyDiagnostics() }
        findViewById<View>(R.id.btnReportBug).setOnClickListener { reportBug() }
        findViewById<View>(R.id.btnHelp).setOnClickListener { openUrl(HELP_URL) }
        findViewById<TextView>(R.id.tvMadeBy).apply {
            text = HtmlCompat.fromHtml(getString(R.string.made_by, AUTHOR_URL), HtmlCompat.FROM_HTML_MODE_LEGACY)
            movementMethod = LinkMovementMethod.getInstance()
        }
        if (resources.getBoolean(R.bool.translation_is_draft)) findViewById<View>(R.id.tvTranslationDraft).visibility = View.VISIBLE
    }

    override fun onStart() {
        super.onStart()
        Updater.addListener(updateListener)
        renderVersion()
    }

    override fun onStop() {
        Updater.removeListener(updateListener)
        super.onStop()
    }

    // One button per look, the picked one drawn like the desktop's picked segment: its colour round it and a wash of it inside
    private fun buildLookChoices(group: MaterialButtonToggleGroup) {
        val hue = attrColor(R.attr.tsHueStreaming)
        val fill = android.content.res.ColorStateList(
            arrayOf(intArrayOf(android.R.attr.state_checked), intArrayOf()),
            intArrayOf(tint(hue), attrColor(R.attr.tsBtnFill)),
        )
        val edge = android.content.res.ColorStateList(
            arrayOf(intArrayOf(android.R.attr.state_checked), intArrayOf()),
            intArrayOf(hue, attrColor(R.attr.tsBtnEdge)),
        )
        val current = Look.current(this)
        Look.entries.forEach { look ->
            val button = MaterialButton(this, null, com.google.android.material.R.attr.materialButtonOutlinedStyle).apply {
                id = View.generateViewId()
                setText(look.label)
                tag = look
                isAllCaps = false
                minWidth = 0
                minimumWidth = 0
                insetTop = 0
                insetBottom = 0
                minHeight = dp(44)
                setPadding(dp(4), 0, dp(4), 0)
                backgroundTintList = fill
                strokeColor = edge
                strokeWidth = dp(1)
                setTextColor(attrColor(R.attr.tsInk))
                rippleColor = attrColors(R.attr.tsLineStrong)
            }
            group.addView(button, android.widget.LinearLayout.LayoutParams(0, android.widget.LinearLayout.LayoutParams.WRAP_CONTENT, 1f))
            if (look == current) group.check(button.id)
        }
        group.addOnButtonCheckedListener { g, id, checked ->
            if (!checked) return@addOnButtonCheckedListener
            val look = g.findViewById<View>(id).tag as Look
            if (look == Look.current(this)) return@addOnButtonCheckedListener
            Look.save(this, look)
            // The new theme reaches every colour at once, and the screens behind this one rebuild when they come back
            recreate()
        }
    }

    private fun renderVersion() {
        val line = when (Updater.state) {
            is Updater.State.UpToDate -> R.string.version_up_to_date
            is Updater.State.Checking -> R.string.version_checking
            is Updater.State.CheckFailed -> R.string.version_check_failed
            else -> R.string.version
        }
        tvAppVersion.text = getString(line, BuildConfig.VERSION_NAME)
        val state = Updater.state
        btnCheckUpdates.isEnabled = state !is Updater.State.Checking &&
            state !is Updater.State.Downloading && state !is Updater.State.Installing
    }

    private fun dp(value: Int): Int = (value * resources.displayMetrics.density).toInt()

    private fun copyDiagnostics() {
        val live = CameraStreamService.instance?.buildDiagnosticsReport()
            ?: (CameraStreamService.reportHeader() + "Not streaming\n" + CameraStreamService.cameraReport(
                runCatching { CameraStreamService.enumerateCameras(getSystemService(CAMERA_SERVICE) as CameraManager) }
                    .getOrDefault(emptyList())))
        val report = RecentRuns.report(live, RecentRuns.load(this))
        val clipboard = getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
        clipboard.setPrimaryClip(ClipData.newPlainText("Telescope diagnostics", report))
        Toast.makeText(this, R.string.diagnostics_copied, Toast.LENGTH_SHORT).show()
    }

    private fun reportBug() {
        copyDiagnostics()
        openUrl(BUG_REPORT_URL)
    }

    private fun openUrl(url: String) {
        runCatching { startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(url))) }
            .onFailure { Toast.makeText(this, getString(R.string.no_browser, url), Toast.LENGTH_LONG).show() }
    }

    companion object {
        private const val AUTHOR_URL = "https://github.com/LunarKittyy"
        private const val HELP_URL = "https://github.com/${UpdateLogic.REPO}/blob/master/docs/troubleshooting.md"
        private const val BUG_REPORT_URL = "https://github.com/${UpdateLogic.REPO}/issues/new?template=bug.yml"
    }
}
