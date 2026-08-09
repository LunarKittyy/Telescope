package com.telescope

import kotlinx.serialization.Serializable

// Unchanged field names for desktop compatibility.

@Serializable
data class CameraSize(
    val width: Int,
    val height: Int,
)

@Serializable
data class CameraCapability(
    val id: String,
    val logicalId: String? = null,
    val label: String,
    val current: Boolean,
    val hasOis: Boolean,
    val isoMin: Int,
    val isoMax: Int,
    val shutterMinNs: Long,
    val shutterMaxNs: Long,
    val supportsManualSensor: Boolean,
    val supportsManualWB: Boolean,
    val supportsManualFocus: Boolean,
    val minFocusDistance: Float,
    val aeCompMin: Int,
    val aeCompMax: Int,
    val aeCompStep: Float,
    val supportsFlash: Boolean,
    val hwLevel: String,
    val supportedSizes: List<CameraSize> = emptyList(),
)

@Serializable
data class V1State(
    val cameras: List<CameraCapability>,
    val auto: Boolean,
    val iso: Int? = null,
    val shutter_ns: Long? = null,
    val wb_manual: Boolean,
    val wb_r: Float? = null,
    val wb_ge: Float? = null,
    val wb_go: Float? = null,
    val wb_b: Float? = null,
    val ois: Boolean,
    val focus_mode: String,
    val focus_distance: Float,
    val nr_mode: Int,
    val edge_mode: Int,
    val ae_comp: Int,
    val black_level_lock: Boolean,
    val torch: Boolean,
    val jpeg_quality: Int,
    val phone_fps: Int,
    val stream_width: Int,
    val stream_height: Int,
    val battery: Int,
    val charging: Boolean,
    val battery_temp_c: Double,
)

@Serializable
data class ControlResult(val ok: Boolean, val error: String? = null)

@Serializable
data class ApiError(val error: String)
