package com.telescope

import java.io.OutputStream
import java.net.DatagramPacket
import java.net.DatagramSocket
import java.net.InetSocketAddress
import java.net.Socket
import java.nio.ByteBuffer
import java.security.SecureRandom
import java.util.concurrent.TimeUnit
import java.util.concurrent.locks.ReentrantLock
import kotlin.concurrent.withLock
import javax.crypto.Cipher
import javax.crypto.spec.GCMParameterSpec
import javax.crypto.spec.SecretKeySpec

// The experimental UDP video's datagrams, shared with the desktop's udp_reader.py (see docs/protocol.md)
object UdpWire {
    const val MAX_PAYLOAD = 1100  // a fragment's bytes: with the headers and the tag a datagram stays under 1200
    const val HEADER = 9          // frame (u32), index (u16), count (u16), flags (u8)
    const val FLAG_KEY = 1
    const val FLAGS_AT = 8        // where the flags are in a fragment (after the frame, index and count)
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
        const val PEAK_MS = 5_000L     // peakBps is the fastest the video came in over this long...
        const val PEAK_QUEUE_MS = 20L  // ...while every frame that came in meanwhile queued at least this long on the way
        const val PEAK_SPAN_MS = 250L  // over at least this long, so a report that came late doesn't read as a burst
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
    private var staleMs = 0L              // the longest a frame waited before the phone dropped it, unsent
    private var lastTakeMs = -1L
    private var lastReportMs = 0L
    private var batchExpected = 0L  // datagrams towards the next loss share (LOSS_DATAGRAMS)
    private var batchGot = 0L
    private val batches = ArrayDeque<LongArray>()  // (when, sent, got) per loss share, for lossLately
    private var batchLoss = 0.0  // the last loss share, which samples carry until the next one (or LATELY_MS)
    private var batchLossMs = Long.MIN_VALUE / 2
    private var peakBytes = 0L
    private var peakClockMs = 0L
    private var peakQueueMs = Long.MAX_VALUE  // the shortest a newly whole frame's trip took past the quickest, meanwhile
    private var tripFrame = -1
    private val peaks = ArrayDeque<DoubleArray>()  // (when, bps), falling: its first is the fastest lately

    /** Whether this desktop sends reports at all (an older one doesn't). */
    @Volatile var reporting = false
        private set

    /**
     * A frame's last datagram went out at nowMs. It queued since queuedMs and started going out at startedMs: only
     * that wait is a queue. The time from there to nowMs is the pacing and the sending itself, which isn't congestion.
     */
    @Synchronized
    fun sent(frame: Int, queuedMs: Long, nowMs: Long, startedMs: Long = nowMs) {
        sentAt[frame] = nowMs
        waitMs = minOf(waitMs, startedMs - queuedMs)
    }

