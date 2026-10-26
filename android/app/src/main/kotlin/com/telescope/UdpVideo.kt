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
    const val REPORT: Byte = 4    // what got through, for Dynamic (UdpReport)

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

// The desktop's report on what got through. Its counts only ever grow (wrapping at 32 bits), so a lost report costs nothing.
data class UdpReport(
    val received: Long,  // datagrams it took
    val highest: Long,   // the highest datagram counter it saw, low 32 bits: received short of this were lost
    val bytes: Long,     // bytes of video it took
    val clockMs: Long,   // its own clock, for the rate those bytes came in at
    val frame: Int,      // the newest frame it has whole
    val sinceMs: Int,    // how long ago that frame became whole
    val gaveUp: Int,     // frames it gave up on (16 bits)
) {
    companion object {
        const val SIZE = 25
        private const val U32 = 0xFFFFFFFFL

        // A report, or null for anything else; longer ones are fine (fields added later go at the end)
        fun parse(plain: ByteArray): UdpReport? {
            if (plain.size < SIZE || plain[0] != UdpWire.REPORT) return null
            val b = ByteBuffer.wrap(plain, 1, SIZE - 1)
            return UdpReport(b.int.toLong() and U32, b.int.toLong() and U32, b.int.toLong() and U32, b.int.toLong() and U32,
                b.int, b.short.toInt() and 0xFFFF, b.short.toInt() and 0xFFFF)
        }
    }
}

// What one desktop's reports say about the UDP link (lost datagrams, slower trips), taken each link sample; pure, so a JVM test
class UdpLink {
    data class Take(val queueMs: Long, val sentBytes: Long, val loss: Double, val routerMs: Long = 0, val lossLately: Double = loss)

    companion object {
        const val FLOOR_MS = 10_000L  // a frame's usual trip is the quickest one over this long
        const val GAVE_UP_LOSS = 0.1  // a frame given up on counts as at least this much loss
        const val LOSS_DATAGRAMS = 50  // loss counts once this many were sent: at low bitrates one lost in 8 isn't 12%
        const val GAP_MS = 500L        // no report for this long: nothing gets through, like a stall over TCP
        const val LATELY_MS = 2_000L   // lossLately is over this long
        private const val U32 = 0xFFFFFFFFL
        private const val KEEP = 120  // frames whose send time is kept, for the reports about them
    }

    private val sentAt = object : LinkedHashMap<Int, Long>() {
        override fun removeEldestEntry(eldest: MutableMap.MutableEntry<Int, Long>) = size > KEEP
    }
    private var last: UdpReport? = null
    private val trips = ArrayDeque<LongArray>()  // (when, trip ms), rising: its first is the quickest lately
    // Since the last take
    private var expected = 0L
    private var got = 0L
    private var bytes = 0L
    private var clockMs = 0L
    private var gaveUp = 0
    private var fresh = false
    private var extraMs = Long.MAX_VALUE  // the shortest a trip took past the quickest one
    private var waitMs = Long.MAX_VALUE   // the shortest a frame waited on the phone
    private var lastTakeMs = -1L
    private var lastReportMs = 0L
    private var batchExpected = 0L  // datagrams towards the next loss share (LOSS_DATAGRAMS)
    private var batchGot = 0L
    private val batches = ArrayDeque<LongArray>()  // (when, sent, got) per loss share, for lossLately

    /** Whether this desktop sends reports at all (an older one doesn't). */
    @Volatile var reporting = false
        private set

    /** A frame's last datagram went out at nowMs, after waiting since queuedMs. */
    @Synchronized
    fun sent(frame: Int, queuedMs: Long, nowMs: Long) {
        sentAt[frame] = nowMs
        waitMs = minOf(waitMs, nowMs - queuedMs)
    }

