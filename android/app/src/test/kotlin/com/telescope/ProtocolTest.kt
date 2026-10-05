package com.telescope

import kotlinx.serialization.json.Json
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

class ProtocolTest {
    private fun state(cameraOff: Boolean) = V1State(
        cameras = emptyList(), auto = true, wb_manual = false, ois = true, focus_mode = "continuous",
        focus_distance = 0f, nr_mode = 1, edge_mode = 1, ae_comp = 0, black_level_lock = false, torch = false,
        jpeg_quality = 85, phone_fps = 30, camera_off = cameraOff, camera_toggle = true,
        stream_width = 1920, stream_height = 1080, battery = 80, charging = false, battery_temp_c = 30.0,
    )

    @Test
    fun `the state always says the camera can be turned off, since a missing field means an older phone`() {
        val json = Json.encodeToString(V1State.serializer(), state(cameraOff = false))
        assertTrue(json.contains("\"camera_toggle\":true"))
        assertFalse(json.contains("camera_off"))  // false is the default, left out like the rest
        assertTrue(Json.encodeToString(V1State.serializer(), state(cameraOff = true)).contains("\"camera_off\":true"))
    }
}
