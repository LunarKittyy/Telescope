package com.telescope

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import java.nio.ByteBuffer

// How old the video on the phone's UDP path can get: its queue, repairs and what Dynamic reads from it
class UdpLatencyTest {

    @Test
    fun `a stall asks for a keyframe at once, not after the poll times out`() {
        var now = 0L
        var asks = 0
        val q = UdpSendQueue(clock = { now }, onNeedKey = { asks++ })
        q.maxAgeMs = 100
        q.offer(ByteArray(10), true)
        now = 1_000
        val started = System.nanoTime()
        val job = q.poll(2_000)
        val tookMs = (System.nanoTime() - started) / 1_000_000
        assertNull(job)
        assertEquals(1, asks)
        assertTrue(tookMs < 1_000) { "poll waited $tookMs ms before asking" }
    }

    // ---- the queue's age

    private class Drained(val maxAgeMs: Long, val firstAfterStallIsKey: Boolean, val keyAsks: Int, val sent: Int)

    // A frame every periodMs with a keyframe every `keyEvery`, a sender that takes nothing for stallMs and then everything
    private fun drainAfterStall(maxAgeMs: Long, periodMs: Int, keyEvery: Int, stallMs: Long): Drained {
        var now = 0L
        var asks = 0
        val q = UdpSendQueue(clock = { now }, onNeedKey = { asks++ })
        q.maxAgeMs = maxAgeMs
        var wantKey = false
        var frame = 0
        var maxAge = 0L
        var sent = 0
        var first: Boolean? = null
        while (now < stallMs + 1_500) {
            if (now % periodMs == 0L) {
                val key = wantKey || frame % keyEvery == 0
                if (q.offer(ByteArray(10), key)) wantKey = true else if (key) wantKey = false
                frame++
            }
            if (now >= stallMs) {
                while (true) {
                    val job = q.poll(0) as? UdpJob.Live ?: break
                    if (first == null) first = job.key
                    maxAge = maxOf(maxAge, now - job.atMs)
                    sent++
                }
            }
            now++
        }
        return Drained(maxAge, first == true, asks, sent)
    }

    @Test
    fun `a frame older than the budget is never sent, and the picture starts again at a keyframe`() {
        // 15 fps: eight frames fit before the queue is full, which is more than half a second of video
        val old = drainAfterStall(Long.MAX_VALUE, 66, 15, 450)  // what the count limit alone did
        val now = drainAfterStall(UdpVideoClient.MAX_AGE_MS, 66, 15, 450)
        println("udp sender stalled 450 ms at 15 fps: oldest frame sent %d ms old before, %d ms now; frames sent %d -> %d; keyframe asks %d -> %d"
            .format(old.maxAgeMs, now.maxAgeMs, old.sent, now.sent, old.keyAsks, now.keyAsks))
        assertTrue(old.maxAgeMs >= 400)
        assertTrue(now.maxAgeMs <= UdpVideoClient.MAX_AGE_MS, "oldest frame sent was ${now.maxAgeMs} ms old")
        assertTrue(now.firstAfterStallIsKey)
        assertTrue(now.keyAsks >= 1)
    }

    @Test
    fun `a stale backlog goes on from a keyframe that is still young instead of starting over`() {
        var now = 0L
        var asks = 0
        val q = UdpSendQueue(clock = { now }, onNeedKey = { asks++ })
        q.offer(byteArrayOf(1), key = true)
        now = 100; q.offer(byteArrayOf(2), key = false)
        now = 200; q.offer(byteArrayOf(3), key = false)
        now = 250; q.offer(byteArrayOf(4), key = true)
        now = 300; q.offer(byteArrayOf(5), key = false)
        now = 420  // the first is 420 ms old, the second keyframe 170
        val a = q.poll(0) as UdpJob.Live
        assertEquals(4, a.frame[0].toInt())
        assertEquals(5, (q.poll(0) as UdpJob.Live).frame[0].toInt())
        assertEquals(0, asks)
    }

