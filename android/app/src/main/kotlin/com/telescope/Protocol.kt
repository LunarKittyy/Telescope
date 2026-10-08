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
    val h264Sizes: List<CameraSize>? = null,  // the supportedSizes the H.264 encoder takes; null = no encoder, or not said
    val supportsFocusPoint: Boolean = false,
    val zoomRatioMax: Float = 1f,
    val cropZoomMax: Float = 1f,
    val freeformCrop: Boolean = false,
    val lensZooms: List<Float> = emptyList(),
    val maxFps: Int = 0,  // the fastest AE target FPS range this camera lists; 0 = none listed
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
    val camera_fps: Double = 0.0,  // frames the phone actually made per second lately; 0 = not known
    val codecs: List<String> = listOf("mjpeg"),
    val codec: String = "mjpeg",
    val bitrate: Int = 0,
    val dynamic_bitrate: Boolean = false,  // takes bitrate -1 (Dynamic); older phones treat it as 0 (Auto)
    val codec_error: String? = null,
    val codec_unsupported: Boolean = false,  // codec_error is H.264 not doing this size or rate here, not a crash
    val active_lens: String? = null,  // the lens a multi-lens camera is streaming from right now
    val camera_off: Boolean = false,  // streaming the mic with the camera closed
    val camera_error: String? = null,  // why the camera didn't turn back on
    val camera_taken: Boolean = false,  // another app on the phone has the camera; it opens again once that lets go
    val camera_toggle: Boolean = false,  // takes camera_on and starts with the camera off; set by every phone that can
    val stream_width: Int,
    val stream_height: Int,
    val battery: Int,
    val charging: Boolean,
    val battery_temp_c: Double,
)

@Serializable
data class ControlResult(
    val ok: Boolean,
    val error: String? = null,
    val computer: String? = null,  // with busy_other: the computer the stream belongs to
)

@Serializable
data class ApiError(val error: String)