    /** The phone dropped a frame unsent after it waited waitedMs: the link couldn't keep up, which counts as a queue that long. */
    @Synchronized
    fun stale(waitedMs: Long) { staleMs = maxOf(staleMs, waitedMs) }

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
            peakBytes += (r.bytes - prev.bytes) and U32
            peakClockMs += (r.clockMs - prev.clockMs) and U32
        }
        last = r
        reporting = true
        fresh = true
        lastReportMs = nowMs
        trip(r, nowMs)
        if (peakClockMs >= PEAK_SPAN_MS) {
            // Only a stretch whose frames all queued on the way went as fast as the link goes; otherwise it has room
            if (peakQueueMs != Long.MAX_VALUE && peakQueueMs >= PEAK_QUEUE_MS) {
                val bps = peakBytes * 8_000.0 / peakClockMs
                while (peaks.isNotEmpty() && peaks.last()[1] <= bps) peaks.removeLast()
                peaks.addLast(doubleArrayOf(nowMs.toDouble(), bps))
            }
            peakBytes = 0; peakClockMs = 0; peakQueueMs = Long.MAX_VALUE
        }
    }

    private fun trip(r: UdpReport, nowMs: Long) {
        val at = sentAt[r.frame] ?: return
        val trip = nowMs - at - r.sinceMs
        if (trip < 0) return
        while (trips.isNotEmpty() && trips.last()[1] >= trip) trips.removeLast()
        trips.addLast(longArrayOf(nowMs, trip))
        while (nowMs - trips.first()[0] > FLOOR_MS) trips.removeFirst()
        extraMs = minOf(extraMs, trip - trips.first()[1])
        if (r.frame != tripFrame) peakQueueMs = minOf(peakQueueMs, trip - trips.first()[1])  // a frame reported again says nothing new
        tripFrame = r.frame
    }

    /** The fastest video came in lately while it queued on the way: a burst that filled the link shows its speed. 0 = not known. */
    @Synchronized
    fun peakBps(nowMs: Long): Double {
        while (peaks.isNotEmpty() && nowMs - peaks.first()[0] > PEAK_MS) peaks.removeFirst()
        return peaks.firstOrNull()?.get(1) ?: 0.0
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
        if (batchExpected >= LOSS_DATAGRAMS) {
            batchLoss = (batchExpected - batchGot).coerceAtLeast(0).toDouble() / batchExpected
            batchLossMs = nowMs
            batches.addLast(longArrayOf(nowMs, batchExpected, batchGot))
            batchExpected = 0; batchGot = 0
        }
        // At low bitrates a share takes a few samples: those in between carry it rather than looking clear
        var loss = if (nowMs - batchLossMs <= LATELY_MS) batchLoss else 0.0
        while (batches.isNotEmpty() && nowMs - batches.first()[0] > LATELY_MS) batches.removeFirst()
        val lately = batches.sumOf { it[1] }.let { sentLately ->
            if (sentLately > 0) (sentLately - batches.sumOf { it[2] }).coerceAtLeast(0).toDouble() / sentLately else 0.0
        }
        val gave = gaveUp > 0
        if (gave) loss = maxOf(loss, GAVE_UP_LOSS)
        val wait = maxOf(if (waitMs == Long.MAX_VALUE) 0L else waitMs, waitingMs)
        val router = if (extraMs == Long.MAX_VALUE) 0L else extraMs
        val queue = maxOf(wait, router, staleMs)
        staleMs = 0
        expected = 0; got = 0; bytes = 0; clockMs = 0; gaveUp = 0; fresh = false
        extraMs = Long.MAX_VALUE; waitMs = Long.MAX_VALUE
        return if (first) null else Take(queue, sent, loss, router, if (gave) maxOf(lately, GAVE_UP_LOSS) else lately)  // the first one only starts the count
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

    /** [seal] into [out] (counter, then the sealed bytes) instead of a new array; the datagram's length, or -1 if [out] is too small. */
    @Synchronized
    fun sealInto(plain: ByteArray, out: ByteArray): Int {
        val size = 8 + plain.size + TAG
        if (out.size < size) return -1
        val n = counter++
        cipher.init(Cipher.ENCRYPT_MODE, spec, GCMParameterSpec(128, UdpWire.nonce(direction, n)))
        ByteBuffer.wrap(out, 0, 8).putLong(n)
        cipher.doFinal(plain, 0, plain.size, out, 8)
        return size
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
        const val TAG = 16
        fun newKey(): ByteArray = ByteArray(32).also { SecureRandom().nextBytes(it) }
    }
}

// UdpVideoClient's pacing on a schedule, so an oversleep (Android's timer slack) is caught up rather than lost; pure, a JVM test
class UdpPacer(private val burst: Int = UdpVideoClient.PACE_BURST) {
    private var nextNs = Long.MIN_VALUE  // when the next datagram is due at the pace
    private var wakeNs = Long.MIN_VALUE  // when the sleep it last asked for should end
    private var lastNs = 0L              // the last call or wake, so sends straight after a wake can be told from ones after idle
    private var overNs = 0L              // how far the last sleep overran, still to catch up