    @Synchronized
    fun report(r: UdpReport, nowMs: Long) {
        val prev = last
        if (prev != null) {
            val more = (r.highest - prev.highest) and U32
            if (more > U32 / 2) return  // an older report that arrived late
            expected += more
            got += (r.received - prev.received) and U32
            bytes += (r.bytes - prev.bytes) and U32
            clockMs += (r.clockMs - prev.clockMs) and U32
            gaveUp += (r.gaveUp - prev.gaveUp) and 0xFFFF
        }
        last = r
        reporting = true
        fresh = true
        lastReportMs = nowMs
        val at = sentAt[r.frame] ?: return
        val trip = nowMs - at - r.sinceMs
        if (trip < 0) return
        while (trips.isNotEmpty() && trips.last()[1] >= trip) trips.removeLast()
        trips.addLast(longArrayOf(nowMs, trip))
        while (nowMs - trips.first()[0] > FLOOR_MS) trips.removeFirst()
        extraMs = minOf(extraMs, trip - trips.first()[1])
    }

    /** How the link did since the last take (waitingMs: the head of the phone's queue), or null with no new report. */
    @Synchronized
    fun take(nowMs: Long, waitingMs: Long): Take? {
        if (!fresh) {
            // No reports for a while: nothing got through, which DynamicBitrate treats as a stall like over TCP
            if (!reporting || lastTakeMs < 0 || nowMs - lastReportMs <= GAP_MS) return null
            lastTakeMs = nowMs
            return Take(nowMs - lastReportMs, 0, 0.0)
        }
        val span = nowMs - lastTakeMs
        val first = lastTakeMs < 0
        lastTakeMs = nowMs
        // The bytes came in over the desktop's clock; spread over this sample's span they're what got through
        val sent = if (clockMs > 0) bytes * span / clockMs else 0L
        batchExpected += expected
        batchGot += got
        var loss = 0.0
        if (batchExpected >= LOSS_DATAGRAMS) {
            loss = (batchExpected - batchGot).coerceAtLeast(0).toDouble() / batchExpected
            batches.addLast(longArrayOf(nowMs, batchExpected, batchGot))
            batchExpected = 0; batchGot = 0
        }
        while (batches.isNotEmpty() && nowMs - batches.first()[0] > LATELY_MS) batches.removeFirst()
        val lately = batches.sumOf { it[1] }.let { sentLately ->
            if (sentLately > 0) (sentLately - batches.sumOf { it[2] }).coerceAtLeast(0).toDouble() / sentLately else 0.0
        }
        if (gaveUp > 0) loss = maxOf(loss, GAVE_UP_LOSS)
        val wait = maxOf(if (waitMs == Long.MAX_VALUE) 0L else waitMs, waitingMs)
        val router = if (extraMs == Long.MAX_VALUE) 0L else extraMs
        val queue = maxOf(wait, router)
        expected = 0; got = 0; bytes = 0; clockMs = 0; gaveUp = 0; fresh = false
        extraMs = Long.MAX_VALUE; waitMs = Long.MAX_VALUE
        return if (first) null else Take(queue, sent, loss, router, if (gaveUp > 0) maxOf(lately, GAVE_UP_LOSS) else lately)  // the first one only starts the count
    }
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

// UdpVideoClient's pacing on a schedule, so an oversleep (Android's timer slack) is caught up rather than lost; pure, a JVM test
class UdpPacer(private val burst: Int = UdpVideoClient.PACE_BURST) {
    private var nextNs = Long.MIN_VALUE  // when the next datagram is due at the pace
    private var owed = false              // behind because it overslept, not because it was idle

    /** How long to wait before sending bytes at bps; 0 or less is no pacing, and nothing builds up meanwhile. */
    fun waitNs(bytes: Int, bps: Int, nowNs: Long): Long {
        if (bps <= 0) { nextNs = Long.MIN_VALUE; owed = false; return 0 }
        val burstNs = (burst * 8e9 / bps).toLong()
        if (owed && nextNs >= nowNs - burstNs) owed = false
        val floor = nowNs - if (owed) CATCH_UP_NS else burstNs  // idle banks at most a burst
        if (nextNs < floor) nextNs = floor
        val wait = nextNs - nowNs
        nextNs += (bytes * 8e9 / bps).toLong()
        if (wait < MIN_SLEEP_NS) return 0  // short waits add up until one is worth a sleep
        owed = true
        return wait
    }

