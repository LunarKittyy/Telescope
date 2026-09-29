package com.telescope

import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import kotlin.random.Random

/**
 * Dynamic bitrate against simulated links: the real H264ClientQueue and DynamicBitrate, a VBR encoder with a
 * keyframe every second, a kernel send buffer and a link whose speed changes over time. Compared with Auto and a
 * maxed-out slider on the same links.
 */
class DynamicBitrateSimTest {

    private val w = 1920
    private val h = 1080
    private val fps = 30
    private val auto = H264Stream.defaultBitrate(w, h, fps)
    private val top = H264Stream.dynamicCeiling(w, h, fps)

    /** A link: megabits per second at a given ms. */
    private fun interface Link { fun mbps(ms: Long): Double }

    data class Result(
        val name: String,
        val meanMbps: Double,      // what reached the computer
        val p50Ms: Long,           // frame latency, encoded to received
        val p95Ms: Long,
        val maxMs: Long,
        val skips: Int,            // backlog dropped, picture skipped ahead
        val changes: Int,          // bitrate changes sent to the encoder
        val bitrates: List<Int>,   // the bitrate once a second
    ) {
        override fun toString() = "%-34s %6.1f Mbps  p50 %4d ms  p95 %5d ms  max %5d ms  skips %2d  changes %3d".format(
            name, meanMbps, p50Ms, p95Ms, maxMs, skips, changes)
    }

    /**
     * mode: a fixed bitrate, or H264Stream.DYNAMIC. usage: how much of its budget the encoder spends (motion).
     */
    private fun simulate(name: String, mode: Int, link: Link, seconds: Int, usage: (Long) -> Double = { 0.9 },
                         seed: Int = 1, auto: Int = this.auto, ceiling: Int = top): Result {
        val rnd = Random(seed)
        var now = 0L
        val dynamic = mode == H264Stream.DYNAMIC
        val controller = if (dynamic) DynamicBitrate(auto, ceiling) else null
        var bitrate = controller?.bitrate ?: if (mode == 0) auto else mode
        val queue = H264ClientQueue(clock = { now })
        if (dynamic) queue.maxWaitMs = MjpegServer.DYNAMIC_MAX_WAIT_MS
        var kernelCap = if (dynamic) MjpegServer.dynamicSendBuffer(bitrate.toDouble()) * 2 else 1 shl 20
        var encodedBps = 0.0

        // The kernel buffer: (bytes left, frame id, is the frame's last byte) in order.
        val kernel = ArrayDeque<LongArray>()
        var kernelBytes = 0L
        var writing: ByteArray? = null      // a write blocked on a full buffer
        var writingFrame = -1
        var writingDone = 0                 // bytes of it already in the buffer
        val frameAt = HashMap<Int, Long>()  // frame id -> when it was encoded
        val latencies = ArrayList<Long>()
        var received = 0L
        var skips = 0
        var changes = 0
        val perSecond = ArrayList<Int>()
        var encoded = 0L
        var frame = 0
        var wantKey = true
        var linkCredit = 0.0
        val step = 1L

        // Frame ids ride along in the packet: 4 bytes up front.
        fun packet(id: Int, size: Int) = ByteArray(maxOf(size, 8)).also {
            it[0] = (id shr 24).toByte(); it[1] = (id shr 16).toByte(); it[2] = (id shr 8).toByte(); it[3] = id.toByte()
        }
        fun idOf(p: ByteArray) = ((p[0].toInt() and 0xff) shl 24) or ((p[1].toInt() and 0xff) shl 16) or
            ((p[2].toInt() and 0xff) shl 8) or (p[3].toInt() and 0xff)

        while (now < seconds * 1000L) {
            // Encoder: a frame every 1/fps. P-frames around the budget, keyframes 5x a P-frame once a second.
            if (now * fps >= frame * 1000L) {
                val key = wantKey || frame % fps == 0
                wantKey = false
                val perFrame = bitrate / 8.0 / fps * usage(now)
                val size = (perFrame * (if (key) 5.0 else 25.0 / 29.0) * (0.8 + 0.4 * rnd.nextDouble())).toInt()
                frameAt[frame] = now
                encoded += size
                if (queue.offer(packet(frame, size), key)) { wantKey = true; skips++ }
                frame++
            }
            // Sender: take the next packet and push it into the kernel buffer as room allows.
            if (writing == null) {
                writing = queue.poll(0)?.also { writingFrame = idOf(it); writingDone = 0 }
            }
            writing?.let { p ->
                val room = (kernelCap - kernelBytes).toInt()
                // Like the server: 16 KB pieces, each reported once it's in.
                val n = minOf(room, p.size - writingDone, MjpegServer.WRITE_CHUNK)
                if (n > 0 && (n == MjpegServer.WRITE_CHUNK || n == p.size - writingDone)) {
                    writingDone += n
                    kernelBytes += n
                    kernel.addLast(longArrayOf(n.toLong(), writingFrame.toLong(), if (writingDone == p.size) 1 else 0))
                    queue.written(n, done = writingDone == p.size)
                }
                if (writingDone == p.size) writing = null
            }
            // Link: drain the kernel buffer at the link's speed.
            linkCredit += link.mbps(now) * 1_000_000 / 8 / 1000 * step
            while (kernel.isNotEmpty() && linkCredit >= 1) {
                val head = kernel.first()
                val n = minOf(head[0], linkCredit.toLong())
                head[0] -= n; linkCredit -= n; kernelBytes -= n; received += n
                if (head[0] == 0L) {
                    kernel.removeFirst()
                    if (head[2] == 1L) frameAt.remove(head[1].toInt())?.let { latencies.add(now - it) }
                }
            }
            if (kernel.isEmpty()) linkCredit = minOf(linkCredit, 1.0)  // an idle link doesn't bank capacity
            // Server: sample the link every 250 ms.
            if (now > 0 && now % MjpegServer.LINK_SAMPLE_MS == 0L) {
                val sample = DynamicBitrate.Sample(now, queue.takeQueueMs(), queue.takeSentBytes(), encoded)
                // Like the server: the socket buffer follows the encoder's rate (the kernel doubles it).
                encodedBps = 0.8 * encodedBps + 0.2 * encoded * 8_000.0 / MjpegServer.LINK_SAMPLE_MS
                if (dynamic) kernelCap = MjpegServer.dynamicSendBuffer(encodedBps) * 2
                encoded = 0
                controller?.update(sample)?.let { bitrate = it; changes++ }
            }
            if (now % 1000 == 0L) perSecond.add(bitrate)
            now += step
        }
        latencies.sort()
        fun pct(p: Double) = if (latencies.isEmpty()) 0L else latencies[((latencies.size - 1) * p).toInt()]
        return Result(name, received * 8.0 / seconds / 1_000_000, pct(0.5), pct(0.95), latencies.lastOrNull() ?: 0,
            skips, changes, perSecond)
    }

