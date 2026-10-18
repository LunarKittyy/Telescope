package com.telescope

import android.Manifest
import android.annotation.SuppressLint
import android.content.Context
import android.content.pm.PackageManager
import android.media.AudioFormat
import android.media.AudioRecord
import android.media.MediaRecorder
import android.media.audiofx.AutomaticGainControl
import android.media.audiofx.NoiseSuppressor
import androidx.core.content.ContextCompat
import java.util.concurrent.atomic.AtomicBoolean

// Records the microphone only while at least one listener is connected, and hands each 10 ms chunk
// to [onChunk]. start() is idempotent; stop() ends recording.
class AudioStreamer(private val context: Context, private val onChunk: (ByteArray) -> Unit) {
    // One flag per recording: a stop() then quick start() mustn't let the old thread (mid-read) carry on beside the new one.
    private var running: AtomicBoolean? = null

    companion object {
        fun permitted(context: Context): Boolean =
            ContextCompat.checkSelfPermission(context, Manifest.permission.RECORD_AUDIO) ==
                PackageManager.PERMISSION_GRANTED
    }

    fun permitted(): Boolean = permitted(context)

    /** False if recording couldn't start (no permission, or the mic is unavailable). */
    @Synchronized
    fun start(): Boolean {
        if (running?.get() == true) return true
        if (!permitted()) return false
        val first = open() ?: return false
        val active = AtomicBoolean(true).also { running = it }
        // The audio server restarting ends or freezes the recording: open a new one rather than stay quiet
        val loop = MicLoop(::open, onChunk, active)
        Thread({ loop.run(first) }, "mic-record").start()
        Thread({
            while (active.get()) {
                try { Thread.sleep(MicLoop.STALL_MS / 2) } catch (_: InterruptedException) { break }
                if (active.get() && loop.checkStall()) android.util.Log.i("AudioStreamer", "No audio for a while, reopening the mic")
            }
        }, "mic-watchdog").apply { isDaemon = true }.start()
        return true
    }

    @SuppressLint("MissingPermission")  // checked by permitted()
    private fun open(): MicSource? {
        if (!permitted()) return null
        val minBuf = AudioRecord.getMinBufferSize(AudioStream.SAMPLE_RATE,
            AudioFormat.CHANNEL_IN_MONO, AudioFormat.ENCODING_PCM_16BIT)
        if (minBuf <= 0) return null
        val record = try {
            AudioRecord(MediaRecorder.AudioSource.CAMCORDER, AudioStream.SAMPLE_RATE,
                AudioFormat.CHANNEL_IN_MONO, AudioFormat.ENCODING_PCM_16BIT,
                maxOf(minBuf, AudioStream.CHUNK_BYTES * 4))
        } catch (_: Exception) { return null }
        if (record.state != AudioRecord.STATE_INITIALIZED) { record.release(); return null }
        val effects = listOfNotNull(
            runCatching { if (NoiseSuppressor.isAvailable()) NoiseSuppressor.create(record.audioSessionId) else null }.getOrNull(),
            runCatching { if (AutomaticGainControl.isAvailable()) AutomaticGainControl.create(record.audioSessionId) else null }.getOrNull(),
        )
        effects.forEach { runCatching { it.enabled = true } }
        try { record.startRecording() } catch (_: Exception) {
            effects.forEach { runCatching { it.release() } }
            record.release()
            return null
        }
        if (record.recordingState != AudioRecord.RECORDSTATE_RECORDING) {
            effects.forEach { runCatching { it.release() } }
            record.release()
            return null  // another app has the mic
        }
        return object : MicSource {
            override fun read(buf: ByteArray, off: Int, len: Int) = record.read(buf, off, len)
            override fun stop() { record.stop() }
            override fun release() {
                effects.forEach { runCatching { it.release() } }
                record.release()
            }
        }
    }

    @Synchronized
    fun stop() {
        running?.set(false)
        running = null
    }
}
