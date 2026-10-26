package com.telescope

import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import kotlin.random.Random

// Dynamic over UDP (the real UdpLink and DynamicBitrate) against a simulated router, phone and desktop, compared with Auto
class DynamicUdpSimTest {

    private val w = 1920
    private val h = 1080
    private val fps = 30
    private val auto = H264Stream.defaultBitrate(w, h, fps)
    private val top = H264Stream.dynamicCeiling(w, h, fps)

    /** The path to the desktop at a given ms: the phone's Wi-Fi, the router's speed (Mbps), its buffer, random loss. */
    data class Path(val wifiMbps: Double, val routerMbps: Double, val bufferMs: Int = 100, val loss: Double = 0.0,
                    val delayMs: Int = 3)

    private fun interface Link { fun at(ms: Long): Path }

    data class Result(
        val name: String,
        val meanMbps: Double,   // video that reached the desktop
        val p50Ms: Long,        // encoded to shown
        val p95Ms: Long,
        val maxMs: Long,
        val lossPct: Double,    // datagrams lost on the way
        val gaveUp: Int,        // frames the desktop gave up on: a freeze until the next keyframe
        val frozenPct: Double,  // of the time, frames that never showed
        val bitrates: List<Int>,
        val gaveUpAt: List<Long> = emptyList(),
        val lossPerSecond: List<Double> = emptyList(),
        val skips: Int = 0,         // the phone's queue overflowed and dropped what was waiting
    ) {
        override fun toString() = "%-22s %5.1f Mbps  p50 %4d ms  p95 %5d ms  max %5d ms  lost %4.1f%%  gave up %3d  frozen %4.1f%%".format(
            name, meanMbps, p50Ms, p95Ms, maxMs, lossPct, gaveUp, frozenPct) + "  skips $skips"
    }

    private class Dgram(val frame: Int, val idx: Int, val count: Int, val size: Int, var counter: Long = 0, var at: Long = 0)

    private class DFrame(var count: Int = 0) {
        val parts = HashSet<Int>()
        var noticed = -1L
        var asked = Long.MIN_VALUE / 2
        var progress = 0
        fun complete() = count > 0 && parts.size == count
    }

