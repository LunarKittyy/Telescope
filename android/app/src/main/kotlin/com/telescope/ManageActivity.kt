package com.telescope

import android.app.Activity
import android.content.res.ColorStateList
import android.os.Bundle
import android.view.View
import android.widget.ImageView
import android.widget.LinearLayout
import android.widget.TextView
import androidx.core.content.res.ResourcesCompat
import com.google.android.material.button.MaterialButton

// The paired computers, opened from the pairing card on Main: each with its delete button, Remove all, and Pair new
class ManageActivity : LookActivity() {

    private lateinit var layoutComputers: LinearLayout
    private lateinit var tvNoComputers: TextView
    private lateinit var btnPairNew: LookButton
    private lateinit var btnRemoveAll: LookButton

    private val pairingListener: () -> Unit = { runOnUiThread { render() } }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_manage)
        SystemBars.pad(findViewById(R.id.root), top = findViewById(R.id.header))
        findViewById<View>(R.id.btnBack).setOnClickListener { finish() }

        layoutComputers = findViewById(R.id.layoutComputers)
        tvNoComputers = findViewById(R.id.tvNoComputers)
        btnPairNew = findViewById(R.id.btnPairNew)
        btnRemoveAll = LookButton(this).apply {
            setText(R.string.remove_all)
            setOnClickListener { confirmRemoveAll() }
        }
        findViewById<CardHeader>(R.id.headerComputers).setAction(btnRemoveAll)
        // Pairing lives on Main: it closes this screen and asks for the scanner
        btnPairNew.setOnClickListener {
            setResult(RESULT_PAIR_NEW)
            finish()
        }
    }

    override fun onStart() {
        super.onStart()
        SessionEndpoint.screenStarted(this)  // the phone stays reachable while this is up
        PairedComputers.addListener(pairingListener)
        render()
    }

    override fun onStop() {
        PairedComputers.removeListener(pairingListener)
        SessionEndpoint.screenStopped()
        super.onStop()
    }

    // Newest first, all of them: the screen scrolls. Remove all only once there are two to remove; with none, Pair new is the main action.
    private fun render() {
        val paired = PairedComputers.list(this)
        layoutComputers.removeAllViews()
        paired.newestFirst.forEach { layoutComputers.addView(buildComputerRow(it)) }
        btnRemoveAll.visibility = if (paired.offersRemoveAll) View.VISIBLE else View.GONE
        tvNoComputers.visibility = if (paired.computers.isEmpty()) View.VISIBLE else View.GONE
        btnPairNew.setRole(
            if (paired.computers.isEmpty()) LookButton.Role.PRIMARY else LookButton.Role.PLAIN,
            attrColor(R.attr.tsHueStreaming),
        )
    }

    private fun buildComputerRow(computer: PairedComputer): View {
        // An inset row like the desktop's phone list: the computer's icon, its name and when it was paired
        val row = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = android.view.Gravity.CENTER_VERTICAL
            setBackgroundResource(R.drawable.bg_inset)
            setPadding(dp(12), dp(10), dp(10), dp(10))
            layoutParams = LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.MATCH_PARENT, LinearLayout.LayoutParams.WRAP_CONTENT,
            ).apply { bottomMargin = dp(8) }
        }
        row.addView(ImageView(this).apply {
            setImageResource(R.drawable.ic_monitor)
            imageTintList = ColorStateList.valueOf(attrColor(R.attr.tsHueStreaming))
            importantForAccessibility = View.IMPORTANT_FOR_ACCESSIBILITY_NO
            layoutParams = LinearLayout.LayoutParams(dp(20), dp(20)).apply { marginEnd = dp(12) }
        })
        val textBlock = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            layoutParams = LinearLayout.LayoutParams(0, LinearLayout.LayoutParams.WRAP_CONTENT, 1f)
        }
        textBlock.addView(TextView(this).apply {
            text = computer.name
            setTextAppearance(R.style.TextAppearance_Telescope_Body)
            typeface = ResourcesCompat.getFont(this@ManageActivity, R.font.ibm_plex_sans_semibold)
        })
        textBlock.addView(TextView(this).apply {
            val date = java.text.DateFormat.getDateInstance(java.text.DateFormat.MEDIUM)
                .format(java.util.Date(computer.pairedAtMs))
            text = getString(R.string.paired_on, date)
            setTextAppearance(R.style.TextAppearance_Telescope_Hint)
        })
        row.addView(textBlock)
        // An icon, so a long name or a large font keeps the row's width instead of breaking mid-word
        row.addView(LookButton(this).apply {
            setIconResource(R.drawable.ic_trash)
            iconSize = dp(18)
            iconPadding = 0
            iconGravity = MaterialButton.ICON_GRAVITY_TEXT_START
            setPadding(0, 0, 0, 0)
            minimumWidth = 0
            minWidth = 0
            minimumHeight = 0
            minHeight = 0
            contentDescription = getString(R.string.remove_computer_title, computer.name)
            androidx.appcompat.widget.TooltipCompat.setTooltipText(this, getString(R.string.remove))
            setOnClickListener { confirmRemoveComputer(computer) }
            // Start, not left: in Persian or Arabic the button sits on the left, and the gap goes on its right
            layoutParams = LinearLayout.LayoutParams(dp(44), dp(48)).apply { marginStart = dp(12) }
        })
        return row
    }

    // Every computer's token goes at once; pairing any of them again starts from scratch
    private fun confirmRemoveAll() {
        lookDialog()
            .setTitle(R.string.remove_all_title)
            .setMessage(R.string.remove_all_message)
            .setNegativeButton(R.string.cancel, null)
            .setPositiveButton(R.string.remove_all) { _, _ -> PairedComputers.removeAll(this) }
            .showLook(LookButton.Role.DANGER)
    }

    // Removing a computer revokes only its token; the others keep working.
    private fun confirmRemoveComputer(computer: PairedComputer) {
        lookDialog()
            .setTitle(getString(R.string.remove_computer_title, computer.name))
            .setMessage(R.string.remove_computer_message)
            .setNegativeButton(R.string.cancel, null)
            .setPositiveButton(R.string.remove) { _, _ -> PairedComputers.remove(this, computer.id) }
            .showLook(LookButton.Role.DANGER)
    }

    private fun dp(value: Int): Int = (value * resources.displayMetrics.density).toInt()

    companion object {
        // Result code: Pair new was tapped, so Main starts the scanner
        const val RESULT_PAIR_NEW = Activity.RESULT_FIRST_USER
    }
}
