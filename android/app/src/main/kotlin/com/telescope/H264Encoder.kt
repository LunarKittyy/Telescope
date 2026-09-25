package com.telescope

import android.media.MediaCodec
import android.media.MediaCodecInfo
import android.media.MediaCodecList
import android.media.MediaFormat
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.HandlerThread
import android.view.Surface

// Hardware H.264 from a camera Surface. Packets come out in Annex-B form, on the encoder's own thread.
class H264Encoder(
    width: Int,
    height: Int,
    fps: Int,
    bitrate: Int,
    private val onPacket: (bytes: ByteArray, key: Boolean, config: Boolean) -> Unit,
    private val onError: (Throwable) -> Unit,
) {
    companion object {
        private const val MIME = MediaFormat.MIMETYPE_VIDEO_AVC

        fun isAvailable(): Boolean = try {
            MediaCodecList(MediaCodecList.REGULAR_CODECS).codecInfos.any { info ->
                info.isEncoder && info.supportedTypes.any { it.equals(MIME, ignoreCase = true) }
            }
        } catch (_: Exception) { false }
    }

    private val codec: MediaCodec = MediaCodec.createEncoderByType(MIME)  // first: nothing to clean up if it throws
    private val thread = HandlerThread("h264-out").also { it.start() }
    val inputSurface: Surface
    @Volatile private var released = false

    init {
        try {
            val format = MediaFormat.createVideoFormat(MIME, width, height).apply {
                setInteger(MediaFormat.KEY_COLOR_FORMAT, MediaCodecInfo.CodecCapabilities.COLOR_FormatSurface)
                setInteger(MediaFormat.KEY_BIT_RATE, bitrate)
                setInteger(MediaFormat.KEY_FRAME_RATE, fps)
                setInteger(MediaFormat.KEY_I_FRAME_INTERVAL, 1)
                setInteger(MediaFormat.KEY_BITRATE_MODE, MediaCodecInfo.EncoderCapabilities.BITRATE_MODE_VBR)
                setInteger(MediaFormat.KEY_PRIORITY, 0)  // realtime
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) setInteger(MediaFormat.KEY_LOW_LATENCY, 1)
                // Baseline: no B-frames, so every packet can be shown as soon as it's decoded.
                setInteger(MediaFormat.KEY_PROFILE, MediaCodecInfo.CodecProfileLevel.AVCProfileBaseline)
            }
            codec.setCallback(object : MediaCodec.Callback() {
                override fun onInputBufferAvailable(mc: MediaCodec, index: Int) {}  // Surface input
                override fun onOutputBufferAvailable(mc: MediaCodec, index: Int, info: MediaCodec.BufferInfo) {
                    if (released) return
                    try {
                        val buf = mc.getOutputBuffer(index)
                        if (buf != null && info.size > 0) {
                            buf.position(info.offset)
                            buf.limit(info.offset + info.size)
                            val bytes = ByteArray(info.size).also { buf.get(it) }
                            val config = info.flags and MediaCodec.BUFFER_FLAG_CODEC_CONFIG != 0
                            val key = info.flags and MediaCodec.BUFFER_FLAG_KEY_FRAME != 0
                            onPacket(bytes, key, config)
                        }
                        mc.releaseOutputBuffer(index, false)
                    } catch (e: Exception) {
                        if (!released) onError(e)
                    }
                }
                override fun onError(mc: MediaCodec, e: MediaCodec.CodecException) { if (!released) onError(e) }
                override fun onOutputFormatChanged(mc: MediaCodec, format: MediaFormat) {}
            }, Handler(thread.looper))
            codec.configure(format, null, null, MediaCodec.CONFIGURE_FLAG_ENCODE)
            inputSurface = codec.createInputSurface()
            codec.start()
        } catch (e: Exception) {
            release()
            throw e
        }
    }

    fun requestKeyFrame() = setParam(MediaCodec.PARAMETER_KEY_REQUEST_SYNC_FRAME, 0)

    fun setBitrate(bps: Int) = setParam(MediaCodec.PARAMETER_KEY_VIDEO_BITRATE, bps)

    private fun setParam(key: String, value: Int) {
        if (released) return
        try { codec.setParameters(Bundle().apply { putInt(key, value) }) } catch (_: Exception) {}
    }

    fun release() {
        released = true
        try { codec.stop() } catch (_: Exception) {}
        try { codec.release() } catch (_: Exception) {}
        thread.quitSafely()
    }
}