    companion object {
        const val MIN_SLEEP_NS = 1_000_000L
        const val CATCH_UP_NS = 100_000_000L
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
    private val udp = DatagramSocket().apply { sendBufferSize = SEND_BUFFER }
    @Volatile private var peer = peer
    @Volatile private var closed = false
    private class Queued(val frame: ByteArray, val key: Boolean, val atMs: Long)
    private val frames = ArrayBlockingQueue<Queued>(QUEUE)
    private val link = UdpLink()
    private var sendBuffer = 0  // what Dynamic last limited the socket's send buffer to; 0 = not limited
    /** Bits per second Dynamic spreads datagrams out at (paceFor), 0 for as fast as they go. */
    @Volatile var paceBps = 0
    private val pacer = UdpPacer()
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
        if (!frames.offer(Queued(frame, key, nowMs()))) {
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
                val next = frames.poll(500, TimeUnit.MILLISECONDS) ?: continue
                // A keyframe carries the encoder's setup, so a desktop can start from any of them
                val data = if (next.key) (config() ?: ByteArray(0)) + next.frame else next.frame
                val seq = frameSeq++
                val parts = UdpWire.fragments(seq, H264Stream.withDelimiter(data), next.key)
                synchronized(sent) { sent[seq] = parts }
                parts.forEach { pace(it.size); send(it) }
                link.sent(seq, next.atMs, nowMs())
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
            UdpWire.REPORT -> UdpReport.parse(plain)?.let { link.report(it, nowMs()) }
            UdpWire.KEYFRAME -> {
                val now = System.currentTimeMillis()
                if (now - lastKeyAskMs >= KEY_ASK_GAP_MS) { lastKeyAskMs = now; requestKeyFrame() }
            }
        }
    }

    // A keyframe going out in one burst can overflow a router's buffer on a thin link, which then loses it
    private fun pace(bytes: Int) {
        val waitNs = pacer.waitNs(bytes, paceBps, System.nanoTime())
        if (waitNs > 0) Thread.sleep(waitNs / 1_000_000, (waitNs % 1_000_000).toInt())
    }

    private fun send(plain: ByteArray) {
        if (closed) return
        val sealed = out.seal(plain)
        try { udp.send(DatagramPacket(sealed, sealed.size, peer)) } catch (_: Exception) {}
    }

    /** Whether this desktop reports how its link does (an older one doesn't). */
    val reporting: Boolean get() = link.reporting

    /** How the link to this desktop did since the last call, for Dynamic; null while it sends no reports. */
    fun takeLink(): UdpLink.Take? = link.take(nowMs(), frames.peek()?.let { nowMs() - it.atMs } ?: 0L)

    /** Dynamic keeps the send buffer short (bytes, 0 = the default) like H264Client's, so a full link shows as a queue. */
    fun limitBuffer(bytes: Int) {
        if (bytes == sendBuffer || (bytes > 0 && sendBuffer > 0 && bytes in sendBuffer * 4 / 5..sendBuffer * 5 / 4)) return
        sendBuffer = bytes
        try { udp.sendBufferSize = if (bytes > 0) bytes else SEND_BUFFER } catch (_: Exception) {}
    }

    fun close() {
        closed = true
        try { udp.close() } catch (_: Exception) {}
        try { socket.close() } catch (_: Exception) {}
    }

    companion object {
        const val SEND_BUFFER = 1 shl 20
        const val PACE = 2.5             // Dynamic sends this many times as fast as the video comes...
        const val PACE_MIN_BPS = 2_000_000  // ...but never slower than this
        const val PACE_BURST = 16 * 1024 // and lets this much go at once
        // Dynamic's bitrate too: a still scene that starts moving spends its whole budget at once
        fun paceFor(encodedBps: Double, dynamicBps: Int = 0): Int =
            maxOf(PACE_MIN_BPS.toDouble(), maxOf(encodedBps, dynamicBps.toDouble()) * PACE).toInt()
        const val QUEUE = 8              // frames waiting to go out; more and the link (or the phone) is stuck
        const val KEEP_FRAMES = 90       // about three seconds at 30 fps for a NACK to ask for again
        const val KEEPALIVE_MS = 1000L
        const val FEEDBACK_TIMEOUT_MS = 5_000L
        const val KEY_ASK_GAP_MS = 300L  // one keyframe per this, however many a lossy link asks for
    }
}