    @Test
    fun `a full queue still drops everything and a keyframe offered then starts afresh`() {
        var now = 0L
        val q = UdpSendQueue(capacity = 3, clock = { now })
        q.offer(byteArrayOf(1), key = true)
        q.offer(byteArrayOf(2), key = false)
        q.offer(byteArrayOf(3), key = false)
        assertTrue(q.offer(byteArrayOf(4), key = false))
        assertEquals(0, q.size())
        assertFalse(q.offer(byteArrayOf(5), key = false))
        assertFalse(q.offer(byteArrayOf(6), key = true))
        assertEquals(6, (q.poll(0) as UdpJob.Live).frame[0].toInt())
    }

    // ---- repairs

    @Test
    fun `repairs are spread over time, expire, and don't hold up a live frame`() {
        var now = 0L
        val q = UdpSendQueue(clock = { now })
        q.repairBps = UdpVideoClient.repairBpsFor(8_000_000)  // 2.4 Mbps
        q.offer(ByteArray(10), key = true)
        q.poll(0)
        val whole = (0 until 1000).map { ByteArray(1100) }  // a 1 MB keyframe asked for in full
        whole.forEachIndexed { i, p -> q.offerRepair(1, i, p) }
        assertFalse(q.offerRepair(1, 0, whole[0]), "the same datagram asked for twice waits once")
        var repaired = 0
        var bytesAt100 = 0
        var liveWaited = -1L
        while (now < 600) {
            if (now == 40L) q.offer(ByteArray(10), key = true)
            while (true) {
                when (val job = q.poll(0)) {
                    is UdpJob.Repair -> repaired++
                    is UdpJob.Live -> liveWaited = now - job.atMs
                    null -> break
                }
            }
            if (now == 100L) bytesAt100 = repaired * 1100
            now++
        }
        println("whole 1 MB frame asked for again: sent %d KB in the first 100 ms and %d KB in all (the rest expired); old code sent 1000 KB at once; live frame waited %d ms"
            .format(bytesAt100 / 1000, repaired * 1100 / 1000, liveWaited))
        assertTrue(bytesAt100 <= UdpVideoClient.REPAIR_BURST + 2_400_000 / 8 / 10 + 1100, "$bytesAt100 bytes in 100 ms")
        assertTrue(repaired < 400, "$repaired datagrams in all")
        assertTrue(liveWaited in 0..1, "the live frame waited $liveWaited ms")
    }

    @Test
    fun `a keyframe's repairs are not held to the rate, so they all go out before they expire`() {
        fun sentIn(key: Boolean): Int {
            var now = 0L
            val q = UdpSendQueue(clock = { now })
            q.repairBps = UdpVideoClient.repairBpsFor(2_000_000)  // the slowest Dynamic paces at: 2 Mbps
            repeat(140) { q.offerRepair(1, it, ByteArray(1100), key) }  // a 150 KB keyframe asked for in full
            var n = 0
            while (now < UdpVideoClient.REPAIR_TTL_MS) { while (q.poll(0) is UdpJob.Repair) n++; now++ }
            return n
        }
        val held = sentIn(false)
        val exempt = sentIn(true)
        println("150 KB keyframe asked for again on a thin link: %d of 140 datagrams out before they expire with the quota, %d without".format(held, exempt))
        assertTrue(held < 140)
        assertEquals(140, exempt)
    }

    @Test
    fun `a repair asked for again after it went out is sent again`() {
        var now = 0L
        val q = UdpSendQueue(clock = { now })
        assertTrue(q.offerRepair(5, 2, byteArrayOf(1)))
        assertTrue(q.poll(0) is UdpJob.Repair)
        assertTrue(q.offerRepair(5, 2, byteArrayOf(1)))
        now = UdpVideoClient.REPAIR_TTL_MS + 1
        assertEquals(null, q.poll(0))  // too late: the desktop gave up on it
        assertTrue(q.offerRepair(5, 2, byteArrayOf(1)))
    }