    private fun simulate(name: String, dynamic: Boolean, link: Link, seconds: Int, seed: Int = 1,
                         usage: (Long) -> Double = { 0.9 }, startBps: Int = auto, keyShare: Double = 5.0 / 30,
                         linkPace: Boolean = true): Result {
        val rnd = Random(seed)
        var now = 0L
        val controller = if (dynamic) DynamicBitrate(startBps, top) else null
        var bitrate = controller?.bitrate ?: auto
        val udpLink = UdpLink()
        val piece = UdpWire.MAX_PAYLOAD
        val overhead = UdpWire.HEADER + 8 + 16 + 28  // header, counter, tag, IP and UDP

        // Phone
        class Queued(val seq: Int, val size: Int, val key: Boolean, val atMs: Long)
        val frames = ArrayDeque<Queued>()
        var frameNo = 0
        var seq = 0
        var wantKey = true
        var waitingForKey = true
        var lastKeyAsk = Long.MIN_VALUE / 2
        val encodedAt = HashMap<Int, Long>()  // sent frame number -> encoded at
        val counts = HashMap<Int, Int>()      // sent frame number -> pieces
        val keyframes = HashSet<Int>()        // sent frame numbers that are keyframes
        val sndbuf = ArrayDeque<Dgram>()
        var sndbufBytes = 0L
        var sndbufCap = if (dynamic) MjpegServer.dynamicSendBuffer(bitrate.toDouble()) * 2 else UdpVideoClient.SEND_BUFFER
        var sending: ArrayDeque<Dgram>? = null
        var sendingFrame: Queued? = null
        var counter = 0L
        var wifiCredit = 0.0
        var paceCredit = 0.0
        var paceBps = 0
        var encoded = 0L
        var rateBytes = 0L  // for encodedBps, reset every sample like MjpegServer's
        var encodedBps = 0.0

        // Router and the air between
        val router = ArrayDeque<Dgram>()
        var routerBytes = 0L
        var routerCredit = 0.0
        val toDesktop = ArrayDeque<Dgram>()  // in flight, by arrival
        val toPhone = ArrayDeque<Pair<Long, () -> Unit>>()

        // Desktop
        val dframes = HashMap<Int, DFrame>()
        var next: Int? = null
        var newest = -1
        var received = 0L
        var highest = -1L
        var bytes = 0L
        var whole = -1
        var wholeAt = 0L
        var gaveUp = 0
        var lastReport = 0L
        val latencies = ArrayList<Long>()
        var shown = 0
        var lostOnWay = 0L
        var sentDgrams = 0L
        val perSecond = ArrayList<Int>()
        val gaveUpAt = ArrayList<Long>()
        var skips = 0
        val lossPerSecond = ArrayList<Double>()
        var lostMark = 0L
        var sentMark = 0L

        fun toPhoneLater(action: () -> Unit) {
            val d = link.at(now)
            if (rnd.nextDouble() >= d.loss) toPhone.addLast(now + d.delayMs * 2 to action)
        }
        fun resend(f: Int, idx: List<Int>) {
            val count = counts[f] ?: return
            for (i in (idx.ifEmpty { (0 until count).toList() })) {
                val size = if (i < count - 1) piece else 200
                // Resends go out from the feedback thread through the same short send buffer; a full one holds them back
                if (sndbufBytes + size + overhead > sndbufCap) return
                sndbuf.addLast(Dgram(f, i, count, size + overhead)); sndbufBytes += size + overhead
            }
        }

        while (now < seconds * 1000L) {
            val path = link.at(now)
            // Encoder
            if (now * fps >= frameNo * 1000L) {
                val key = wantKey || frameNo % fps == 0
                wantKey = false
                val perFrame = bitrate / 8.0 / fps * usage(now)
                val size = (perFrame * (if (key) keyShare * fps else (1 - keyShare) * fps / (fps - 1)) * (0.8 + 0.4 * rnd.nextDouble())).toInt()
                encoded += size
                rateBytes += size
                frameNo++
                if (!(waitingForKey && !key)) {
                    if (frames.size >= UdpVideoClient.QUEUE) { frames.clear(); waitingForKey = true; wantKey = true; skips++ }
                    else { frames.addLast(Queued(-1, size, key, now)); waitingForKey = false }
                }
            }
            // Sender: a frame's pieces into the send buffer as it has room, like a blocking send
            if (sending == null && frames.isNotEmpty()) {
                val q = frames.removeFirst()
                val s = seq++
                val count = maxOf(1, (q.size + piece - 1) / piece)
                encodedAt[s] = q.atMs
                counts[s] = count
                if (q.key) keyframes.add(s)
                sending = ArrayDeque((0 until count).map { i -> Dgram(s, i, count, (if (i < count - 1) piece else q.size - piece * (count - 1)).coerceAtLeast(1) + overhead) })
                sendingFrame = q
            }
            // Paced into the send buffer as it has room, like a blocking send; resends skip the pacing, as on the phone
            paceCredit = minOf(paceCredit + (if (paceBps > 0) paceBps / 8000.0 else 1e12), UdpVideoClient.PACE_BURST.toDouble())
            sending?.let { parts ->
                while (parts.isNotEmpty() && sndbufBytes + parts.first().size <= sndbufCap && paceCredit >= parts.first().size) {
                    val d = parts.removeFirst(); sndbuf.addLast(d); sndbufBytes += d.size; paceCredit -= d.size
                }
                if (parts.isEmpty()) { udpLink.sent(seq - 1, sendingFrame!!.atMs, now); sending = null }
            }
            // The phone's Wi-Fi, then the router
            wifiCredit += path.wifiMbps * 1000 / 8
            while (sndbuf.isNotEmpty() && wifiCredit >= sndbuf.first().size) {
                val d = sndbuf.removeFirst(); sndbufBytes -= d.size; wifiCredit -= d.size
                d.counter = counter++; sentDgrams++
                if (rnd.nextDouble() < path.loss || routerBytes + d.size > path.routerMbps * 1000 / 8 * path.bufferMs) { lostOnWay++; continue }
                router.addLast(d); routerBytes += d.size
            }
            if (sndbuf.isEmpty()) wifiCredit = minOf(wifiCredit, 1500.0)
            routerCredit += path.routerMbps * 1000 / 8
            while (router.isNotEmpty() && routerCredit >= router.first().size) {
                val d = router.removeFirst(); routerBytes -= d.size; routerCredit -= d.size
                d.at = now + path.delayMs; toDesktop.addLast(d)
            }
            if (router.isEmpty()) routerCredit = minOf(routerCredit, 1500.0)
            // Desktop: take what arrived
            while (toDesktop.isNotEmpty() && toDesktop.first().at <= now) {
                val d = toDesktop.removeFirst()
                received++; highest = maxOf(highest, d.counter)
                if (next != null && d.frame < next!!) continue
                val f = dframes.getOrPut(d.frame) { DFrame() }
                f.count = d.count
                if (f.parts.add(d.idx)) bytes += d.size - overhead
                newest = maxOf(newest, d.frame)
                if (d.frame > whole && f.complete()) { whole = d.frame; wholeAt = now }
            }
            // Desktop: hand on whole frames in order, ask again for missing pieces, give up after 250 ms
            if (now % 5 == 0L) {
                if (next == null) {
                    val start = dframes.entries.filter { it.value.complete() && it.key in keyframes }.maxOfOrNull { it.key }
                    if (start != null) { dframes.keys.removeAll { it < start }; next = start }
                    else {
                        // Repair the newest partial keyframe, like Reassembler.chase
                        val k = dframes.keys.filter { it in keyframes }.maxOrNull()
                        val f = k?.let { dframes[it] }
                        if (f == null) {
                            if (newest >= 0 && now - lastKeyAsk >= 300) { lastKeyAsk = now; toPhoneLater { wantKey = true } }
                        } else {
                            if (f.noticed < 0 || f.parts.size != f.progress) { f.noticed = now; f.progress = f.parts.size }
                            if (now - f.noticed >= 250) {
                                dframes.keys.removeAll { it <= k }
                                if (now - lastKeyAsk >= 300) { lastKeyAsk = now; toPhoneLater { wantKey = true } }
                            } else if (now - f.noticed >= 15 && now - f.asked >= 50) {
                                f.asked = now
                                val missing = (0 until f.count).filter { it !in f.parts }
                                toPhoneLater { resend(k, missing) }
                            }
                        }
                    }
                }
                while (next != null && dframes[next!!]?.complete() == true) {
                    dframes.remove(next!!)
                    encodedAt[next!!]?.let { latencies.add(now - it) }
                    shown++
                    next = next!! + 1
                }
                if (next != null) {
                    for (s in next!!..newest) {
                        val f = dframes.getOrPut(s) { DFrame() }
                        if (f.complete()) continue
                        if (f.noticed < 0 || (s in keyframes && f.parts.size != f.progress)) { f.noticed = now; f.progress = f.parts.size }
                        val waited = now - f.noticed
                        if (waited >= 250) {
                            gaveUp++; gaveUpAt.add(now); next = null; dframes.keys.removeAll { it <= s }
                            lastKeyAsk = now; toPhoneLater { wantKey = true }
                            break
                        }
                        val patience = if (s < newest) 15 else 60
                        if (waited >= patience && now - f.asked >= 50) {
                            f.asked = now
                            val missing = if (f.count == 0) emptyList() else (0 until f.count).filter { it !in f.parts }
                            toPhoneLater { resend(s, missing) }
                        }
                    }
                }
            }
            // Desktop: report every 100 ms
            if (now - lastReport >= 100) {
                lastReport = now
                val r = UdpReport(received, maxOf(0, highest), bytes, now, whole, (now - wholeAt).toInt().coerceIn(0, 65535), gaveUp)
                toPhoneLater { udpLink.report(r, now) }
            }
            while (toPhone.isNotEmpty() && toPhone.first().first <= now) toPhone.removeFirst().second()
            // Phone: sample every 250 ms, like MjpegServer
            if (now > 0 && now % MjpegServer.LINK_SAMPLE_MS == 0L) {
                (rateBytes * 8_000.0 / MjpegServer.LINK_SAMPLE_MS).let { encodedBps = if (encodedBps == 0.0) it else 0.8 * encodedBps + 0.2 * it }
                rateBytes = 0
                if (dynamic) { sndbufCap = MjpegServer.dynamicSendBuffer(encodedBps) * 2; paceBps = UdpVideoClient.paceFor(encodedBps, bitrate, if (controller!!.full && linkPace) udpLink.peakBps(now) else 0.0) }
                val take = udpLink.take(now, frames.firstOrNull()?.let { now - it.atMs } ?: 0L)
                if (take != null) {
                    controller?.update(DynamicBitrate.Sample(now, take.queueMs, take.sentBytes, encoded, take.loss, take.routerMs, take.lossLately))?.let { bitrate = it }
                    encoded = 0
                }
            }
            if (now % 1000 == 0L) {
                perSecond.add(bitrate)
                lossPerSecond.add(100.0 * (lostOnWay - lostMark) / maxOf(1, sentDgrams - sentMark)); lostMark = lostOnWay; sentMark = sentDgrams
            }
            now++
        }
        latencies.sort()
        fun pct(p: Double) = if (latencies.isEmpty()) 0L else latencies[((latencies.size - 1) * p).toInt()]
        val frozen = 1.0 - shown.toDouble() / maxOf(1, seq)
        return Result(name, bytes * 8.0 / seconds / 1e6, pct(0.5), pct(0.95), latencies.lastOrNull() ?: 0,
            100.0 * lostOnWay / maxOf(1, sentDgrams), gaveUp, 100.0 * frozen, perSecond, gaveUpAt, lossPerSecond, skips)
    }

