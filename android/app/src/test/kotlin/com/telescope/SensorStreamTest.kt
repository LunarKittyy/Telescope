package com.telescope

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.float
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.long
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNotNull
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import kotlin.math.cos
import kotlin.math.sin

class SensorStreamTest {
    private fun wrap(deg: Int): Int = ((deg % 360) + 540) % 360 - 180

    private fun assertAngle(expected: Int, actual: Float?, what: String) {
        assertNotNull(actual, what)
        // -180 and 180 are the same tilt
        val diff = (((actual!! - expected) % 360f) + 540f) % 360f - 180f
        assertTrue(kotlin.math.abs(diff) < 0.01f, "$what: expected $expected, got $actual")
    }

    // Which way is up in device axes with the phone turned held degrees anticlockwise from upright portrait (90:
    // its top points left), leaning back by lean degrees (the screen tips towards the sky).
    private fun up(held: Int, lean: Double = 0.0): FloatArray {
        val h = Math.toRadians(held.toDouble())
        val l = Math.toRadians(lean)
        return floatArrayOf((sin(h) * cos(l) * 9.81).toFloat(), (cos(h) * cos(l) * 9.81).toFloat(), (sin(l) * 9.81).toFloat())
    }

    @Test
    fun `roll follows facing, sensor orientation and the way the phone is held`() {
        for (sensor in listOf(0, 90, 180, 270)) for (held in listOf(0, 90, 180, 270)) {
            // The back camera's picture is upright once the phone is turned as far as the sensor is; the front
            // camera's picture is unmirrored, so seen from the screen it turns the other way
            val back = PictureOrientation.of(up(held), LensLabel.BACK, sensor)!!
            assertAngle(wrap(held - sensor), back.roll, "back, sensor $sensor, held $held")
            assertEquals(0f, back.pitch, 0.01f)
            val front = PictureOrientation.of(up(held), LensLabel.FRONT, sensor)!!
            assertAngle(wrap(-(held + sensor)), front.roll, "front, sensor $sensor, held $held")
            assertEquals(0f, front.pitch, 0.01f)
        }
    }

    @Test
    fun `the usual phone gives a level picture held in landscape with its top to the left`() {
        // Back sensors are mostly mounted at 90 and front ones at 270
        assertAngle(0, PictureOrientation.of(up(90), LensLabel.BACK, 90)!!.roll, "back")
        assertAngle(0, PictureOrientation.of(up(90), LensLabel.FRONT, 270)!!.roll, "front")
        // Upright portrait: the back camera's horizon is turned a quarter anticlockwise, the selfie camera's clockwise
        assertAngle(-90, PictureOrientation.of(up(0), LensLabel.BACK, 90)!!.roll, "back portrait")
        assertAngle(90, PictureOrientation.of(up(0), LensLabel.FRONT, 270)!!.roll, "front portrait")
    }

    @Test
    fun `a small tilt shows as a small roll the right way round`() {
        // Turned a little past landscape (further anticlockwise): the back camera's picture turns clockwise
        assertAngle(5, PictureOrientation.of(up(95), LensLabel.BACK, 90)!!.roll, "back")
        assertAngle(-5, PictureOrientation.of(up(95), LensLabel.FRONT, 270)!!.roll, "front")
    }

    @Test
    fun `pitch is where the camera looks and roll stays put while leaning`() {
        for (sensor in listOf(0, 90, 180, 270)) for (held in listOf(0, 90, 180, 270)) {
            val back = PictureOrientation.of(up(held, lean = 30.0), LensLabel.BACK, sensor)!!
            assertEquals(-30f, back.pitch, 0.01f)  // the screen tips up, so the back camera looks down
            assertAngle(wrap(held - sensor), back.roll, "back leaning, sensor $sensor, held $held")
            val front = PictureOrientation.of(up(held, lean = 30.0), LensLabel.FRONT, sensor)!!
            assertEquals(30f, front.pitch, 0.01f)
            assertAngle(wrap(-(held + sensor)), front.roll, "front leaning, sensor $sensor, held $held")
        }
    }

    @Test
    fun `lying flat there is no roll to speak of`() {
        val back = PictureOrientation.of(floatArrayOf(0.3f, 0.2f, 9.8f), LensLabel.BACK, 90)!!
        assertNull(back.roll)
        assertEquals(-90f, back.pitch, 3f)
        val front = PictureOrientation.of(floatArrayOf(0f, 0f, 9.8f), LensLabel.FRONT, 270)!!
        assertNull(front.roll)
        assertEquals(90f, front.pitch, 0.01f)
    }

    @Test
    fun `an external camera or no reading gives nothing`() {
        assertNull(PictureOrientation.of(up(90), LensLabel.EXTERNAL, 0))
        assertNull(PictureOrientation.of(floatArrayOf(0f, 0f, 0f), LensLabel.BACK, 90))
    }

