package com.telescope

import java.io.OutputStream
import java.net.DatagramPacket
import java.net.DatagramSocket
import java.net.InetSocketAddress
import java.net.Socket
import java.nio.ByteBuffer
import java.security.SecureRandom
import java.util.concurrent.ArrayBlockingQueue
import java.util.concurrent.TimeUnit
import javax.crypto.Cipher
import javax.crypto.spec.GCMParameterSpec
import javax.crypto.spec.SecretKeySpec

// The experimental UDP video's datagrams, shared with the desktop's udp_reader.py (see docs/protocol.md)
object UdpWire {
    const val MAX_PAYLOAD = 1100  // a fragment's bytes: with the headers and the tag a datagram stays under 1200
    const val HEADER = 9          // frame (u32), index (u16), count (u16), flags (u8)
    const val FLAG_KEY = 1
    const val TO_DESKTOP: Byte = 1
    const val TO_PHONE: Byte = 2
    const val NACK: Byte = 1      // + frame (u32) + indexes (u16 each); none means the whole frame
    const val KEYFRAME: Byte = 2
    const val HELLO: Byte = 3     // the desktop's keepalive, which also keeps its firewall open for the replies

    // One frame cut into datagram-sized plaintexts
    fun fragments(frame: Int, data: ByteArray, key: Boolean): List<ByteArray> {
        val count = maxOf(1, (data.size + MAX_PAYLOAD - 1) / MAX_PAYLOAD)
        require(count <= 0xFFFF) { "frame too big" }
        return (0 until count).map { i ->
            val from = i * MAX_PAYLOAD
            val to = minOf(data.size, from + MAX_PAYLOAD)
            ByteBuffer.allocate(HEADER + to - from).apply {
                putInt(frame); putShort(i.toShort()); putShort(count.toShort()); put((if (key) FLAG_KEY else 0).toByte())
                put(data, from, to - from)
            }.array()
        }
    }

    fun nonce(direction: Byte, counter: Long): ByteArray =
        ByteBuffer.allocate(12).put(direction).put(0).put(0).put(0).putLong(counter).array()
}

// AES-GCM for one direction: each datagram is its counter (8 bytes) then the sealed plaintext
class UdpSeal(key: ByteArray, private val direction: Byte) {
    private val spec = SecretKeySpec(key, "AES")
    private val cipher = Cipher.getInstance("AES/GCM/NoPadding")
    private var counter = 0L
    private var highest = -1L
    private val seen = java.util.BitSet(WINDOW)

    @Synchronized
    fun seal(plain: ByteArray): ByteArray {
        val n = counter++
        cipher.init(Cipher.ENCRYPT_MODE, spec, GCMParameterSpec(128, UdpWire.nonce(direction, n)))
        val sealed = cipher.doFinal(plain)
        return ByteBuffer.allocate(8 + sealed.size).putLong(n).put(sealed).array()
    }

    // The plaintext, or null for anything forged, damaged, replayed or far too old
    @Synchronized
    fun open(datagram: ByteArray, length: Int = datagram.size): ByteArray? {
        if (length < 8 + 16) return null
        val n = ByteBuffer.wrap(datagram, 0, 8).long
        if (n < 0 || n <= highest - WINDOW || (n <= highest && seen[(n % WINDOW).toInt()])) return null
        val plain = try {
            cipher.init(Cipher.DECRYPT_MODE, spec, GCMParameterSpec(128, UdpWire.nonce(direction, n)))
            cipher.doFinal(datagram, 8, length - 8)
        } catch (_: Exception) { return null }
        if (n > highest) {
            for (k in maxOf(highest + 1, n - WINDOW + 1)..n) seen.clear((k % WINDOW).toInt())
            highest = n
        }
        seen.set((n % WINDOW).toInt())
        return plain
    }

    companion object {
        const val WINDOW = 4096
        fun newKey(): ByteArray = ByteArray(32).also { SecureRandom().nextBytes(it) }
    }
}

