package com.telescope

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit

class RunGateTest {
    @Test
    fun `only the current run gets through`() {
        val gate = RunGate<String>()
        gate.begin("one")
        assertTrue(gate.ifCurrent("one") {})
        gate.begin("two")
        assertFalse(gate.ifCurrent("one") { error("stale run") })
        gate.end {}
        assertFalse(gate.ifCurrent("two") { error("ended run") })
    }

    @Test
    fun `a run ending while a callback is mid-way waits for it, so Idle is the last state`() {
        val gate = RunGate<String>()
        val machine = StreamStateMachine()
        gate.begin("run")
        val inside = CountDownLatch(1)
        val letGo = CountDownLatch(1)
        val camera = Thread {
            gate.ifCurrent("run") {
                inside.countDown()  // checked: the run is still the current one
                letGo.await(5, TimeUnit.SECONDS)
                machine.transition(StreamState.Streaming, "late callback")
            }
        }
        camera.start()
        assertTrue(inside.await(5, TimeUnit.SECONDS))
        val stopper = Thread { gate.end { machine.transition(StreamState.Idle, "stop") } }
        stopper.start()
        Thread.sleep(100)  // long enough for the stop to have run if nothing held it back
        letGo.countDown()
        camera.join(5000)
        stopper.join(5000)
        assertEquals(StreamState.Idle, machine.state)
    }
}