    private fun compare(title: String, link: Link, seconds: Int, seed: Int = 1, usage: (Long) -> Double = { 0.9 }): Pair<Result, Result> {
        val dyn = simulate("Dynamic over UDP", true, link, seconds, seed, usage)
        val fixed = simulate("Auto over UDP (%.1f)".format(auto / 1e6), false, link, seconds, seed, usage)
        println("\n$title\n  $dyn\n  $fixed")
        println("  Dynamic's bitrate each second (Mbps): " + dyn.bitrates.joinToString(" ") { "%.1f".format(it / 1e6) })
        println("  Dynamic's loss each second (%): " + dyn.lossPerSecond.joinToString(" ") { "%.0f".format(it) })
        println("  Dynamic gave up at (s): " + dyn.gaveUpAt.joinToString(" ") { "%.1f".format(it / 1e3) })
        return dyn to fixed
    }

    @Test
    fun `a thin router settles under it without freezing`() {
        val (dyn, fixed) = compare("Router at 5 Mbps, 100 ms buffer, 0.5% loss", { Path(40.0, 5.0, 100, 0.005) }, 90)
        assertTrue(dyn.p95Ms < 300, "p95 ${dyn.p95Ms}")
        assertTrue(dyn.frozenPct < 5 && fixed.frozenPct > 50, "frozen ${dyn.frozenPct} vs ${fixed.frozenPct}")
        assertTrue(dyn.gaveUpAt.count { it >= 5_000 } <= 1, "freezes while it finds the link, then at most once when a probe overshoots: ${dyn.gaveUpAt}")
        assertTrue(dyn.bitrates.drop(10).all { it in 2_800_000..5_200_000 }, "${dyn.bitrates.drop(10)}")
    }

