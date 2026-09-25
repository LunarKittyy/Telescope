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
    private val running = AtomicBoolean(false)
    private var thread: Thread? = null

    companion object {
        fun permitted(context: Context): Boolean =
            ContextCompat.checkSelfPermission(context, Manifest.permission.RECORD_AUDIO) ==
                PackageManager.PERMISSION_GRANTED
    }

    fun permitted(): Boolean = permitted(context)

    /** False if recording couldn't start (no permission, or the mic is unavailable). */
    @SuppressLint("MissingPermission")  // checked by permitted()
    @Synchronized
    fun start(): Boolean {
        if (running.get()) return true
        if (!permitted()) return false
        val minBuf = AudioRecord.getMinBufferSize(AudioStream.SAMPLE_RATE,
            AudioFormat.CHANNEL_IN_MONO, AudioFormat.ENCODING_PCM_16BIT)
        if (minBuf <= 0) return false
        val record = try {
            AudioRecord(MediaRecorder.AudioSource.VOICE_COMMUNICATION, AudioStream.SAMPLE_RATE,
                AudioFormat.CHANNEL_IN_MONO, AudioFormat.ENCODING_PCM_16BIT,
                maxOf(minBuf, AudioStream.CHUNK_BYTES * 4))
        } catch (_: Exception) { return false }
        if (record.state != AudioRecord.STATE_INITIALIZED) { record.release(); return false }
        val effects = listOfNotNull(
            if (NoiseSuppressor.isAvailable()) NoiseSuppressor.create(record.audioSessionId) else null,
            if (AutomaticGainControl.isAvailable()) AutomaticGainControl.create(record.audioSessionId) else null,
        )
        effects.forEach { runCatching { it.enabled = true } }
        try { record.startRecording() } catch (_: Exception) {
            effects.forEach { it.release() }
            record.release()
            return false
        }
        running.set(true)
        thread = Thread({
            try {
                while (running.get()) {
                    val chunk = ByteArray(AudioStream.CHUNK_BYTES)
                    var n = 0
                    while (n < chunk.size && running.get()) {
                        val r = record.read(chunk, n, chunk.size - n)
                        if (r < 0) { running.set(false); break }
                        n += r
                    }
                    if (n == chunk.size) onChunk(chunk)
                }
            } finally {
                runCatching { record.stop() }
                effects.forEach { it.release() }
                record.release()
            }
        }, "mic-record").also { it.start() }
        return true
    }

    @Synchronized
    fun stop() {
        running.set(false)
        thread = null
    }
}
