package com.telescope

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.view.View
import android.view.WindowManager
import android.widget.ProgressBar
import android.widget.TextView
import android.widget.Toast
import androidx.activity.OnBackPressedCallback
import androidx.appcompat.widget.TooltipCompat
import java.util.concurrent.Executors

// Developer Camera2 probe screen: warns up front about duration and stand placement, tracks progress and lets the user cancel or copy the report.
class CameraProbeActivity : LookActivity() {

    private lateinit var btnBack: LookButton
    private lateinit var tvStatus: TextView
    private lateinit var tvDetail: TextView
    private lateinit var progressBar: ProgressBar
    private lateinit var btnStart: LookButton
    private lateinit var btnCancel: LookButton
    private lateinit var btnCopy: LookButton
    private lateinit var tvCacheNote: TextView
    private lateinit var cardPreview: View
    private lateinit var tvPreview: TextView

    private val mainHandler = Handler(Looper.getMainLooper())
    private val cacheLoader = Executors.newSingleThreadExecutor { r ->
        Thread(r, "camera-probe-cache").apply { isDaemon = true }
    }

    private var running = false
    private var cancelling = false
    private var startedMonoMs = 0L
    private var lastProgress: ProbeProgress? = null
    private var latestReport: String? = null

    private val sessionListener = object : ProbeSessionCoordinator.Listener {
        override fun onProgress(progress: ProbeProgress) {
            if (isDestroyed) return
            renderProgress(progress)
        }

        override fun onFinished(result: ProbeSessionCoordinator.FinishedResult) {
            if (isDestroyed) return
            renderFinished(result.data, result.reportText, result.savedToCache)
        }
    }

    // Refreshes the elapsed time once a second so long 15 s or 30 s exposure steps visibly tick forward.
    private val elapsedTicker = object : Runnable {
        override fun run() {
            if (!running) return
            val progress = lastProgress
            if (progress != null) {
                val elapsed = CameraProbeReport.formatElapsedMs((SystemClock.elapsedRealtime() - startedMonoMs).coerceAtLeast(0L))
                tvDetail.text = getString(R.string.probe_step_detail, progress.lensLabel, progress.stepLabel, elapsed)
            }
            mainHandler.postDelayed(this, 1_000L)
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_camera_probe)
        SystemBars.pad(findViewById(R.id.root), top = findViewById(R.id.header))
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)

        btnBack = findViewById(R.id.btnBack)
        tvStatus = findViewById(R.id.tvProbeStatus)
        tvDetail = findViewById(R.id.tvProbeDetail)
        progressBar = findViewById(R.id.progressProbe)
        btnStart = findViewById(R.id.btnStartProbe)
        btnCancel = findViewById(R.id.btnCancelProbe)
        btnCopy = findViewById(R.id.btnCopyReport)
        tvCacheNote = findViewById(R.id.tvProbeCacheNote)
        cardPreview = findViewById(R.id.cardReportPreview)
        tvPreview = findViewById(R.id.tvReportPreview)

        TooltipCompat.setTooltipText(btnBack, getString(R.string.probe_back_tooltip))
        TooltipCompat.setTooltipText(btnStart, getString(R.string.probe_start_tooltip))
        TooltipCompat.setTooltipText(btnCancel, getString(R.string.probe_cancel_tooltip))
        TooltipCompat.setTooltipText(btnCopy, getString(R.string.probe_copy_report_tooltip))

        btnBack.setOnClickListener { handleLeaveRequested() }
        btnStart.setOnClickListener { startProbe() }
        btnCancel.setOnClickListener { cancelProbe() }
        btnCopy.setOnClickListener { copyReport() }

        onBackPressedDispatcher.addCallback(this, object : OnBackPressedCallback(true) {
            override fun handleOnBackPressed() {
                handleLeaveRequested()
            }
        })

