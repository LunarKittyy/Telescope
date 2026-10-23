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
    val label: String,  // English, for desktops that don't read facing, focalMm and lensKind
    val facing: String = "",  // "back", "front" or "external"
    val focalMm: Int = 0,  // 35 mm equivalent; 0 = not known
    val lensKind: String = "",  // "physical" for one lens of a multi-lens camera, "auto" for the camera itself
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
    val dynamic_fps: Int = 0,  // the rate Dynamic stepped down to for a slow link; 0 = the rate asked for
    val heat_fps: Int = 0,  // the rate Android's thermal status capped the camera at; 0 = not capped
    val low_light: Boolean = false,  // auto exposure may slow the camera down in the dark (and noise reduction is high)
    val low_light_toggle: Boolean = false,  // takes low_light; set by every phone that can
    val h264_profile: String? = null,  // "high", "baseline" or "auto" while H.264 runs
    val intra_refresh: Boolean = false,  // H.264 refreshes the picture over a second of frames instead of keyframes
    val wifi_band: String? = null,  // the phone's Wi-Fi: "2.4", "5" or "6" GHz, left out on mobile data or Android 9
    val wifi_rssi: Int? = null,     // its signal in dBm
    val wifi_mbps: Int? = null,     // its link speed
    val codec_error: String? = null,  // English, for desktops that don't read codec_error_code
    val codec_error_code: String? = null,  // see CodecFailure; the desktop words it itself, in its own language
    val codec_error_width: Int = 0,  // the size codec_error_code is about
    val codec_error_height: Int = 0,
    val codec_unsupported: Boolean = false,  // codec_error is H.264 not doing this size or rate here, not a crash
    val active_lens: String? = null,  // the lens a multi-lens camera is streaming from right now
    val camera_off: Boolean = false,  // streaming the mic with the camera closed
    val camera_error: String? = null,  // why the camera didn't turn back on, in English
    val camera_error_code: String? = null,  // "camera_in_use"; the desktop words it itself, in its own language
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
data class ApiError(
    val error: String,  // English, for desktops that don't read code
    val code: String? = null,  // a fixed identifier for the reason, which the desktop words in its own language
)