    /** How long to wait before sending bytes at bps; 0 or less is no pacing, and nothing builds up meanwhile. */
    fun waitNs(bytes: Int, bps: Int, nowNs: Long): Long {
        if (bps <= 0) { nextNs = Long.MIN_VALUE; wakeNs = Long.MIN_VALUE; overNs = 0; return 0 }
        val burstNs = (burst * 8e9 / bps).toLong()
        // An oversleep is caught up by the sends right after it, never by ones after idle
        if (overNs > 0 && (nowNs - lastNs > MIN_SLEEP_NS || nextNs >= nowNs - burstNs)) overNs = 0
        lastNs = nowNs
        val floor = nowNs - maxOf(burstNs, overNs)  // idle banks at most a burst
        if (nextNs < floor) nextNs = floor
        val wait = nextNs - nowNs
        nextNs += (bytes * 8e9 / bps).toLong()
        if (wait < MIN_SLEEP_NS) return 0  // short waits add up until one is worth a sleep
        wakeNs = nowNs + wait
        overNs = 0
        return wait
    }

    /** The sleep waitNs asked for ended at nowNs: how late it woke (Android's timer slack) is caught up next. */
    fun woke(nowNs: Long) {
        if (wakeNs == Long.MIN_VALUE) return
        overNs = (nowNs - wakeNs).coerceIn(0, CATCH_UP_NS)
        wakeNs = Long.MIN_VALUE
        lastNs = nowNs
    }

    companion object {
        const val MIN_SLEEP_NS = 1_000_000L
        const val CATCH_UP_NS = 100_000_000L
    }
}

// What the sender does next: a live frame, or a datagram the desktop asked for again
sealed class UdpJob {
    class Live(val frame: ByteArray, val key: Boolean, val atMs: Long, val pacedAtMs: Long = 0) : UdpJob()
    class Repair(val plain: ByteArray) : UdpJob()
}

/**
 * One desktop's backlog for the sender thread, pure so a JVM test can drive it with a clock.
 *
 * Live frames wait at most [maxAgeMs], not counting [paced] (the sender's own pacing sleeps, which a big keyframe
 * causes for the frames behind it): a frame older than that is never sent. A decoder can't skip single frames, so the
 * backlog jumps to a keyframe that is still young, or else is dropped whole and [onNeedKey] asks for a new one.
 * Datagrams the desktop asked for again wait beside them, at most [UdpVideoClient.REPAIR_TTL_MS] (it gives up on a frame
 * after about that), and go out at most [repairBps] so a whole-frame request can't crowd out live video.
 */