    // On the real client over loopback: a whole-frame request used to go out at once from the thread reading feedback
    @Test
    fun `a whole-frame resend request goes out at a measured pace`() {
        val lo = java.net.InetAddress.getLoopbackAddress()
        val desktop = java.net.DatagramSocket(0, lo).apply { receiveBufferSize = 8 shl 20; soTimeout = 50 }
        val server = java.net.ServerSocket(0, 1, lo)
        val session = java.net.Socket(lo, server.localPort)
        val client = UdpVideoClient(server.accept(), java.net.InetSocketAddress(lo, desktop.localPort), requestKeyFrame = {})
        val hello = client.hello()
        val key = java.util.Base64.getDecoder().decode(Regex("\"key\":\"([^\"]+)\"").find(hello)!!.groupValues[1])
        val phonePort = Regex("\"port\":(\\d+)").find(hello)!!.groupValues[1].toInt()
        val runner = Thread { client.run { null } }.apply { start() }
        val back = UdpSeal(key, UdpWire.TO_PHONE)
        fun tell(plain: ByteArray) {
            val sealed = back.seal(plain)
            desktop.send(java.net.DatagramPacket(sealed, sealed.size, lo, phonePort))
        }
        fun drain(ms: Long): Int {
            var n = 0
            val end = System.nanoTime() + ms * 1_000_000
            val buf = ByteArray(2048)
            while (System.nanoTime() < end) {
                try { desktop.receive(java.net.DatagramPacket(buf, buf.size)); n++ } catch (_: java.net.SocketTimeoutException) {}
            }
            return n
        }
        try {
            tell(byteArrayOf(UdpWire.HELLO))
            client.offer(ByteArray(10), key = true)
            client.offer(ByteArray(600_000) { 7 }, key = false)  // a keyframe's repairs aren't paced, so a plain frame
            val parts = drain(700)
            assertTrue(parts >= 540, "the frame itself: $parts datagrams")
            tell(ByteBuffer.allocate(5).put(UdpWire.NACK).putInt(1).array())  // all of frame 1
            val early = drain(100)
            val total = early + drain(500)
            println("whole-frame NACK for %d datagrams: %d arrived in the first 100 ms, %d in all".format(parts, early, total))
            assertTrue(early < parts / 2, "$early of $parts datagrams within 100 ms")
            assertTrue(total < parts, "$total of $parts datagrams in all")
        } finally {
            client.close(); session.close(); desktop.close(); server.close()
            runner.join(2000)
        }
    }

    // ---- pacing isn't congestion

    private class Climb(val bitrate: Int, val ceiling: Int, val maxPressureMs: Long, val atCeilingMs: Long?, val lateMaxBitrate: Int, val path: List<Int> = emptyList())

