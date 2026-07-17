package com.telescope

import kotlinx.serialization.Serializable

/**
 * Typed shapes for the v1 phone HTTP API, encoded/decoded through
 * kotlinx.serialization rather than hand-built JSON strings. Field names
 * mirror the wire format exactly (snake_case for the historically
 * snake_case top-level state fields, camelCase for camera capability
 * fields) so the desktop client, which is untouched here, keeps working
 * unchanged.
 */

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
    val battery: Int,
    val charging: Boolean,
    val battery_temp_c: Double,
)

@Serializable
data class ControlResult(val ok: Boolean, val error: String? = null)

@Serializable
data class ApiError(val error: String)
