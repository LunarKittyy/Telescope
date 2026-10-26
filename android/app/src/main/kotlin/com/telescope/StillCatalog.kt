package com.telescope

import android.graphics.ImageFormat
import android.hardware.camera2.CameraCharacteristics
import android.hardware.camera2.CameraManager
import android.hardware.camera2.CaptureRequest
import android.hardware.camera2.params.StreamConfigurationMap
import android.os.Build

// Reads what each lens can do for photos (StillCaps) from Camera2, once per lens.
object StillCatalog {
    fun read(manager: CameraManager, entry: CameraEntry): StillCaps? = runCatching {
        val chars = manager.getCameraCharacteristics(entry.id)
        val map = chars.get(CameraCharacteristics.SCALER_STREAM_CONFIGURATION_MAP) ?: return null
        val jpeg = largest(map, ImageFormat.JPEG) ?: return null
        val caps = chars.get(CameraCharacteristics.REQUEST_AVAILABLE_CAPABILITIES)?.toSet() ?: emptySet()
        // A lens of a multi-lens camera goes through its parent's session: its RAW needs the per-lens results of
        // Android 12, and the full resolution mode and the maker's modes only exist for the camera as a whole
        val physical = entry.logicalId != null

        val raw = if (CameraCharacteristics.REQUEST_AVAILABLE_CAPABILITIES_RAW in caps &&
            (!physical || Build.VERSION.SDK_INT >= Build.VERSION_CODES.S)) largest(map, ImageFormat.RAW_SENSOR) else null
        val heic = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) largest(map, ImageFormat.HEIC) else null
        val ultraHdr = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) largest(map, ImageFormat.JPEG_R) else null

        var maxResJpeg: Pair<Int, Int>? = null
        var maxResRaw: Pair<Int, Int>? = null
        if (!physical && Build.VERSION.SDK_INT >= Build.VERSION_CODES.S &&
            CameraCharacteristics.REQUEST_AVAILABLE_CAPABILITIES_ULTRA_HIGH_RESOLUTION_SENSOR in caps) {
            chars.get(CameraCharacteristics.SCALER_STREAM_CONFIGURATION_MAP_MAXIMUM_RESOLUTION)?.let { full ->
                maxResJpeg = largest(full, ImageFormat.JPEG)?.takeIf { pixels(it) > pixels(jpeg) }
                if (raw != null) maxResRaw = largest(full, ImageFormat.RAW_SENSOR)?.takeIf { pixels(it) > pixels(raw) }
            }
        }

        val extensions = if (!physical && Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) runCatching {
            val ext = manager.getCameraExtensionCharacteristics(entry.id)
            ext.supportedExtensions.filter { ext.getExtensionSupportedSizes(it, ImageFormat.JPEG).isNotEmpty() }
                .mapNotNull { StillOptions.EXTENSIONS[it] }
        }.getOrDefault(emptyList()) else emptyList()

        val exposure = chars.get(CameraCharacteristics.SENSOR_INFO_EXPOSURE_TIME_RANGE)
        val maxFrame = chars.get(CameraCharacteristics.SENSOR_INFO_MAX_FRAME_DURATION) ?: Long.MAX_VALUE
        val iso = chars.get(CameraCharacteristics.SENSOR_INFO_SENSITIVITY_RANGE)
        val hotPixel = chars.get(CameraCharacteristics.HOT_PIXEL_AVAILABLE_HOT_PIXEL_MODES)?.toSet() ?: emptySet()
        val distortion = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P)
            chars.get(CameraCharacteristics.DISTORTION_CORRECTION_AVAILABLE_MODES)?.toSet() ?: emptySet() else emptySet()

        StillCaps(
            jpeg = jpeg, heic = heic, ultraHdr = ultraHdr, raw = raw, maxResJpeg = maxResJpeg, maxResRaw = maxResRaw,
            extensions = extensions,
            manualSensor = entry.supportsManualSensor && exposure != null && iso != null,
            manualFocus = entry.supportsManualFocus,
            exposureMinNs = exposure?.lower ?: 0L,
            exposureMaxNs = minOf(exposure?.upper ?: 0L, maxFrame),
            isoMin = iso?.lower ?: 0, isoMax = iso?.upper ?: 0,
            minFocusDistance = entry.minFocusDistance,
            nrHq = CaptureRequest.NOISE_REDUCTION_MODE_HIGH_QUALITY in entry.nrModes,
            edgeHq = CaptureRequest.EDGE_MODE_HIGH_QUALITY in entry.edgeModes,
            hotPixelHq = CaptureRequest.HOT_PIXEL_MODE_HIGH_QUALITY in hotPixel,
            distortion = CaptureRequest.DISTORTION_CORRECTION_MODE_OFF in distortion && distortion.size > 1,
        )
    }.getOrNull()

    private fun pixels(size: Pair<Int, Int>): Long = size.first.toLong() * size.second

    private fun largest(map: StreamConfigurationMap, format: Int): Pair<Int, Int>? = runCatching {
        map.getOutputSizes(format)?.maxByOrNull { it.width.toLong() * it.height }?.let { it.width to it.height }
    }.getOrNull()
}