        // Reattach to any active or recently finished probe owned by CameraProbeSession so recreation keeps the run alive.
        val snap = CameraProbeSession.attach(sessionListener)
        when {
            snap.running -> renderRunning(snap.startedMonoMs, snap.lastProgress, snap.cancelling)
            snap.lastFinished != null ->
                renderFinished(snap.lastFinished.data, snap.lastFinished.reportText, snap.lastFinished.savedToCache)
            else -> loadCachedReport()
        }
    }

    override fun onStart() {
        super.onStart()
        SessionEndpoint.screenStarted(this)
    }

    override fun onStop() {
        SessionEndpoint.screenStopped()
        super.onStop()
    }

    override fun onDestroy() {
        mainHandler.removeCallbacks(elapsedTicker)
        CameraProbeSession.detach(sessionListener)
        // Only cancel when the screen is actually finishing; Activity recreation (theme or rotation) keeps the run alive.
        if (isFinishing && !isChangingConfigurations && CameraProbeSession.isRunning()) {
            CameraProbeSession.cancel("Screen closed")
        }
        cacheLoader.shutdownNow()
        super.onDestroy()
    }

    // Loads any previous probe report from cache off the main thread so Copy report is ready immediately.
    private fun loadCachedReport() {
        val file = CameraProbeCache.cacheFile(this)
        cacheLoader.execute {
            val cached = CameraProbeCache.read(file)
            if (cached != null) {
                mainHandler.post {
                    if (isDestroyed || running || latestReport != null) return@post
                    latestReport = cached
                    tvStatus.setText(R.string.probe_status_cached)
                    tvStatus.setTextColor(attrColor(R.attr.tsInk2))
                    tvCacheNote.setText(R.string.probe_cache_saved)
                    tvCacheNote.visibility = View.VISIBLE
                    showReportPreview(cached)
                    btnCopy.isEnabled = true
                }
            }
        }
    }

    private fun startProbe() {
        if (running) return
        when (val result = CameraProbeSession.start(this)) {
            is CameraProbeRunner.StartResult.Started -> {
                val snap = CameraProbeSession.snapshot()
                if (snap.running) {
                    renderRunning(snap.startedMonoMs, snap.lastProgress, snap.cancelling)
                } else if (snap.lastFinished != null) {
                    renderFinished(snap.lastFinished.data, snap.lastFinished.reportText, snap.lastFinished.savedToCache)
                }
            }
            is CameraProbeRunner.StartResult.StreamRunning -> {
                showRefused(R.string.probe_error_stream_running)
            }
            is CameraProbeRunner.StartResult.TooHot -> {
                showRefused(R.string.probe_error_too_hot)
            }
            is CameraProbeRunner.StartResult.NoPermission -> {
                showRefused(R.string.probe_error_no_permission)
            }
            is CameraProbeRunner.StartResult.Failed -> {
                tvStatus.text = getString(R.string.probe_status_failed, 0, 0)
                tvStatus.setTextColor(attrColor(R.attr.tsErr))
                tvDetail.text = result.detail
                tvDetail.visibility = View.VISIBLE
            }
        }
    }

    private fun renderRunning(startedMs: Long, progress: ProbeProgress?, isCancelling: Boolean) {
        running = true
        cancelling = isCancelling
        startedMonoMs = startedMs
        lastProgress = progress
        tvCacheNote.visibility = View.GONE
        progressBar.visibility = View.VISIBLE
        btnStart.visibility = View.GONE
        btnCancel.visibility = View.VISIBLE
        btnCancel.isEnabled = !isCancelling
        btnCopy.isEnabled = false
        if (isCancelling) {
            tvStatus.setText(R.string.probe_status_cancelling)
            tvStatus.setTextColor(attrColor(R.attr.tsWarn))
            if (progress != null) {
                val elapsed = CameraProbeReport.formatElapsedMs((SystemClock.elapsedRealtime() - startedMonoMs).coerceAtLeast(0L))
                tvDetail.text = getString(R.string.probe_step_detail, progress.lensLabel, progress.stepLabel, elapsed)
                tvDetail.visibility = View.VISIBLE
            } else {
                tvDetail.visibility = View.GONE
            }
        } else if (progress != null) {
            tvStatus.text = getString(R.string.probe_status_running, progress.lensIndex + 1, progress.totalLenses)
            tvStatus.setTextColor(attrColor(R.attr.tsInk))
            val elapsed = CameraProbeReport.formatElapsedMs((SystemClock.elapsedRealtime() - startedMonoMs).coerceAtLeast(0L))
            tvDetail.text = getString(R.string.probe_step_detail, progress.lensLabel, progress.stepLabel, elapsed)
            tvDetail.visibility = View.VISIBLE
        } else {
            tvStatus.setText(R.string.probe_status_starting)
            tvStatus.setTextColor(attrColor(R.attr.tsInk))
            tvDetail.visibility = View.GONE
        }
        mainHandler.removeCallbacks(elapsedTicker)
        mainHandler.postDelayed(elapsedTicker, 1_000L)
    }

    private fun showRefused(messageRes: Int) {
        tvStatus.setText(messageRes)
        tvStatus.setTextColor(attrColor(R.attr.tsWarn))
        tvDetail.visibility = View.GONE
    }

    private fun cancelProbe() {
        if (!running || cancelling) return
        cancelling = true
        btnCancel.isEnabled = false
        tvStatus.setText(R.string.probe_status_cancelling)
        tvStatus.setTextColor(attrColor(R.attr.tsWarn))
        CameraProbeSession.cancel("Cancelled by user")
    }

    private fun handleLeaveRequested() {
        if (!running) {
            finish()
            return
        }
        lookDialog()
            .setTitle(R.string.probe_leave_title)
            .setMessage(R.string.probe_leave_message)
            .setNegativeButton(R.string.probe_keep_running, null)
            .setPositiveButton(R.string.probe_stop_and_leave) { _, _ ->
                CameraProbeSession.cancel("Cancelled before leaving")
                finish()
            }
            .showLook(LookButton.Role.STOP)
    }

    private fun renderProgress(progress: ProbeProgress) {
        if (!running || cancelling) return
        lastProgress = progress
        tvStatus.text = getString(R.string.probe_status_running, progress.lensIndex + 1, progress.totalLenses)
        tvStatus.setTextColor(attrColor(R.attr.tsInk))
        val elapsed = CameraProbeReport.formatElapsedMs(progress.elapsedMs)
        tvDetail.text = getString(R.string.probe_step_detail, progress.lensLabel, progress.stepLabel, elapsed)
        tvDetail.visibility = View.VISIBLE
    }

    private fun renderFinished(data: ProbeReportData, reportText: String, savedToCache: Boolean) {
        running = false
        cancelling = false
        mainHandler.removeCallbacks(elapsedTicker)
        progressBar.visibility = View.GONE
        btnCancel.visibility = View.GONE
        btnStart.visibility = View.VISIBLE
        latestReport = reportText
        btnCopy.isEnabled = reportText.isNotBlank()

        val (statusText, toneAttr) = when (data.status) {
            ProbeRunStatus.COMPLETED ->
                getString(R.string.probe_status_finished, data.completedLenses, data.totalLenses) to R.attr.tsOk
            ProbeRunStatus.CANCELLED ->
                getString(R.string.probe_status_cancelled, data.completedLenses, data.totalLenses) to R.attr.tsWarn
            ProbeRunStatus.INTERRUPTED ->
                getString(R.string.probe_status_interrupted, data.completedLenses, data.totalLenses) to R.attr.tsWarn
            ProbeRunStatus.THERMAL_STOP ->
                getString(R.string.probe_status_thermal_stop, data.completedLenses, data.totalLenses) to R.attr.tsWarn
            ProbeRunStatus.FAILED ->
                getString(R.string.probe_status_failed, data.completedLenses, data.totalLenses) to R.attr.tsErr
        }
        tvStatus.text = statusText
        tvStatus.setTextColor(attrColor(toneAttr))

        val stopDetail = data.stopReason?.takeIf { it.isNotBlank() }
        if (stopDetail != null) {
            tvDetail.text = stopDetail
            tvDetail.visibility = View.VISIBLE
        } else {
            tvDetail.visibility = View.GONE
        }

        tvCacheNote.setText(if (savedToCache) R.string.probe_cache_saved else R.string.probe_cache_failed)
        tvCacheNote.visibility = View.VISIBLE
        showReportPreview(reportText)
    }

    private fun showReportPreview(reportText: String) {
        if (reportText.isBlank()) {
            cardPreview.visibility = View.GONE
            return
        }
        tvPreview.text = CameraProbeReport.preview(reportText)
        cardPreview.visibility = View.VISIBLE
    }

    private fun copyReport() {
        val text = latestReport ?: return
        val clipboard = getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
        clipboard.setPrimaryClip(ClipData.newPlainText("Telescope Camera2 probe", text))
        Toast.makeText(this, R.string.probe_report_copied, Toast.LENGTH_SHORT).show()
    }
}