    private fun compare(title: String, link: Link, seconds: Int, usage: (Long) -> Double = { 0.9 }, seed: Int = 1,
                        auto: Int = this.auto, ceiling: Int = top): List<Result> {
        val results = listOf(
            simulate("Dynamic", H264Stream.DYNAMIC, link, seconds, usage, seed, auto, ceiling),
            simulate("Auto (%.1f Mbps)".format(auto / 1e6), 0, link, seconds, usage, seed, auto),
            simulate("Maxed slider (100 Mbps)", H264Stream.MAX_BPS, link, seconds, usage, seed, auto),
        )
        println("\n$title")
        results.forEach { println("  $it") }
        println("  Dynamic's bitrate each second (Mbps): " +
            results[0].bitrates.joinToString(" ") { "%.1f".format(it / 1e6) })
        return results
    }

    // Changes in direction of the bitrate after the first `settle` seconds: a sawtooth shows up as many.
    private fun swings(r: Result, settle: Int): Int {
        val b = r.bitrates.drop(settle)
        var swings = 0
        var dir = 0
        for (i in 1 until b.size) {
            val d = Integer.signum(b[i] - b[i - 1])
            if (d != 0 && d != dir) { if (dir != 0) swings++; dir = d }
        }
        return swings
    }

    @Test
    fun `strong wifi climbs to the ceiling and stays there`() {
        val (dyn, autoR, _) = compare("Strong Wi-Fi, 60 Mbps", { 60.0 }, 60)
        assertTrue(dyn.bitrates.last() == top, "reaches the ceiling")
        assertTrue(dyn.meanMbps > autoR.meanMbps * 1.8, "sends much more than Auto")
        assertTrue(dyn.p95Ms < 150, "stays quick: ${dyn.p95Ms}")
        assertTrue(dyn.skips == 0)
    }

