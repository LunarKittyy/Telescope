package com.telescope

import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.Json
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

class PhoneStatsTest {
    private var now = 1_000L
    private val stats = PhoneStats { now }

    @Test
    fun `measures nothing until the desktop asks, and stops when it lets go`() {
        stats.addEncode(12.0)
        stats.addQueue(5)
        assertNull(stats.report(30.0, 0, null))
        stats.ask(true)
        stats.addEncode(12.0)
        assertEquals(12.0, stats.report(null, null, null)?.encode_ms)
        stats.ask(false)
        assertFalse(stats.active())
        assertNull(stats.report(null, null, null))
        stats.ask(true)
        assertNull(stats.report(null, null, null)!!.encode_ms, "what was measured before is gone")
    }

    @Test
    fun `a desktop that stops asking times out`() {
        stats.ask(true)
        now += PhoneStats.LEASE_MS - 1
        assertTrue(stats.active())
        now += 2
        assertFalse(stats.active())
        stats.addEncode(9.0)
        assertNull(stats.report(null, null, null))
    }

    @Test
    fun `averages encode times, keeps the slowest and the longest queue over two seconds`() {
        stats.ask(true)
        stats.addEncode(10.0); stats.addEncode(20.0); stats.addEncode(30.0)
        stats.addQueue(3); stats.addQueue(40)
        val r = stats.report(29.97, 4, 1.04f)!!
        assertEquals(20.0, r.encode_ms)
        assertEquals(30.0, r.encode_max_ms)
        assertEquals(40.0, r.queue_ms)
        assertEquals(30.0, r.encoder_fps)
        assertEquals(4, r.thermal)
        assertEquals(1.04f, r.thermal_headroom)
        now += PhoneStats.WINDOW_MS + 1
        stats.ask(true)
        assertNull(stats.report(null, null, null)!!.queue_ms, "old samples fall out of the window")
    }

    @Test
    fun `leaves out what could not be measured`() {
        stats.ask(true)
        val r = stats.report(0.0, null, Float.NaN)!!
        assertNull(r.encoder_fps)
        assertNull(r.thermal)
        assertNull(r.thermal_headroom)
        assertEquals("{}", Json.encodeToString(r))
    }

    @Test
    fun `a camera timestamp only counts when it is on the clock it is compared with`() {
        val nowNs = 50_000_000_000L
        assertEquals(25.0, PhoneStats.delayMs((nowNs - 25_000_000L) / 1000, nowNs)!!, 0.001)
        assertNull(PhoneStats.delayMs((nowNs + 5_000_000L) / 1000, nowNs), "from the future")
        assertNull(PhoneStats.delayMs(1_000_000L, nowNs), "from another clock, seconds away")
        assertNull(PhoneStats.delayMs(0, nowNs))
    }

    @Test
    fun `a timestamp is read on whichever clock it is from, even after the phone slept`() {
        val monoNs = 50_000_000_000L
        val realNs = monoNs + 3_600_000_000_000L
        assertEquals(25.0, PhoneStats.delayMs((monoNs - 25_000_000L) / 1000, monoNs, realNs)!!, 0.001, "monotonic stamp")
        assertEquals(25.0, PhoneStats.delayMs((realNs - 25_000_000L) / 1000, monoNs, realNs)!!, 0.001, "elapsed-realtime stamp")
        assertNull(PhoneStats.delayMs(1_000_000L, monoNs, realNs), "on neither")
    }

    @Test
    fun `the lease runs on the monotonic clock, not the wall clock`() {
        val live = PhoneStats()
        live.ask(true)
        assertTrue(live.active())
        assertTrue(kotlin.math.abs(PhoneStats.monotonicMs() - System.nanoTime() / 1_000_000) < 1_000)
    }

    @Test
    fun `the packet queue reports how long each packet waited`() {
        var t = 0L
        val waits = mutableListOf<Long>()
        val queue = H264ClientQueue(clock = { t }, onWaited = { waits += it })
        queue.offer(byteArrayOf(1), key = true)
        t = 42
        queue.poll(0)
        assertEquals(listOf(42L), waits)
    }

    @Test
    fun `the state carries stats only while they were asked for, and an older desktop can ignore them`() {
        val json = Json { ignoreUnknownKeys = true; encodeDefaults = false }
        val report = PhoneStatsReport(encode_ms = 8.5, thermal = 2)
        val text = json.encodeToString(report)
        assertEquals("""{"encode_ms":8.5,"thermal":2}""", text)
        assertEquals(report, json.decodeFromString<PhoneStatsReport>(text))
        assertEquals(report, json.decodeFromString<PhoneStatsReport>("""{"encode_ms":8.5,"thermal":2,"future":1}"""))
    }
}
