package com.phonecam

import android.Manifest
import android.content.ComponentName
import android.content.Intent
import android.content.ServiceConnection
import android.content.pm.PackageManager
import android.graphics.ImageFormat
import android.hardware.camera2.CameraCharacteristics
import android.hardware.camera2.CameraManager
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import android.util.Size
import android.widget.*
import androidx.appcompat.app.AppCompatActivity
import androidx.core.app.ActivityCompat
import androidx.core.content.ContextCompat
import kotlin.math.sqrt

/**
 * logicalId: if non-null, this camera is a physical sub-camera of the given logical camera ID.
 * We must open logicalId and route the surface via OutputConfiguration.setPhysicalCameraId(id).
 */
data class CameraInfo(
    val id: String,
    val logicalId: String?,
    val label: String,
    val hasOis: Boolean,
    val supportedSizes: List<Size>
)

class MainActivity : AppCompatActivity() {

    private lateinit var spinnerCamera: Spinner
    private lateinit var spinnerResolution: Spinner
    private lateinit var btnToggle: Button
    private lateinit var checkOis: CheckBox
    private lateinit var tvStatus: TextView
    private lateinit var tvCameraList: TextView

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

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_main)

        spinnerCamera     = findViewById(R.id.spinnerCamera)
        spinnerResolution = findViewById(R.id.spinnerResolution)
        btnToggle         = findViewById(R.id.btnToggle)
        checkOis          = findViewById(R.id.checkOis)
        tvStatus          = findViewById(R.id.tvStatus)
        tvCameraList      = findViewById(R.id.tvCameraList)

        btnToggle.setOnClickListener { onToggleClicked() }

        spinnerCamera.onItemSelectedListener = object : AdapterView.OnItemSelectedListener {
            override fun onItemSelected(p: AdapterView<*>?, v: android.view.View?, pos: Int, id: Long) {
                populateResolutionSpinner(pos)
            }
            override fun onNothingSelected(p: AdapterView<*>?) {}
        }

        requestPermissionsIfNeeded()
    }

    override fun onStart() {
        super.onStart()
        bindService(Intent(this, CameraStreamService::class.java), serviceConnection, 0)
        uiHandler.post(statusPoller)
    }

    override fun onStop() {
        uiHandler.removeCallbacks(statusPoller)
        if (bound) { unbindService(serviceConnection); bound = false }
        super.onStop()
    }

    // ── Permissions ────────────────────────────────────────────────────────────

    private fun requestPermissionsIfNeeded() {
        val needed = mutableListOf(Manifest.permission.CAMERA)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU)
            needed += Manifest.permission.POST_NOTIFICATIONS
        val missing = needed.filter {
            ContextCompat.checkSelfPermission(this, it) != PackageManager.PERMISSION_GRANTED
        }
        if (missing.isEmpty()) loadCameras() else
            ActivityCompat.requestPermissions(this, missing.toTypedArray(), RC_PERMS)
    }

    override fun onRequestPermissionsResult(
        requestCode: Int, permissions: Array<String>, grantResults: IntArray
    ) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults)
        if (requestCode == RC_PERMS) {
            if (grantResults.all { it == PackageManager.PERMISSION_GRANTED }) loadCameras()
            else tvStatus.text = "Camera permission denied"
        }
    }

    // ── Camera enumeration ─────────────────────────────────────────────────────

    private fun loadCameras() {
        val manager = getSystemService(CAMERA_SERVICE) as CameraManager
        val result  = mutableListOf<CameraInfo>()
        val sb      = StringBuilder()

        // Helper: build a CameraInfo from a camera ID (logical or physical)
        fun buildInfo(id: String, logicalParent: String?): CameraInfo? = runCatching {
            val chars = manager.getCameraCharacteristics(id)

            val facing = when (chars.get(CameraCharacteristics.LENS_FACING)) {
                CameraCharacteristics.LENS_FACING_BACK     -> "Back"
                CameraCharacteristics.LENS_FACING_FRONT    -> "Front"
                CameraCharacteristics.LENS_FACING_EXTERNAL -> "Ext"
                else                                        -> "?"
            }

            val focalRaw = chars.get(CameraCharacteristics.LENS_INFO_AVAILABLE_FOCAL_LENGTHS)
                ?.firstOrNull() ?: 0f
            val sensor = chars.get(CameraCharacteristics.SENSOR_INFO_PHYSICAL_SIZE)
            val focalEq = if (sensor != null && focalRaw > 0f) {
                val diag = sqrt((sensor.width * sensor.width + sensor.height * sensor.height).toDouble()).toFloat()
                (focalRaw * 43.27f / diag).toInt()
            } else 0

            val oisModes = chars.get(CameraCharacteristics.LENS_INFO_AVAILABLE_OPTICAL_STABILIZATION)
            val hasOis   = oisModes?.contains(1) == true // 1 = LENS_OPTICAL_STABILIZATION_MODE_ON

            // SCALER_STREAM_CONFIGURATION_MAP can be null on physical sub-cameras
            val map   = chars.get(CameraCharacteristics.SCALER_STREAM_CONFIGURATION_MAP)
            val sizes = map?.getOutputSizes(ImageFormat.JPEG)
                ?.sortedByDescending { it.width * it.height }
                ?.takeIf { it.isNotEmpty() }
                ?: listOf(Size(1920, 1080), Size(1280, 720))   // safe fallback

            val prefix    = if (logicalParent != null) "[phys of $logicalParent] " else ""
            val focalStr  = if (focalEq > 0) "~${focalEq}mm eq" else "focal?"
            val oisStr    = if (hasOis) " OIS" else ""
            val label     = "ID $id  $facing  $focalStr$oisStr"

            sb.appendLine("$prefix[$id] $facing  $focalStr$oisStr")
            sb.appendLine("     sizes: ${sizes.take(3).joinToString { "${it.width}x${it.height}" }}…")

            CameraInfo(id, logicalParent, label, hasOis, sizes)
        }.getOrNull()

        // Pass 1: regular cameraIdList
        val topLevel = manager.cameraIdList
        topLevel.forEach { id ->
            buildInfo(id, null)?.let { result += it }
        }

        // Pass 2: physical sub-cameras from logical multi-camera groups (API 28+)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
            topLevel.forEach { logicalId ->
                runCatching {
                    val chars = manager.getCameraCharacteristics(logicalId)
                    val caps  = chars.get(CameraCharacteristics.REQUEST_AVAILABLE_CAPABILITIES)
                    val isLogicalMultiCam = caps?.contains(
                        CameraCharacteristics.REQUEST_AVAILABLE_CAPABILITIES_LOGICAL_MULTI_CAMERA
                    ) == true

                    if (isLogicalMultiCam) {
                        chars.physicalCameraIds.forEach { physId ->
                            if (result.none { it.id == physId }) {   // avoid duplicates
                                buildInfo(physId, logicalId)?.let { result += it }
                            }
                        }
                    }
                }
            }
        }

        cameras = result
        tvCameraList.text = sb.toString().trimEnd()

        val adapter = ArrayAdapter(this,
            android.R.layout.simple_spinner_item, cameras.map { it.label }
        ).also { it.setDropDownViewResource(android.R.layout.simple_spinner_dropdown_item) }
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
            android.R.layout.simple_spinner_item, labels
        ).also { it.setDropDownViewResource(android.R.layout.simple_spinner_dropdown_item) }
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
        }
        ContextCompat.startForegroundService(this, intent)

        if (bound) { unbindService(serviceConnection); bound = false }
        bindService(Intent(this, CameraStreamService::class.java), serviceConnection, 0)
    }

    // ── Status ─────────────────────────────────────────────────────────────────

    private fun updateStatusText() {
        val streaming = service?.isStreaming == true
        btnToggle.text = if (streaming) "Stop" else "Start Streaming"
        if (streaming) {
            val ip   = getDeviceIp()
            val port = service?.port ?: CameraStreamService.DEFAULT_PORT
            tvStatus.text = "Streaming\n\nWiFi: http://$ip:$port/video\nUSB:  http://localhost:$port/video"
        } else {
            tvStatus.text = "Not streaming"
        }
    }

    private fun getDeviceIp(): String = try {
        java.net.NetworkInterface.getNetworkInterfaces()
            ?.asSequence()
            ?.flatMap { it.inetAddresses.asSequence() }
            ?.firstOrNull { !it.isLoopbackAddress && it is java.net.Inet4Address }
            ?.hostAddress ?: "unknown"
    } catch (_: Exception) { "unknown" }

    companion object {
        private const val RC_PERMS = 100
    }
}