    @Test
    fun `weak wifi settles just under the link without lag or a sawtooth`() {
        val (dyn, autoR, maxed) = compare("Weak Wi-Fi, 5 Mbps", { 5.0 }, 90)
        assertTrue(dyn.p95Ms < 400, "no lag: ${dyn.p95Ms}")
        assertTrue(dyn.bitrates.take(5).all { it >= 3_800_000 }, "doesn't overshoot on the way down: ${dyn.bitrates.take(5)}")
        assertTrue(autoR.p95Ms > 1_000 && maxed.p95Ms > 1_000, "the fixed ones lag")
        assertTrue(dyn.meanMbps > 3.5, "uses most of the link: ${dyn.meanMbps}")
        assertTrue(dyn.bitrates.drop(20).all { it in 4_000_000..7_000_000 }, "settles near it: ${dyn.bitrates.drop(20)}")
        assertTrue(swings(dyn, 20) <= 12, "doesn't keep swinging: ${swings(dyn, 20)}")
    }

    @Test
    fun `a link that drops recovers within seconds and climbs back after`() {
        val link = Link { ms -> if (ms in 30_000 until 60_000) 4.0 else 25.0 }
        val (dyn, _, _) = compare("Wi-Fi drops from 25 to 4 Mbps at 30 s, back at 60 s", link, 100)
        assertTrue(dyn.bitrates[33] <= 4_000_000, "down within 3 s: ${dyn.bitrates[33]}")
        assertTrue(dyn.bitrates[99] >= 15_000_000, "back up by the end: ${dyn.bitrates[99]}")
        assertTrue(dyn.p95Ms < 400, "p95 ${dyn.p95Ms}")
    }

    @Test
    fun `flaky wifi with stalls stays responsive`() {
        for (seed in listOf(7, 21, 99)) flaky(seed)
    }

    private fun flaky(seed: Int) {
        val rnd = Random(seed)
        val speeds = DoubleArray(240) { 4.0 + rnd.nextDouble() * 16.0 }  // a new speed every 500 ms, 4-20 Mbps
        val stalls = (1 until 12).map { it * 10_000L + rnd.nextLong(3_000) to 300L + rnd.nextLong(600) }
        val link = Link { ms ->
            if (stalls.any { (at, len) -> ms in at until at + len }) 0.0 else speeds[(ms / 500).toInt() % speeds.size]
        }
        val (dyn, autoR, maxed) = compare("Flaky Wi-Fi (seed $seed): 4-20 Mbps every 500 ms, 0.3-0.9 s stalls every 10 s",
            link, 120, seed = seed)
        // Faster swings than anything can follow: the best is Auto's safe level, and far better than maxing out.
        assertTrue(dyn.p95Ms < autoR.p95Ms * 1.25 && dyn.p95Ms < maxed.p95Ms / 3, "p95 ${dyn.p95Ms}")
        assertTrue(dyn.meanMbps > autoR.meanMbps * 0.8, "sends about what Auto does: ${dyn.meanMbps}")
        assertTrue(dyn.skips <= 2, "skips ahead only after the longest stalls: ${dyn.skips}")
    }

    @Test
    fun `usb climbs to the ceiling`() {
        val (dyn, _, _) = compare("USB, 300 Mbps", { 300.0 }, 40)
        assertTrue(dyn.bitrates.last() == top)
        assertTrue(dyn.p95Ms < 50)
    }

    @Test
    fun `a still scene doesn't climb on a budget it isn't using`() {
        val (dyn, _, _) = compare("Strong Wi-Fi, still scene (20% of the budget)", { 60.0 }, 40, usage = { 0.2 })
        assertTrue(dyn.bitrates.all { it == dyn.bitrates.first() }, "stays at Auto's rate")
    }

    @Test
    fun `a still scene that starts moving on a middling link settles quickly`() {
        val (dyn, _, _) = compare("10 Mbps Wi-Fi, still for 20 s then moving", { 10.0 }, 60,
            usage = { ms -> if (ms < 20_000) 0.2 else 0.9 })
        assertTrue(dyn.p95Ms < 300, "p95 ${dyn.p95Ms}")
        assertTrue(dyn.bitrates.drop(30).all { it in 8_000_000..13_500_000 }, "settles: ${dyn.bitrates.drop(30)}")
        assertTrue(swings(dyn, 30) <= 6, "doesn't keep swinging: ${swings(dyn, 30)}")
    }