    // The real queue, transmitter, pacer, link and Dynamic against a desktop that gets every datagram the moment it
    // goes out, in virtual time. Frames use the whole budget, and pacing is the 1.2 times the video that Dynamic
    // settles on once it has found where the link fills up. Nothing in the path is congested, so Dynamic must climb.
    // [blockedAtBps]: each send blocks as long as its bytes take at that rate; [still]: small frames, a keyframe of 0.25 s of video
    private fun zeroDelay(width: Int, height: Int, fps: Int, seconds: Int, blockedAtBps: Int = 0, still: Boolean = false, startBps: Int = 0, keySeconds: Double = 0.25): Climb {
        val ceiling = H264Stream.dynamicCeiling(width, height, fps)
        val dynamic = DynamicBitrate(if (startBps > 0) startBps else ceiling / 2, ceiling)
        var bitrate = dynamic.bitrate
        val link = UdpLink()
        var nowNs = 0L
        val nowMs = { nowNs / 1_000_000 }
        var wantKey = true
        lateinit var transmitter: UdpTransmitter
        val queue = UdpSendQueue(clock = nowMs, onNeedKey = { wantKey = true }, onStale = { link.stale(it) }, paced = { transmitter.pacedMs })
        var advance: (Long) -> Unit = {}

        // The desktop
        var counter = 0L
        var bytes = 0L
        var whole = -1
        var wholeAtMs = 0L
        val seen = HashMap<Int, Int>()
        val emit = { part: ByteArray ->
            if (blockedAtBps > 0) advance(nowNs + part.size * 8_000_000_000L / blockedAtBps)
            val b = ByteBuffer.wrap(part)
            val frame = b.int; b.short; val count = b.short.toInt()
            counter++
            bytes += part.size
            val n = (seen[frame] ?: 0) + 1
            seen[frame] = n
            if (n == count) { whole = frame; wholeAtMs = nowMs(); seen.remove(frame) }
        }

        var frameNo = 0
        var seq = 0
        var encodedBytes = 0L
        var rateBytes = 0L
        var encodedBps = 0.0
        var maxPressure = 0L
        var atCeiling: Long? = null
        var lateMax = 0
        val path = ArrayList<Int>()
        val periodNs = 1_000_000_000L / fps
        var nextFrame = 0L
        var nextReport = 100_000_000L
        var nextSample = 250_000_000L
        var lastSample = 0L

        fun advanceTo(target: Long) {
            while (true) {
                val next = minOf(nextFrame, nextReport, nextSample)
                if (next > target) break
                nowNs = next
                if (next == nextFrame) {
                    val key = wantKey || frameNo % fps == 0
                    val size = if (!still) bitrate / 8 / fps else if (key) (bitrate / 8 * keySeconds).toInt() else bitrate / 8 / fps / 5
                    if (queue.offer(ByteArray(size), key)) wantKey = true else if (key) wantKey = false
                    frameNo++
                    encodedBytes += size
                    rateBytes += size
                    nextFrame += periodNs
                }
                if (next == nextReport) {
                    val r = UdpReport(counter, counter - 1, bytes, nowMs(), whole, (nowMs() - wholeAtMs).toInt(), 0)
                    link.report(r, nowMs())
                    nextReport += 100_000_000L
                }
                if (next == nextSample) {
                    val ms = nowMs()
                    encodedBps = rateBytes * 8_000.0 / (ms - lastSample).coerceAtLeast(1)
                    rateBytes = 0
                    lastSample = ms
                    transmitter.paceBps = (bitrate * UdpVideoClient.PACE_FLOOR).toInt()
                    link.take(ms, queue.oldestAgeMs())?.let { take ->
                        val sample = DynamicBitrate.Sample(ms, take.queueMs, take.sentBytes, encodedBytes, take.loss, take.routerMs, take.lossLately)
                        if (ms > 2_000) maxPressure = maxOf(maxPressure, DynamicBitrate.pressureMs(sample))
                        encodedBytes = 0
                        dynamic.update(sample)?.let { bitrate = it }
                        if (atCeiling == null && bitrate >= ceiling) atCeiling = ms
                        if (ms % 5_000L < 250) path.add(bitrate)
                        if (ms > seconds * 500L) lateMax = maxOf(lateMax, bitrate)
                    }
                    nextSample += 250_000_000L
                }
            }
            nowNs = maxOf(nowNs, target)
        }

        advance = ::advanceTo
        transmitter = UdpTransmitter(link, { nowNs }, { advanceTo(nowNs + it) }, emit)
        transmitter.paceBps = (bitrate * UdpVideoClient.PACE_FLOOR).toInt()
        while (nowNs < seconds * 1_000_000_000L) {
            val job = queue.poll(0)
            if (job is UdpJob.Live) {
                val parts = UdpWire.fragments(seq, H264Stream.withDelimiter(job.frame), job.key)
                transmitter.frame(seq++, parts, job.atMs, job.pacedAtMs)
            } else {
                advanceTo(minOf(nextFrame, nextReport, nextSample))
            }
        }
        return Climb(bitrate, ceiling, maxPressure, atCeiling, lateMax, path)
    }

