package com.telescope

import kotlin.math.asin
import kotlin.math.atan2
import kotlin.math.cos
import kotlin.math.sin
import kotlin.math.sqrt

// The kinds /v1/sensors streams, by the names the desktop asks for them.
object SensorKinds {
    const val ORIENTATION = "orientation"     // roll and pitch of the streamed picture, worked out on the phone
    const val ACCELEROMETER = "accelerometer"  // m/s², device axes
    const val GYROSCOPE = "gyroscope"          // rad/s, device axes
    const val MAGNETOMETER = "magnetometer"    // µT, device axes
    const val GRAVITY = "gravity"              // m/s², device axes (Android's fused TYPE_GRAVITY)
    const val ROTATION = "rotation"            // quaternion x, y, z, w (TYPE_GAME_ROTATION_VECTOR: no compass)
    const val LIGHT = "light"                  // lux

    val ALL = listOf(ORIENTATION, ACCELEROMETER, GYROSCOPE, MAGNETOMETER, GRAVITY, ROTATION, LIGHT)

    // The kinds=a,b,c a request names (unknown ones dropped), or every kind when it names none.
    fun asked(query: String): Set<String> {
        val named = query.split('&').firstOrNull { it.startsWith("kinds=") }?.removePrefix("kinds=")
            ?: return ALL.toSet()
        return named.split(',', '+').map { it.trim() }.filter { it in ALL }.toSet()
    }
}

// What /v1/sensors streams from: PhoneSensors in the app, a fake in tests.
interface SensorFeed {
    fun available(): List<String>
    // Starts what kinds needs (counted per desktop) and returns the kinds it started; stop() takes that same set.
    fun start(kinds: Set<String>): Set<String>
    fun stop(kinds: Set<String>)
    // The newest reading of each started kind; seq goes up with every new one.
    fun latest(): Map<String, SensorValue>
}

// For orientation: values are roll (NaN when the camera points straight up or down) and pitch, camera the streaming lens.
class SensorValue(val seq: Long, val values: FloatArray, val camera: String? = null)

// Turns which way is up (the gravity sensor's reading, device axes) into how the streamed picture is tilted.
// The stream is the sensor's picture as it comes off the camera, never turned to the screen (the app is portrait-only
// and the encoder ignores the preview's transform), so the way the phone is held is all in the gravity reading.
object PictureOrientation {
    // roll: degrees the horizon is turned clockwise in the picture, -180..180 (null when the camera points within
    // about 10 degrees of straight up or down, where it means nothing). pitch: degrees the camera looks above the horizon.
    data class Tilt(val roll: Float?, val pitch: Float)

    private const val MIN_LEVEL = 0.17  // sin(10°): less of "up" than this in the picture plane and roll is noise

    fun of(up: FloatArray, facing: String, sensorOrientation: Int): Tilt? {
        if (facing != LensLabel.BACK && facing != LensLabel.FRONT) return null  // an external camera's mounting isn't known
        val len = sqrt((up[0] * up[0] + up[1] * up[1] + up[2] * up[2]).toDouble())
        if (len < 1e-3 || len.isNaN()) return null
        val gx = up[0] / len
        val gy = up[1] / len
        val gz = up[2] / len
        val s = Math.toRadians(sensorOrientation.toDouble())
        // The picture's up and right, and where the camera looks, in device axes (x right, y up, z out of the screen).
        // SENSOR_ORIENTATION is the clockwise turn that makes the picture upright on the screen; the front camera's
        // picture comes unmirrored, so seen from the screen its up turns the other way.
        val front = facing == LensLabel.FRONT
        val ux = if (front) -sin(s) else sin(s)
        val uy = cos(s)
        val fz = if (front) 1.0 else -1.0
        // right = forward × up, with forward (0, 0, fz) and up (ux, uy, 0)
        val rx = -fz * uy
        val ry = fz * ux
        val alongUp = gx * ux + gy * uy
        val alongRight = gx * rx + gy * ry
        val pitch = Math.toDegrees(asin((gz * fz).coerceIn(-1.0, 1.0))).toFloat()
        val roll = if (sqrt(alongUp * alongUp + alongRight * alongRight) < MIN_LEVEL) null
                   else Math.toDegrees(atan2(alongRight, alongUp)).toFloat()
        return Tilt(roll, pitch)
    }
}

// /v1/sensors is newline-delimited JSON: a hello line saying what's streamed, then one line per update with the kinds
// that changed. Built by hand: it goes out 30 times a second and only ever holds numbers and fixed names.
object SensorWire {
    const val CONTENT_TYPE = "application/x-ndjson"
    const val VERSION = 1

    fun hello(kinds: Collection<String>): String =
        "{\"version\":$VERSION,\"sensors\":[${kinds.joinToString(",") { "\"$it\"" }}]}\n"

    // t: the phone's clock in ms. values: kind to reading, orientation as roll (NaN = null) and pitch.
    fun line(tMs: Long, values: Map<String, SensorValue>): String {
        val sb = StringBuilder(96).append("{\"t\":").append(tMs)
        for ((kind, value) in values) {
            sb.append(",\"").append(kind).append("\":")
            if (kind == SensorKinds.ORIENTATION) {
                sb.append("{\"roll\":").append(number(value.values.getOrElse(0) { Float.NaN }))
                    .append(",\"pitch\":").append(number(value.values.getOrElse(1) { Float.NaN }))
                if (value.camera != null) sb.append(",\"camera\":\"").append(escape(value.camera)).append('"')
                sb.append('}')
            } else if (kind == SensorKinds.LIGHT) {
                sb.append(number(value.values.getOrElse(0) { Float.NaN }))
            } else {
                value.values.joinTo(sb, ",", "[", "]") { number(it) }
            }
        }
        return sb.append("}\n").toString()
    }

    // Two decimals is far finer than any of these sensors; NaN and infinity aren't JSON
    private fun number(v: Float): String =
        if (!v.isFinite()) "null" else (Math.round(v * 100.0) / 100.0).toString()

    private fun escape(s: String): String = s.filter { it != '"' && it != '\\' && it >= ' ' }
}