    @Test
    fun `walking away from the router and back follows the link`() {
        // 30 Mbps down to 3 over a minute, then back up over the next.
        val link = Link { ms -> val t = (ms / 1000.0).coerceAtMost(120.0); if (t < 60) 30 - 27 * t / 60 else 3 + 27 * (t - 60) / 60 }
        val (dyn, autoR, maxed) = compare("Walking away: 30 Mbps down to 3 over 60 s, back over 60 s", link, 130)
        assertTrue(dyn.p95Ms < 400 && dyn.p95Ms < autoR.p95Ms && dyn.p95Ms < maxed.p95Ms, "p95 ${dyn.p95Ms}")
        assertTrue(dyn.bitrates[60] <= 4_000_000, "down with the link: ${dyn.bitrates[60]}")
        assertTrue(dyn.bitrates[125] >= 15_000_000, "back up with it: ${dyn.bitrates[125]}")
    }

    @Test
    fun `noisy but steady wifi doesn't make it jump around`() {
        // 12 Mbps give or take 30%, a new value every 100 ms: ordinary Wi-Fi.
        val rnd = Random(3)
        val noise = DoubleArray(1200) { 12.0 * (0.7 + 0.6 * rnd.nextDouble()) }
        val (dyn, autoR, _) = compare("Noisy Wi-Fi: 12 Mbps +-30% every 100 ms", { ms -> noise[(ms / 100).toInt() % noise.size] }, 100)
        assertTrue(dyn.p95Ms < 300, "p95 ${dyn.p95Ms}")
        assertTrue(dyn.meanMbps > autoR.meanMbps, "more than Auto: ${dyn.meanMbps}")
        assertTrue(swings(dyn, 20) <= 10, "doesn't keep swinging: ${swings(dyn, 20)}")
        assertTrue(dyn.bitrates.drop(20).all { it in 7_000_000..16_000_000 }, "stays near the link: ${dyn.bitrates.drop(20)}")
    }

    @Test
    fun `a 720p stream tops out at its own ceiling`() {
        val top720 = H264Stream.dynamicCeiling(1280, 720, 30)
        val (dyn, _, _) = compare("720p30 on strong Wi-Fi", { 60.0 }, 40,
            auto = H264Stream.defaultBitrate(1280, 720, 30), ceiling = top720)
        assertTrue(dyn.bitrates.last() == top720, "${dyn.bitrates.last()}")
        assertTrue(dyn.p95Ms < 50)
    }

    @Test
    fun `4k gets far more than auto's 30 mbps when the link has it`() {
        val auto4k = H264Stream.defaultBitrate(3840, 2160, 30)
        val top4k = H264Stream.dynamicCeiling(3840, 2160, 30)
        assertTrue(top4k in 75_000_000..85_000_000, "$top4k")
        val (dyn, autoR, _) = compare("4K30 on very strong Wi-Fi, 200 Mbps", { 200.0 }, 40, auto = auto4k, ceiling = top4k)
        assertTrue(dyn.bitrates.last() == top4k, "${dyn.bitrates.last()}")
        assertTrue(dyn.meanMbps > autoR.meanMbps * 1.8 && dyn.p95Ms < 100, "${dyn.meanMbps} ${dyn.p95Ms}")
    }

    @Test
    fun `4k on a link that can't carry the ceiling settles under it`() {
        val auto4k = H264Stream.defaultBitrate(3840, 2160, 30)
        val (dyn, _, maxed) = compare("4K30 on 50 Mbps Wi-Fi", { 50.0 }, 90, auto = auto4k,
            ceiling = H264Stream.dynamicCeiling(3840, 2160, 30))
        assertTrue(dyn.p95Ms < 300 && maxed.p95Ms > 1_000, "${dyn.p95Ms} ${maxed.p95Ms}")
        assertTrue(dyn.bitrates.drop(30).all { it in 35_000_000..62_000_000 }, "${dyn.bitrates.drop(30)}")
    }
}