    @Test
    fun `a socket that blocks on every keyframe makes Dynamic cut, though the frames behind it are dropped unsent`() {
        // 5 Mbps into a socket that takes 4: each keyframe blocks 0.3 s and the frames behind it age out unsent
        val r = zeroDelay(1920, 1080, 30, 60, blockedAtBps = 4_000_000, still = true, startBps = 5_000_000)
        println("keyframes blocking a 4 Mbps socket, video starting at 5.0 Mbps: ends at %.1f Mbps, never above %.1f Mbps in the second half; queue read up to %d ms"
            .format(r.bitrate / 1e6, r.lateMaxBitrate / 1e6, r.maxPressureMs))
        assertTrue(r.lateMaxBitrate <= 4_000_000, "kept up to ${r.lateMaxBitrate}")
    }

    @Test
    fun `a big keyframe paced out doesn't read as congestion or collapse Dynamic`() {
        // A still scene on an unconstrained link, its keyframe 0.4 s of video, paced at 1.2 times the video: it takes
        // 0.33 s to go out and the frames behind it wait that long on purpose
        for (start in listOf(5_000_000, 10_000_000)) {
            val r = zeroDelay(1920, 1080, 30, 60, still = true, startBps = start, keySeconds = 0.4)
            println("0.4 s keyframes paced at 1.2x, video starting at %d Mbps: %s Mbps every 5 s; queue read up to %d ms"
                .format(start / 1_000_000, r.path.joinToString(" ") { "%.1f".format(it / 1e6) }, r.maxPressureMs))
            assertTrue(r.bitrate >= start * 0.9, "ended at ${r.bitrate}")
            assertTrue(r.maxPressureMs <= DynamicBitrate.CLEAR_MS, "queue read ${r.maxPressureMs} ms")
        }
    }

    @Test
    fun `frames waiting out the sender's own pacing aren't stale`() {
        var now = 0L
        var slept = 0L
        var asks = 0
        val stale = ArrayList<Long>()
        val q = UdpSendQueue(clock = { now }, onNeedKey = { asks++ }, onStale = { stale.add(it) }, paced = { slept })
        q.offer(byteArrayOf(1), key = true)
        q.poll(0)  // the keyframe goes out, paced
        now = 33; q.offer(byteArrayOf(2), key = false)
        now = 66; q.offer(byteArrayOf(3), key = false)
        now = 450; slept = 400  // 0.4 s of it was pacing
        assertEquals(17, q.oldestAgeMs())
        assertEquals(2, (q.poll(0) as UdpJob.Live).frame[0].toInt())
        assertEquals(0, asks)
        assertTrue(stale.isEmpty())
        now = 900  // now it waits on something else for half a second
        assertEquals(null, q.poll(0))
        assertEquals(1, asks)
        assertTrue(stale.single() >= UdpVideoClient.MAX_AGE_MS)
    }

    @Test
    fun `an unconstrained link at zero delay never reads as queued and Dynamic climbs to its ceiling`() {
        for ((w, h, fps) in listOf(Triple(1920, 1080, 30), Triple(3840, 2160, 30), Triple(3840, 2160, 15))) {
            val r = zeroDelay(w, h, fps, 40)
            println("zero delay %dx%d at %d fps: reported queue at most %d ms, bitrate %.1f of %.1f Mbps, ceiling reached at %s"
                .format(w, h, fps, r.maxPressureMs, r.bitrate / 1e6, r.ceiling / 1e6, r.atCeilingMs?.let { "$it ms" } ?: "never"))
            assertTrue(r.maxPressureMs <= DynamicBitrate.CLEAR_MS, "${w}x$h at $fps fps read a queue of ${r.maxPressureMs} ms")
            assertEquals(r.ceiling, r.bitrate, "${w}x$h at $fps fps stopped at ${r.bitrate}")
        }
    }
}
