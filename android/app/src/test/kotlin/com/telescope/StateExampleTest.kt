package com.telescope

import kotlinx.serialization.descriptors.elementNames
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Test
import java.io.File

// docs/state.json is one /v1/state both apps' tests read: this one checks the phone still sends exactly that, and
// desktop/tests/test_state_example.py checks the desktop still reads it. Run with UPDATE_STATE_EXAMPLE=1 to rewrite it.
class StateExampleTest {
    // Every field off its default, so none is left out of the JSON
    private val example = V1State(
        cameras = listOf(CameraCapability(
            id = "2", logicalId = "0", label = "Main (wide)", current = true, hasOis = true,
            isoMin = 50, isoMax = 3200, shutterMinNs = 100_000L, shutterMaxNs = 1_000_000_000L,
            supportsManualSensor = true, supportsManualWB = true, supportsManualFocus = true, minFocusDistance = 10f,
            aeCompMin = -12, aeCompMax = 12, aeCompStep = 0.16666667f, supportsFlash = true, hwLevel = "FULL",
            supportedSizes = listOf(CameraSize(4032, 3024), CameraSize(1920, 1080)),
            h264Sizes = listOf(CameraSize(1920, 1080)),
            supportsFocusPoint = true, zoomRatioMax = 10f, cropZoomMax = 8f, freeformCrop = true,
            lensZooms = listOf(3f), maxFps = 60,
        )),
        auto = false, iso = 400, shutter_ns = 16_666_666L,
        wb_manual = true, wb_r = 2.1f, wb_ge = 1f, wb_go = 1f, wb_b = 1.8f,
        ois = false, focus_mode = "manual", focus_distance = 2.5f, nr_mode = 2, edge_mode = 2, ae_comp = -3,
        black_level_lock = true, torch = true, jpeg_quality = 90, phone_fps = 30, camera_fps = 29.8,
        codecs = listOf("mjpeg", "h264"), codec = "h264", bitrate = 8_087_040,
        dynamic_bitrate = true, dynamic_fps = 24, heat_fps = 15, low_light = true, low_light_toggle = true,
        h264_profile = "high", codec_error = "The phone's H.264 encoder stopped", codec_unsupported = true,
        active_lens = "2", camera_off = true, camera_error = "The camera is in use by another app",
        camera_taken = true, camera_toggle = true,
        stream_width = 1920, stream_height = 1080, battery = 87, charging = true, battery_temp_c = 32.5,
    )

    private val file = File("../../docs/state.json")
    private val pretty = Json { prettyPrint = true }

    @Test
    fun `the phone sends the state in docs-state-json`() {
        val sent = Json.encodeToString(V1State.serializer(), example)
        if (System.getenv("UPDATE_STATE_EXAMPLE") == "1") {
            file.writeText(pretty.encodeToString(JsonElement.serializer(), Json.parseToJsonElement(sent)) + "\n")
        }
        val expected = Json.parseToJsonElement(file.readText())
        assertEquals(expected, Json.parseToJsonElement(sent),
            "/v1/state changed: run with UPDATE_STATE_EXAMPLE=1 to rewrite docs/state.json, then make the desktop's test pass")
    }

    @Test
    fun `the example has every field, so a new one can't hide behind its default`() {
        val state = Json.parseToJsonElement(file.readText()).jsonObject
        assertEquals(V1State.serializer().descriptor.elementNames.toSet(), state.keys)
        val camera = state.getValue("cameras").jsonArray.first().jsonObject
        assertEquals(CameraCapability.serializer().descriptor.elementNames.toSet(), camera.keys)
    }

    @Test
    fun `it parses back into the same state`() {
        assertEquals(example, Json.decodeFromString(V1State.serializer(), file.readText()))
    }
}
