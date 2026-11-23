package com.telescope

import android.graphics.ImageFormat
import android.hardware.camera2.CameraCharacteristics
import android.hardware.camera2.CameraManager
import android.media.MediaCodecInfo
import android.media.MediaCodecList
import android.os.Build

// One video encoder the phone has, for Copy diagnostics
data class EncoderEntry(val codec: String, val name: String, val hardware: Boolean)

// What Copy diagnostics adds beyond the camera lines: RAW support per camera and the phone's video encoders
object DiagnosticsExtras {
    private val CODECS = mapOf("video/avc" to "H.264", "video/hevc" to "HEVC")

    // ", RAW 4000x3000" when the lens can shoot a DNG, ", no RAW" when it can't, "" when it couldn't be read
    fun rawNote(read: Boolean, raw: Pair<Int, Int>?): String = when {
        !read -> ""
        raw == null -> ", no RAW"
        else -> ", RAW ${raw.first}x${raw.second}"
    }

    // "Video encoders: H.264 hw name; H.264 sw name; HEVC hw name", or a line saying there are none
    fun encoderLine(encoders: List<EncoderEntry>): String =
        if (encoders.isEmpty()) "Video encoders: none found"
        else "Video encoders: " + encoders.sortedWith(compareBy({ it.codec }, { !it.hardware }, { it.name }))
            .joinToString("; ") { "${it.codec} ${if (it.hardware) "hw" else "sw"} ${it.name}" }

    // Largest RAW size per camera id; a missing id means that lens couldn't be read, a null value that it has no RAW
    fun readRaw(manager: CameraManager, ids: Collection<String>): Map<String, Pair<Int, Int>?> {
        val out = mutableMapOf<String, Pair<Int, Int>?>()
        for (id in ids) runCatching {
            val chars = manager.getCameraCharacteristics(id)
            val caps = chars.get(CameraCharacteristics.REQUEST_AVAILABLE_CAPABILITIES)
            out[id] = if (caps?.contains(CameraCharacteristics.REQUEST_AVAILABLE_CAPABILITIES_RAW) == true)
                chars.get(CameraCharacteristics.SCALER_STREAM_CONFIGURATION_MAP)?.getOutputSizes(ImageFormat.RAW_SENSOR)
                    ?.maxByOrNull { it.width.toLong() * it.height }?.let { it.width to it.height }
            else null
        }
        return out
    }

    fun readEncoders(): List<EncoderEntry> = runCatching {
        MediaCodecList(MediaCodecList.REGULAR_CODECS).codecInfos.filter { it.isEncoder }.flatMap { info ->
            info.supportedTypes.mapNotNull { type ->
                CODECS[type.lowercase()]?.let { EncoderEntry(it, info.name, isHardware(info)) }
            }
        }
    }.getOrDefault(emptyList())

    private fun isHardware(info: MediaCodecInfo): Boolean =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) info.isHardwareAccelerated
        else !info.name.startsWith("OMX.google.") && !info.name.startsWith("c2.android.")
}