    @Test
    fun `a deep router buffer doesn't build up lag`() {
        val (dyn, fixed) = compare("Router at 5 Mbps, 1 s buffer, no loss", { Path(40.0, 5.0, 1000, 0.0) }, 90)
        assertTrue(dyn.p95Ms < 300 && dyn.frozenPct < 3, "p95 ${dyn.p95Ms} frozen ${dyn.frozenPct}")
        assertTrue(fixed.frozenPct > 50, "Auto loses most of it: ${fixed.frozenPct}")
    }

    @Test
    fun `slow wifi on the phone queues there and settles like over tcp`() {
        val (dyn, fixed) = compare("Phone's Wi-Fi at 6 Mbps", { Path(6.0, 100.0, 100, 0.0) }, 90)
        assertTrue(dyn.p95Ms < 300 && dyn.frozenPct < 3, "p95 ${dyn.p95Ms} frozen ${dyn.frozenPct}")
        assertTrue(fixed.p95Ms > 1_000, "Auto lags: ${fixed.p95Ms}")
        assertTrue(dyn.bitrates.drop(10).all { it in 3_000_000..6_500_000 }, "${dyn.bitrates.drop(10)}")
    }

    @Test
    fun `random loss the resends cover doesn't hold it back`() {
        val (dyn, _) = compare("Strong Wi-Fi, 1% random loss", { Path(60.0, 60.0, 100, 0.01) }, 60)
        assertTrue(dyn.bitrates.last() == top, "reaches the ceiling: ${dyn.bitrates.last()}")
        assertTrue(dyn.p95Ms < 150 && dyn.frozenPct < 1, "p95 ${dyn.p95Ms} frozen ${dyn.frozenPct}")
    }

