package com.telescope

import android.content.Context
import android.hardware.Sensor
import android.hardware.SensorEvent
import android.hardware.SensorEventListener
import android.hardware.SensorManager
import android.os.Handler
import android.os.HandlerThread
import kotlin.math.sqrt

// The phone's motion and light sensors for /v1/sensors. A sensor is only on while some desktop asks for a kind that
// needs it, and its events arrive on a thread of their own, so neither the main thread nor the camera waits on them.
class PhoneSensors(context: Context, private val lens: () -> Lens?) : SensorFeed {
    // The streaming camera, for turning gravity into the picture's roll and pitch.
    data class Lens(val id: String, val facing: String, val sensorOrientation: Int)

    private val manager = context.getSystemService(Context.SENSOR_SERVICE) as SensorManager?
    private val lock = Any()
    private val use = SensorUse(::register, ::unregister)
    private val readings = mutableMapOf<Int, SensorValue>()
    private var seq = 0L
    private var thread: HandlerThread? = null
    private var smoothedAccel: FloatArray? = null  // orientation from the accelerometer, on a phone without gravity

    private val listener = object : SensorEventListener {
        override fun onSensorChanged(event: SensorEvent) {
            val type = event.sensor.type
            var values = event.values.copyOf()
            if (type == Sensor.TYPE_GAME_ROTATION_VECTOR) values = quaternion(values)
            synchronized(lock) {
                if (!use.inUse(type)) return  // one last event after it was let go
                if (type == Sensor.TYPE_ACCELEROMETER) {
                    val prev = smoothedAccel
                    smoothedAccel = if (prev == null) values.copyOf() else FloatArray(3) { prev[it] + 0.25f * (values[it] - prev[it]) }
                }
                readings[type] = SensorValue(++seq, values)
            }
        }

        override fun onAccuracyChanged(sensor: Sensor, accuracy: Int) {}
    }

    private fun has(type: Int): Boolean = manager?.getDefaultSensor(type) != null

    override fun available(): List<String> = SensorKinds.ALL.filter(::offered)

    // Orientation only while the streaming camera is one of the phone's own: an external one's mounting isn't known
    private fun offered(kind: String): Boolean = needs(kind).isNotEmpty() &&
        (kind != SensorKinds.ORIENTATION || lens()?.facing.let { it == LensLabel.BACK || it == LensLabel.FRONT })

    // The sensor types a kind runs on here, empty when this phone doesn't have it.
    private fun needs(kind: String): List<Int> = kindTypes[kind].orEmpty()

    private val kindTypes: Map<String, List<Int>> by lazy { SensorKinds.ALL.associateWith(::lookUp) }

    private fun lookUp(kind: String): List<Int> {
        val type = when (kind) {
            SensorKinds.ORIENTATION -> if (has(Sensor.TYPE_GRAVITY)) Sensor.TYPE_GRAVITY else Sensor.TYPE_ACCELEROMETER
            SensorKinds.ACCELEROMETER -> Sensor.TYPE_ACCELEROMETER
            SensorKinds.GYROSCOPE -> Sensor.TYPE_GYROSCOPE
            SensorKinds.MAGNETOMETER -> Sensor.TYPE_MAGNETIC_FIELD
            SensorKinds.GRAVITY -> Sensor.TYPE_GRAVITY
            SensorKinds.ROTATION -> Sensor.TYPE_GAME_ROTATION_VECTOR
            SensorKinds.LIGHT -> Sensor.TYPE_LIGHT
            else -> return emptyList()
        }
        return if (has(type)) listOf(type) else emptyList()
    }

    override fun start(kinds: Set<String>): SensorHandle {
        val started = kinds.filter(::offered).toSet()
        synchronized(lock) { return SensorHandle(started, use.add(started.flatMap(::needs))) }
    }

    override fun stop(handle: SensorHandle) {
        synchronized(lock) { forget(use.remove(handle.kinds.flatMap(::needs), handle.generation)) }
    }

    // The stream ended: every sensor off, whatever a desktop's connection still has to say about it
    fun stopAll() {
        synchronized(lock) { forget(use.removeAll()) }
    }

    private fun forget(types: List<Int>) {
        for (type in types) {
            readings.remove(type)
            if (type == Sensor.TYPE_ACCELEROMETER) smoothedAccel = null
        }
        if (use.idle()) {
            thread?.quitSafely()
            thread = null
        }
    }

    private fun register(type: Int) {
        val sensor = manager?.getDefaultSensor(type) ?: return
        val looper = (thread ?: HandlerThread("sensors").also { it.start(); thread = it }).looper
        // Batched where the phone can: events may wait a little and come together, which lets the chip sleep between
        val (period, latency) = if (type == Sensor.TYPE_LIGHT) LIGHT_US to LIGHT_LATENCY_US else MOTION_US to MOTION_LATENCY_US
        manager.registerListener(listener, sensor, period, latency, Handler(looper))
    }

    private fun unregister(type: Int) {
        manager?.getDefaultSensor(type)?.let { manager.unregisterListener(listener, it) }
    }

    override fun latest(): Map<String, SensorValue> = synchronized(lock) {
        val out = mutableMapOf<String, SensorValue>()
        for (kind in SensorKinds.ALL) {
            val type = needs(kind).firstOrNull() ?: continue
            val value = readings[type] ?: continue
            if (kind == SensorKinds.ORIENTATION) {
                val camera = lens() ?: continue
                val up = if (type == Sensor.TYPE_ACCELEROMETER) smoothedAccel ?: continue else value.values
                val tilt = PictureOrientation.of(up, camera.facing, camera.sensorOrientation) ?: continue
                out[kind] = SensorValue(value.seq, floatArrayOf(tilt.roll ?: Float.NaN, tilt.pitch), camera.id)
            } else {
                out[kind] = value
            }
        }
        out
    }

    companion object {
        const val MOTION_US = 33_333   // about 30 a second
        const val LIGHT_US = 500_000   // light only reports changes anyway; the stream sends it twice a second at most
        const val MOTION_LATENCY_US = 100_000  // a tenth of a second late at most, which the desktop's smoothing hides
        const val LIGHT_LATENCY_US = 1_000_000

        // Some phones leave the game rotation vector's w out; it's the one that makes the quaternion unit length
        fun quaternion(v: FloatArray): FloatArray {
            if (v.size >= 4) return floatArrayOf(v[0], v[1], v[2], v[3])
            val x = v.getOrElse(0) { 0f }
            val y = v.getOrElse(1) { 0f }
            val z = v.getOrElse(2) { 0f }
            return floatArrayOf(x, y, z, sqrt(maxOf(0f, 1f - x * x - y * y - z * z)))
        }
    }
}