// One desktop watching over UDP. Its HTTPS request stays open as the session: when that ends, so does this.
class UdpVideoClient(
    private val socket: Socket,
    peer: InetSocketAddress,
    private val stillAuthorized: () -> Boolean = { true },
    private val feedbackTimeoutMs: Long = FEEDBACK_TIMEOUT_MS,
    private val requestKeyFrame: () -> Unit,
) {
    private val key = UdpSeal.newKey()
    private val out = UdpSeal(key, UdpWire.TO_DESKTOP)
    private val back = UdpSeal(key, UdpWire.TO_PHONE)
    private val udp = DatagramSocket().apply { sendBufferSize = 1 shl 20 }
    @Volatile private var peer = peer
    @Volatile private var closed = false
    private val frames = ArrayBlockingQueue<Pair<ByteArray, Boolean>>(QUEUE)  // (frame, is a keyframe)
    private val sent = object : LinkedHashMap<Int, List<ByteArray>>() {
        override fun removeEldestEntry(eldest: MutableMap.MutableEntry<Int, List<ByteArray>>) = size > KEEP_FRAMES
    }
    @Volatile private var waitingForKey = true
    private var frameSeq = 0
    private var lastKeyAskMs = 0L
    @Volatile private var lastFeedbackMs = 0L
    private val openedAtMs = nowMs()

    fun hasActiveViewer(): Boolean = !closed && lastFeedbackMs != 0L && nowMs() - lastFeedbackMs < feedbackTimeoutMs

    private fun nowMs(): Long = System.nanoTime() / 1_000_000

    // The first line of the reply: what the desktop needs to listen and decrypt
    fun hello(): String =
        """{"key":"${java.util.Base64.getEncoder().encodeToString(key)}","port":${udp.localPort}}"""

    // From the encoder's thread: never blocks it; a backed-up queue drops frames and waits for a keyframe
    fun offer(frame: ByteArray, key: Boolean) {
        if (closed) return
        if (waitingForKey && !key) return
        if (!frames.offer(frame to key)) {
            frames.clear()
            waitingForKey = true
            requestKeyFrame()
            return
        }
        waitingForKey = false
    }

    // Blocks until the desktop goes: a sender thread, a feedback thread, and the HTTPS request kept alive here
    fun run(config: () -> ByteArray?) {
        val sender = Thread({ sendLoop(config) }, "udp-video-send").apply { start() }
        val feedback = Thread({ feedbackLoop() }, "udp-video-feedback").apply { start() }
        try {
            val stream: OutputStream = socket.getOutputStream()
            while (!closed && stillAuthorized() && nowMs() - maxOf(openedAtMs, lastFeedbackMs) < feedbackTimeoutMs) {
                stream.write('\n'.code)  // a desktop that went away shows up as a failed write
                stream.flush()
                Thread.sleep(minOf(KEEPALIVE_MS, feedbackTimeoutMs))
            }
        } catch (_: Exception) {
        } finally {
            close()
            sender.interrupt(); feedback.interrupt()
        }
    }

    private fun sendLoop(config: () -> ByteArray?) {
        try {
            while (!closed) {
                val (frame, key) = frames.poll(500, TimeUnit.MILLISECONDS) ?: continue
                // A keyframe carries the encoder's setup, so a desktop can start from any of them
                val data = if (key) (config() ?: ByteArray(0)) + frame else frame
                val seq = frameSeq++
                val parts = UdpWire.fragments(seq, H264Stream.withDelimiter(data), key)
                synchronized(sent) { sent[seq] = parts }
                parts.forEach { send(it) }
            }
        } catch (_: InterruptedException) {
        } catch (_: Exception) { close() }
    }

    private fun feedbackLoop() {
        val buf = ByteArray(2048)
        val packet = DatagramPacket(buf, buf.size)
        try {
            udp.soTimeout = 1000
            while (!closed) {
                packet.length = buf.size
                try { udp.receive(packet) } catch (_: java.net.SocketTimeoutException) { continue }
                val from = packet.socketAddress as? InetSocketAddress ?: continue
                if (from.address != peer.address) continue
                val plain = back.open(buf, packet.length) ?: continue
                lastFeedbackMs = nowMs()
                peer = from  // the desktop's port as its router sees it
                handle(plain)
            }
        } catch (_: Exception) {}
    }

    private fun handle(plain: ByteArray) {
        if (plain.isEmpty()) return
        when (plain[0]) {
            UdpWire.NACK -> {
                if (plain.size < 5) return
                val b = ByteBuffer.wrap(plain, 1, plain.size - 1)
                val seq = b.int
                val parts = synchronized(sent) { sent[seq] } ?: return
                if (!b.hasRemaining()) parts.forEach { send(it) }
                while (b.remaining() >= 2) parts.getOrNull(b.short.toInt() and 0xFFFF)?.let { send(it) }
            }
            UdpWire.KEYFRAME -> {
                val now = System.currentTimeMillis()
                if (now - lastKeyAskMs >= KEY_ASK_GAP_MS) { lastKeyAskMs = now; requestKeyFrame() }
            }
        }
    }

    private fun send(plain: ByteArray) {
        if (closed) return
        val sealed = out.seal(plain)
        try { udp.send(DatagramPacket(sealed, sealed.size, peer)) } catch (_: Exception) {}
    }

    fun close() {
        closed = true
        try { udp.close() } catch (_: Exception) {}
        try { socket.close() } catch (_: Exception) {}
    }

    companion object {
        const val QUEUE = 8              // frames waiting to go out; more and the link (or the phone) is stuck
        const val KEEP_FRAMES = 90       // about three seconds at 30 fps for a NACK to ask for again
        const val KEEPALIVE_MS = 1000L
        const val FEEDBACK_TIMEOUT_MS = 5_000L
        const val KEY_ASK_GAP_MS = 300L  // one keyframe per this, however many a lossy link asks for
    }
}