    @Test
    fun `heavy random loss holds it where it is rather than driving it down`() {
        val (dyn, _) = compare("Strong Wi-Fi, 4% random loss", { Path(60.0, 60.0, 100, 0.04) }, 60)
        assertTrue(dyn.bitrates.all { it >= auto * 0.9 }, "${dyn.bitrates}")
        assertTrue(dyn.p95Ms < 150 && dyn.frozenPct < 1, "p95 ${dyn.p95Ms} frozen ${dyn.frozenPct}")
    }

    @Test
    fun `a link that drops recovers within seconds and climbs back after`() {
        val (dyn, _) = compare("Router drops from 25 to 4 Mbps at 30 s, back at 60 s",
            { ms -> Path(60.0, if (ms in 30_000 until 60_000) 4.0 else 25.0, 100, 0.002) }, 100)
        assertTrue(dyn.bitrates[33] <= 4_000_000, "down within 3 s: ${dyn.bitrates[33]}")
        assertTrue(dyn.bitrates[99] >= 15_000_000, "back up by the end: ${dyn.bitrates[99]}")
        assertTrue(dyn.p95Ms < 300 && dyn.frozenPct < 4, "p95 ${dyn.p95Ms} frozen ${dyn.frozenPct}")
    }

    @Test
    fun `a blip with nothing getting through is a stall, not a slower link`() {
        // A second of nothing (reports included), like Wi-Fi roaming
        val (dyn, _) = compare("10 Mbps router, nothing through for 1 s at 30 s",
            { ms -> if (ms in 30_000 until 31_000) Path(40.0, 0.0, 100, 1.0) else Path(40.0, 10.0, 100, 0.002) }, 45)
        val before = dyn.bitrates[29]
        assertTrue(dyn.bitrates.subList(31, 45).all { it >= before * 0.6 }, "no deep cut after it: $before then ${dyn.bitrates.subList(31, 45)}")
    }