class UdpSendQueue(
    private val capacity: Int = UdpVideoClient.QUEUE,
    private val clock: () -> Long,
    private val onNeedKey: () -> Unit = {},
    private val onStale: (waitedMs: Long) -> Unit = {},  // frames dropped unsent, and how long the oldest had waited
    private val paced: () -> Long = { 0L },  // milliseconds the sender has slept on its pacing so far
) {
    private class Wanted(val id: Long, val plain: ByteArray, val key: Boolean, val expiresMs: Long)

    private val lock = ReentrantLock()
    private val ready = lock.newCondition()
    private val live = java.util.ArrayDeque<UdpJob.Live>()
    private val repairs = java.util.ArrayDeque<Wanted>()
    private val wanted = HashSet<Long>()
    private var waitingForKey = true
    private var cleared = false  // dropStale started over rather than jumping to a keyframe
    private var credit = 0.0
    private var creditMs = Long.MIN_VALUE

    @Volatile var maxAgeMs: Long = UdpVideoClient.MAX_AGE_MS
    @Volatile var repairBps: Int = UdpVideoClient.repairBpsFor(0)

    /** From the encoder's thread; true when it should be asked for a keyframe (the backlog was dropped). */
    fun offer(frame: ByteArray, key: Boolean): Boolean {
        var dropped = -1L
        val ask = lock.withLock {
            if (waitingForKey && !key) return@withLock false
            if (live.size >= capacity) {
                dropped = ageOf(live.peek()!!, clock())
                live.clear()
                waitingForKey = true
                if (!key) return@withLock true
            }
            live.add(UdpJob.Live(frame, key, clock(), paced()))
            waitingForKey = false
            ready.signalAll()
            false
        }
        if (dropped >= 0) onStale(dropped)
        return ask
    }

    /**
     * Queue one datagram of frame [seq] to go out again; false when it's already waiting or too many are. A keyframe's
     * datagrams ([key]) are exempt from the rate: without them the desktop gives up and asks for another keyframe.
     */
    fun offerRepair(seq: Int, index: Int, plain: ByteArray, key: Boolean = false): Boolean = lock.withLock {
        val id = (seq.toLong() shl 16) or index.toLong()
        if (repairs.size >= UdpVideoClient.REPAIR_QUEUE || !wanted.add(id)) return false
        repairs.add(Wanted(id, plain, key, clock() + UdpVideoClient.REPAIR_TTL_MS))
        ready.signalAll()
        true
    }

    /** The next thing to send, waiting up to [timeoutMs] for one; null when nothing is due. */
    fun poll(timeoutMs: Long): UdpJob? {
        var ask = false
        var dropped = -1L
        var waitNs = TimeUnit.MILLISECONDS.toNanos(timeoutMs)
        var job: UdpJob? = null
        lock.withLock {
            while (job == null) {
                val now = clock()
                val waited = dropStale(now)
                if (waited >= 0) { dropped = maxOf(dropped, waited); if (cleared) ask = true }
                dropExpired(now)
                refill(now)
                if (repairs.isNotEmpty() && (credit > 0 || repairs.peek()!!.key)) {
                    val r = repairs.remove()
                    wanted.remove(r.id)
                    if (!r.key) credit -= r.plain.size
                    job = UdpJob.Repair(r.plain)
                } else {
                    job = live.poll()
                }
                if (job != null || waitNs <= 0 || ask) break
                var step = waitNs
                if (repairs.isNotEmpty()) {  // out of credit: it comes back in a moment
                    val ms = (-credit * 8_000 / repairBps.coerceAtLeast(1)).toLong() + 1
                    step = minOf(step, TimeUnit.MILLISECONDS.toNanos(ms))
                }
                waitNs -= step - ready.awaitNanos(step)
            }
        }
        if (dropped >= 0) onStale(dropped)
        if (ask) onNeedKey()
        return job
    }

    /** How long the oldest waiting frame has waited, 0 with none. */
    fun oldestAgeMs(): Long = lock.withLock { live.peek()?.let { ageOf(it, clock()) } ?: 0L }

    // How long a frame has waited, less the time the sender slept on purpose meanwhile
    private fun ageOf(job: UdpJob.Live, now: Long): Long = maxOf(0L, now - job.atMs - (paced() - job.pacedAtMs))

    fun size(): Int = lock.withLock { live.size }

    // The oldest frame is past its age: go on from a young keyframe or start over (cleared); returns its wait, -1 if none dropped
    private fun dropStale(now: Long): Long {
        cleared = false
        val head = live.peek() ?: return -1
        val waited = ageOf(head, now)
        if (waited <= maxAgeMs) return -1
        val young = live.indexOfFirst { it.key && ageOf(it, now) <= maxAgeMs }
        if (young >= 0) {
            repeat(young) { live.remove() }
            return waited
        }
        live.clear()
        waitingForKey = true
        cleared = true
        return waited
    }

    private fun dropExpired(now: Long) {
        while (repairs.isNotEmpty() && repairs.peek()!!.expiresMs <= now) wanted.remove(repairs.remove().id)
    }

    private fun refill(now: Long) {
        credit = if (creditMs == Long.MIN_VALUE) UdpVideoClient.REPAIR_BURST.toDouble()
        else minOf(UdpVideoClient.REPAIR_BURST.toDouble(), credit + (now - creditMs) * repairBps / 8_000.0)
        creditMs = now
    }
}