    @Test
    fun `asked kinds are the known ones named, or all of them`() {
        assertEquals(SensorKinds.ALL.toSet(), SensorKinds.asked(""))
        assertEquals(SensorKinds.ALL.toSet(), SensorKinds.asked("x=1"))
        assertEquals(setOf("orientation", "light"), SensorKinds.asked("kinds=orientation,light,sonar"))
        assertEquals(emptySet<String>(), SensorKinds.asked("kinds="))
    }

    @Test
    fun `hello line names the version and what's streamed`() {
        val hello = SensorWire.hello(listOf("orientation", "light"), 15_000L)
        assertTrue(hello.endsWith("\n"))
        val json = Json.parseToJsonElement(hello).jsonObject
        assertEquals(1L, json.getValue("version").jsonPrimitive.long)
        assertEquals(listOf("orientation", "light"), json.getValue("sensors").jsonArray.map { it.jsonPrimitive.content })
        assertEquals(15_000L, json.getValue("alive_ms").jsonPrimitive.long)
    }

    @Test
    fun `an update line is one JSON object per line`() {
        val line = SensorWire.line(1234L, linkedMapOf(
            "orientation" to SensorValue(1, floatArrayOf(Float.NaN, -12.3456f), "0"),
            "light" to SensorValue(2, floatArrayOf(250.5f)),
            "gravity" to SensorValue(3, floatArrayOf(0.123f, 9.8f, Float.POSITIVE_INFINITY)),
        ))
        assertTrue(line.endsWith("\n"))
        assertEquals(1, line.count { it == '\n' })
        val json = Json.parseToJsonElement(line).jsonObject
        assertEquals(1234L, json.getValue("t").jsonPrimitive.long)
        val orientation = json.getValue("orientation").jsonObject
        assertEquals(JsonNull, orientation.getValue("roll"))
        assertEquals(-12.35f, orientation.getValue("pitch").jsonPrimitive.float, 0.001f)
        assertEquals("0", orientation.getValue("camera").jsonPrimitive.content)
        assertEquals(250.5f, json.getValue("light").jsonPrimitive.float, 0.001f)
        val gravity = json.getValue("gravity").jsonArray
        assertEquals(0.12f, gravity[0].jsonPrimitive.float, 0.001f)
        assertEquals(JsonNull, gravity[2])
    }

    @Test
    fun `a line with nothing new is just the time`() {
        assertEquals("{\"t\":5}\n", SensorWire.line(5L, emptyMap()))
    }

    @Test
    fun `a quaternion without w gets the one that makes it unit length`() {
        val q = PhoneSensors.quaternion(floatArrayOf(0f, 0f, 0.6f))
        assertEquals(0.8f, q[3], 0.0001f)
        assertEquals(4, PhoneSensors.quaternion(floatArrayOf(0.1f, 0.2f, 0.3f, 0.9f, 0f)).size)
    }

    private class Registrations {
        val on = mutableSetOf<Int>()
        val log = mutableListOf<String>()
        val use = SensorUse({ on.add(it); log.add("+$it") }, { on.remove(it); log.add("-$it") })
    }

    @Test
    fun `a sensor is registered with the first desktop that needs it and unregistered with the last`() {
        val r = Registrations()
        val first = r.use.add(listOf(9))
        val second = r.use.add(listOf(9, 5))
        assertEquals(listOf("+9", "+5"), r.log)
        assertEquals(emptyList<Int>(), r.use.remove(listOf(9), first))
        assertEquals(setOf(9, 5), r.on)
        assertEquals(listOf(9, 5), r.use.remove(listOf(9, 5), second))
        assertTrue(r.on.isEmpty() && r.use.idle())
        assertEquals(listOf("+9", "+5", "-9", "-5"), r.log)
    }

    @Test
    fun `letting go of something never asked for changes nothing`() {
        val r = Registrations()
        val g = r.use.add(listOf(9))
        assertEquals(emptyList<Int>(), r.use.remove(listOf(4), g))
        assertEquals(setOf(9), r.on)
    }

    @Test
    fun `the stream ending turns everything off and a late goodbye can't touch the next stream`() {
        val r = Registrations()
        val old = r.use.add(listOf(9, 5))
        assertEquals(setOf(9, 5), r.use.removeAll().toSet())
        assertTrue(r.on.isEmpty())
        val fresh = r.use.add(listOf(9))
        assertEquals(emptyList<Int>(), r.use.remove(listOf(9, 5), old))  // the old connection's finally, late
        assertEquals(setOf(9), r.on)
        assertEquals(listOf(9), r.use.remove(listOf(9), fresh))
        assertTrue(r.on.isEmpty())
    }
}
