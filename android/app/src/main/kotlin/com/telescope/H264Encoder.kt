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
    // Optional settings. Some encoders refuse one of them outright (an Exynos S10+ threw IllegalArgumentException
    // at 1080p), so a refused setup is retried with a fresh encoder and fewer of them.
    enum class Extra { LOW_LATENCY, REALTIME, BASELINE }

    companion object {
        private const val MIME = MediaFormat.MIMETYPE_VIDEO_AVC

        val ATTEMPTS: List<Set<Extra>> = listOf(Extra.values().toSet(), setOf(Extra.BASELINE), emptySet())

        fun isAvailable(): Boolean = try {
            MediaCodecList(MediaCodecList.REGULAR_CODECS).codecInfos.any { info ->
                info.isEncoder && info.supportedTypes.any { it.equals(MIME, ignoreCase = true) }
            }
        } catch (_: Exception) { false }

        /** The most the phone's first H.264 encoder takes (the one createEncoderByType picks), for Dynamic's ceiling. */
        val maxBitrate: Int by lazy {
            try {
                MediaCodecList(MediaCodecList.REGULAR_CODECS).codecInfos
                    .firstOrNull { info -> info.isEncoder && info.supportedTypes.any { it.equals(MIME, ignoreCase = true) } }
                    ?.getCapabilitiesForType(MIME)?.videoCapabilities?.bitrateRange?.upper ?: H264Stream.MAX_BPS
            } catch (_: Exception) { H264Stream.MAX_BPS }
        }
    }

    private var bitrateRange = android.util.Range(1, Int.MAX_VALUE)  // this encoder's own, once it's made

    private val thread = HandlerThread("h264-out").also { it.start() }
    @Volatile private var released = false
    private val codec: MediaCodec
    val inputSurface: Surface

    private val callback = object : MediaCodec.Callback() {
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
    }

    init {
        var opened: Pair<MediaCodec, Surface>? = null
        var failure: Exception? = null
        for (extras in ATTEMPTS) {
            val mc = try { MediaCodec.createEncoderByType(MIME) } catch (e: Exception) { failure = e; break }
            val caps = try { mc.codecInfo.getCapabilitiesForType(MIME).videoCapabilities } catch (_: Exception) { null }
            // 4:3 4K is past what H.264 encoders do (its usual 4K tops out near 4096x2304), and the camera then fails
            // feeding it: say so now, and the stream goes to MJPEG instead. Only the size: encoders often claim lower
            // frame rates than they manage.
            if (caps != null && !fits(caps, width, height)) {
                try { mc.release() } catch (_: Exception) {}
                failure = IllegalArgumentException("${width}x$height is more than this phone's H.264 encoder takes")
                break
            }
            caps?.bitrateRange?.let { bitrateRange = it }
            try {
                mc.setCallback(callback, Handler(thread.looper))
                mc.configure(format(width, height, fps, bitrate, extras), null, null, MediaCodec.CONFIGURE_FLAG_ENCODE)
                val surface = mc.createInputSurface()
                mc.start()
                opened = mc to surface
                break
            } catch (e: Exception) {
                failure = e
                try { mc.release() } catch (_: Exception) {}
            }
        }
        if (opened == null) {
            released = true
            thread.quitSafely()
            throw failure ?: IllegalStateException("no H.264 encoder")
        }
        codec = opened.first
        inputSurface = opened.second
    }

    // The size within the encoder's limits. Not isSizeSupported(): that also wants the size aligned to what the encoder
    // prefers, which some report as 16 and 1080 isn't. A capability that can't be read doesn't stop anything.
    private fun fits(caps: MediaCodecInfo.VideoCapabilities, width: Int, height: Int): Boolean = try {
        caps.supportedWidths.contains(width) && caps.getSupportedHeightsFor(width).contains(height)
    } catch (_: IllegalArgumentException) {
        false  // getSupportedHeightsFor: a width it doesn't take at all
    } catch (_: Exception) {
        true
    }

    private fun format(width: Int, height: Int, fps: Int, bitrate: Int, extras: Set<Extra>): MediaFormat =
        MediaFormat.createVideoFormat(MIME, width, height).apply {
            setInteger(MediaFormat.KEY_COLOR_FORMAT, MediaCodecInfo.CodecCapabilities.COLOR_FormatSurface)
            setInteger(MediaFormat.KEY_BIT_RATE, bitrate.coerceIn(bitrateRange.lower, bitrateRange.upper))
            setInteger(MediaFormat.KEY_FRAME_RATE, fps)
            setInteger(MediaFormat.KEY_I_FRAME_INTERVAL, 1)
            setInteger(MediaFormat.KEY_BITRATE_MODE, MediaCodecInfo.EncoderCapabilities.BITRATE_MODE_VBR)
            if (Extra.REALTIME in extras) setInteger(MediaFormat.KEY_PRIORITY, 0)
            if (Extra.LOW_LATENCY in extras && Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
                setInteger(MediaFormat.KEY_LOW_LATENCY, 1)
            }
            // Baseline: no B-frames, so every packet can be shown as soon as it's decoded. Without it, ask for none.
            if (Extra.BASELINE in extras) {
                setInteger(MediaFormat.KEY_PROFILE, MediaCodecInfo.CodecProfileLevel.AVCProfileBaseline)
            } else if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                setInteger(MediaFormat.KEY_MAX_B_FRAMES, 0)
            }
        }

    fun requestKeyFrame() = setParam(MediaCodec.PARAMETER_KEY_REQUEST_SYNC_FRAME, 0)

    fun setBitrate(bps: Int) = setParam(MediaCodec.PARAMETER_KEY_VIDEO_BITRATE, bps.coerceIn(bitrateRange.lower, bitrateRange.upper))

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
