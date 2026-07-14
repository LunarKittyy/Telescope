package com.telescope

import android.graphics.ImageFormat
import android.hardware.camera2.CameraCharacteristics
import android.hardware.camera2.CameraManager
import android.os.Build
import android.util.Size
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

object CameraCatalog {
    fun enumerate(manager: CameraManager, debugOut: StringBuilder? = null): List<CameraInfo> {
        val result = mutableListOf<CameraInfo>()

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
            val hasOis   = oisModes?.contains(1) == true

            val map   = chars.get(CameraCharacteristics.SCALER_STREAM_CONFIGURATION_MAP)
            val sizes = map?.getOutputSizes(ImageFormat.JPEG)
                ?.sortedByDescending { it.width * it.height }
                ?.takeIf { it.isNotEmpty() }
                ?: listOf(Size(1920, 1080), Size(1280, 720))

            val prefix    = if (logicalParent != null) "[phys of $logicalParent] " else ""
            val focalStr  = if (focalEq > 0) "~${focalEq}mm eq" else "focal?"
            val oisStr    = if (hasOis) " OIS" else ""
            val label     = "ID $id  $facing  $focalStr$oisStr"

            debugOut?.appendLine("$prefix[$id] $facing  $focalStr$oisStr")
            debugOut?.appendLine("     sizes: ${sizes.take(3).joinToString { "${it.width}x${it.height}" }}…")

            CameraInfo(id, logicalParent, label, hasOis, sizes)
        }.getOrNull()

        val topLevel = manager.cameraIdList
        topLevel.forEach { id ->
            buildInfo(id, null)?.let { result += it }
        }

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
                            if (result.none { it.id == physId }) {
                                buildInfo(physId, logicalId)?.let { result += it }
                            }
                        }
                    }
                }
            }
        }

        return result
    }
}