    @Test
    fun `a still scene that starts moving doesn't overflow the phone's queue`() {
        val (dyn, _) = compare("6 Mbps router, still for 20 s then moving", { Path(40.0, 6.0, 100, 0.002) }, 60,
            usage = { ms -> if (ms < 20_000) 0.1 else 0.9 })
        assertTrue(dyn.skips <= 1, "skips ${dyn.skips}")
        assertTrue(dyn.p95Ms < 300 && dyn.frozenPct < 4, "p95 ${dyn.p95Ms} frozen ${dyn.frozenPct}")
    }

    @Test
    fun `random loss doesn't keep it down once the link is back`() {
        val (dyn, _) = compare("Router at 1.5 Mbps for 20 s then 25 Mbps, 2% random loss throughout",
            { ms -> Path(40.0, if (ms < 20_000) 1.5 else 25.0, 100, 0.02) }, 90)
        assertTrue(dyn.bitrates[19] < 2_000_000, "down while thin: ${dyn.bitrates[19]}")
        assertTrue(dyn.bitrates.last() >= 12_000_000, "back up: ${dyn.bitrates.last()}")
    }

    @Test
    fun `steady heavy loss at a low bitrate still cuts`() {
        // At 1 Mbps a loss share takes a couple of samples, which mustn't read as clear in between
        val dyn = simulate("Dynamic over UDP", true, { Path(40.0, 40.0, 100, 0.15) }, 20, startBps = 1_000_000)
        println("\n1 Mbps, 15% loss\n  $dyn\n  " + dyn.bitrates.joinToString(" ") { "%.2f".format(it / 1e6) })
        assertTrue(dyn.bitrates.drop(5).all { it < 1_000_000 }, "${dyn.bitrates}")
    }

    @Test
    fun `a still scene's big keyframes fit a thin router once the link's speed is known`() {
        // A desk with nothing moving puts half of each second into its keyframe. Paced at 2.5 times the video, that
        // overflowed a 4 Mbps router's 100 ms buffer every few seconds and the loss held Dynamic near half the link
        val link = Link { Path(40.0, 4.0, 100, 0.0) }
        val dyn = simulate("Dynamic over UDP", true, link, 60, usage = { 0.8 }, keyShare = 0.5)
        val before = simulate("2.5 times throughout", true, link, 60, usage = { 0.8 }, keyShare = 0.5, linkPace = false)
        println("\nStill scene, 4 Mbps router, 100 ms buffer\n  $dyn\n  $before")
        for (r in listOf(dyn, before)) {
            println("  ${r.name} (Mbps): " + r.bitrates.joinToString(" ") { "%.1f".format(it / 1e6) })
            println("  ${r.name} loss (%): " + r.lossPerSecond.joinToString(" ") { "%.0f".format(it) })
        }
        val lossy = dyn.lossPerSecond.drop(10).count { it > 3 }
        assertTrue(lossy <= 3 && lossy < before.lossPerSecond.drop(10).count { it > 3 }, "seconds over 3% loss: ${dyn.lossPerSecond}")
        val settled = dyn.bitrates.drop(20).sorted()[20]
        assertTrue(settled >= 2_600_000 && settled > before.bitrates.drop(20).sorted()[20], "settled at $settled: ${dyn.bitrates}")
    }

    @Test
    fun `steady heavy loss at the floor never climbs`() {
        val start = H264Stream.DYNAMIC_MIN_BPS
        val dyn = simulate("Dynamic over UDP", true, { Path(40.0, 40.0, 100, 0.15) }, 30, startBps = start)
        println("\nFloor, 15% loss\n  $dyn\n  " + dyn.bitrates.joinToString(" ") { "%.2f".format(it / 1e6) })
        assertTrue(dyn.bitrates.all { it <= start }, "${dyn.bitrates}")
    }
}