// Sends a frame's datagrams on UdpPacer's schedule, and tells UdpLink how long the frame waited before it began. Pure
// apart from what it's given (a clock, a sleep, where datagrams go), so a JVM test can run it in virtual time.
class UdpTransmitter(
    private val link: UdpLink,
    private val clockNs: () -> Long,
    private val sleepNs: (Long) -> Unit,
    private val emit: (ByteArray) -> Unit,
    private val pacer: UdpPacer = UdpPacer(),
) {
    /** Bits per second Dynamic spreads datagrams out at (UdpVideoClient.paceFor), 0 for as fast as they go. */
    @Volatile var paceBps = 0

    @Volatile private var pacedNs = 0L  // the pacer's own sleeps, which are intended and not a queue

    /** Milliseconds slept on pacing so far, over all frames. */
    val pacedMs: Long get() = pacedNs / 1_000_000

    // Everything but the sleeps since the frame queued (pacedAtMs, from UdpJob.Live) counts as its wait: also a send that blocks
    fun frame(seq: Int, parts: List<ByteArray>, queuedMs: Long, pacedAtMs: Long) {
        for (part in parts) { pace(part.size); emit(part) }
        val nowMs = clockNs() / 1_000_000
        link.sent(seq, queuedMs, nowMs, startedMs = nowMs - (pacedMs - pacedAtMs))
    }

    // Beside the live frames, on the same schedule
    fun repair(plain: ByteArray) { pace(plain.size); emit(plain) }

    // A keyframe going out in one burst can overflow a router's buffer on a thin link, which then loses it
    private fun pace(bytes: Int) {
        val waitNs = pacer.waitNs(bytes, paceBps, clockNs())
        if (waitNs <= 0) return
        val before = clockNs()
        sleepNs(waitNs)
        val after = clockNs()
        pacedNs += after - before
        pacer.woke(after)
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
    private val queue = UdpSendQueue(clock = ::nowMs, onNeedKey = requestKeyFrame, onStale = { link.stale(it) }, paced = { transmitter.pacedMs })
    private val link = UdpLink()
    private val sendBuf = ByteArray(8 + UdpWire.HEADER + UdpWire.MAX_PAYLOAD + UdpSeal.TAG)
    private val outPacket = DatagramPacket(sendBuf, sendBuf.size)
    private var sendBuffer = 0  // what Dynamic last limited the socket's send buffer to; 0 = not limited
    private val transmitter = UdpTransmitter(link, System::nanoTime,
        { Thread.sleep(it / 1_000_000, (it % 1_000_000).toInt()) }, ::send)
    /** Bits per second Dynamic spreads datagrams out at (paceFor), 0 for as fast as they go. */
    var paceBps: Int
        get() = transmitter.paceBps
        set(value) { transmitter.paceBps = value; queue.repairBps = repairBpsFor(value) }
    private val sent = object : LinkedHashMap<Int, List<ByteArray>>() {
        override fun removeEldestEntry(eldest: MutableMap.MutableEntry<Int, List<ByteArray>>) = size > KEEP_FRAMES
    }
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
        if (queue.offer(frame, key)) requestKeyFrame()
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
                when (val job = queue.poll(500) ?: continue) {
                    is UdpJob.Live -> {
                        // A keyframe carries the encoder's setup, so a desktop can start from any of them
                        val data = if (job.key) (config() ?: ByteArray(0)) + job.frame else job.frame
                        val seq = frameSeq++
                        val parts = UdpWire.fragments(seq, H264Stream.withDelimiter(data), job.key)
                        synchronized(sent) { sent[seq] = parts }
                        transmitter.frame(seq, parts, job.atMs, job.pacedAtMs)
                    }
                    is UdpJob.Repair -> transmitter.repair(job.plain)
                }
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
                // Sent by the sender thread, paced and for a little while only: this thread has reports to read
                val key = parts.firstOrNull()?.let { it[UdpWire.FLAGS_AT].toInt() and UdpWire.FLAG_KEY != 0 } ?: false
                if (!b.hasRemaining()) parts.forEachIndexed { i, part -> queue.offerRepair(seq, i, part, key) }
                while (b.remaining() >= 2) {
                    val i = b.short.toInt() and 0xFFFF
                    parts.getOrNull(i)?.let { queue.offerRepair(seq, i, it, key) }
                }
            }
            UdpWire.REPORT -> UdpReport.parse(plain)?.let { link.report(it, nowMs()) }
            UdpWire.KEYFRAME -> {
                val now = System.currentTimeMillis()
                if (now - lastKeyAskMs >= KEY_ASK_GAP_MS) { lastKeyAskMs = now; requestKeyFrame() }
            }
        }
    }

    private fun send(plain: ByteArray) {
        if (closed) return
        // Only the sender thread gets here, so one buffer and packet do for every datagram
        val length = out.sealInto(plain, sendBuf)
        try {
            if (length >= 0) {
                outPacket.setData(sendBuf, 0, length)
                outPacket.socketAddress = peer
                udp.send(outPacket)
            } else {
                val sealed = out.seal(plain)
                udp.send(DatagramPacket(sealed, sealed.size, peer))
            }
        } catch (_: Exception) {}
    }

    /** Whether this desktop reports how its link does (an older one doesn't). */
    val reporting: Boolean get() = link.reporting

    /** How the link to this desktop did since the last call, for Dynamic; null while it sends no reports. */
    fun takeLink(): UdpLink.Take? = link.take(nowMs(), queue.oldestAgeMs())

    /** The fastest video reached this desktop lately while it queued on the way (UdpLink.peakBps), 0 when not known. */
    fun peakBps(): Double = link.peakBps(nowMs())

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
        // Once Dynamic found the wall, only a little faster than the link went (UdpLink.peakBps): 2.5 times a big
        // keyframe overflows a router that buffers 100 ms, and the loss then cuts the bitrate to about half the link
        const val PACE_OVER_LINK = 1.1
        const val PACE_FLOOR = 1.2       // yet always this much faster than the video
        // Dynamic's bitrate too: a still scene that starts moving spends its whole budget at once
        fun paceFor(encodedBps: Double, dynamicBps: Int = 0, peakBps: Double = 0.0): Int {
            val video = maxOf(encodedBps, dynamicBps.toDouble())
            val pace = maxOf(PACE_MIN_BPS.toDouble(), video * PACE)
            if (peakBps <= 0) return pace.toInt()
            return minOf(pace, maxOf(video * PACE_FLOOR, peakBps * PACE_OVER_LINK)).toInt()
        }
        const val QUEUE = 8              // frames waiting to go out; more and the link (or the phone) is stuck
        const val MAX_AGE_MS = 300L      // a frame that waited this long isn't sent: the picture would show old video
        // The desktop gives up 250 ms after it noticed a loss (GIVE_UP_S in udp.rs), and the NACK has spent a delay and a trip by now
        const val REPAIR_TTL_MS = 200L
        const val REPAIR_QUEUE = 512     // datagrams waiting to be sent again
        const val REPAIR_BURST = 32 * 1024
        // What may go out again each second: a share of the pace, so a whole-frame request can't crowd out live video
        fun repairBpsFor(paceBps: Int): Int = if (paceBps <= 0) 8_000_000 else maxOf(2_000_000, paceBps * 3 / 10)
        const val KEEP_FRAMES = 90       // about three seconds at 30 fps for a NACK to ask for again
        const val KEEPALIVE_MS = 1000L
        const val FEEDBACK_TIMEOUT_MS = 5_000L
        const val KEY_ASK_GAP_MS = 300L  // one keyframe per this, however many a lossy link asks for
    }
}
