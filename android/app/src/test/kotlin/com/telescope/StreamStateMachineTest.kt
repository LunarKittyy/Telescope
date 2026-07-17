package com.telescope

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

class StreamStateMachineTest {

    @Test
    fun `starts idle and not streaming`() {
        val m = StreamStateMachine()
        assertEquals(StreamState.Idle, m.state)
        assertFalse(m.isStreaming)
        assertTrue(m.recentTransitions().isEmpty())
    }

    @Test
    fun `isStreaming is true only in the Streaming state`() {
        val m = StreamStateMachine()
        for (s in StreamState.entries) {
            m.transition(s, "op")
            assertEquals(s == StreamState.Streaming, m.isStreaming)
        }
    }

    @Test
    fun `transition records from-to-op and updates current state`() {
        val m = StreamStateMachine()
        val t = m.transition(StreamState.StartingServer, "onStartCommand")
        assertEquals(StreamState.Idle, t.from)
        assertEquals(StreamState.StartingServer, t.to)
        assertEquals("onStartCommand", t.op)
        assertEquals(StreamState.StartingServer, m.state)
    }

    @Test
    fun `error is reduced to class name and message only`() {
        val m = StreamStateMachine()
        m.transition(StreamState.Failed, "openCamera", RuntimeException("boom"))
        val t = m.recentTransitions().single()
        assertEquals("RuntimeException: boom", t.error)
    }

    @Test
    fun `open-then-configure-failure sequence ends Failed with no Streaming ever reported`() {
        val m = StreamStateMachine()
        m.transition(StreamState.StartingServer, "onStartCommand")
        m.transition(StreamState.OpeningCamera, "onStartCommand")
        m.transition(StreamState.ConfiguringSession, "openCamera.onOpened")
        m.transition(StreamState.Failed, "createLegacySession.onConfigureFailed")

        assertEquals(StreamState.Failed, m.state)
        assertFalse(m.isStreaming)
        assertTrue(m.recentTransitions().none { it.to == StreamState.Streaming })
    }

    @Test
    fun `stop during startup transitions straight to Stopping then Idle`() {
        val m = StreamStateMachine()
        m.transition(StreamState.StartingServer, "onStartCommand")
        m.transition(StreamState.OpeningCamera, "onStartCommand")
        // stopStreaming() races an in-flight open before it ever reaches Streaming.
        m.transition(StreamState.Stopping, "stopStreaming")
        m.transition(StreamState.Idle, "stopStreaming")

        assertEquals(StreamState.Idle, m.state)
        assertFalse(m.isStreaming)
    }

    @Test
    fun `recovering a lens switch returns to Streaming on success`() {
        val m = StreamStateMachine()
        m.transition(StreamState.Streaming, "startRepeating")
        m.transition(StreamState.Recovering, "switchCameraTo")
        assertFalse(m.isStreaming)
        m.transition(StreamState.Streaming, "startRepeating")
        assertTrue(m.isStreaming)
    }

    @Test
    fun `history is bounded and drops the oldest entries first`() {
        val m = StreamStateMachine()
        repeat(StreamStateMachine.MAX_HISTORY + 5) { i ->
            m.transition(StreamState.Recovering, "op$i")
        }
        val recent = m.recentTransitions()
        assertEquals(StreamStateMachine.MAX_HISTORY, recent.size)
        assertEquals("op5", recent.first().op)
        assertEquals("op${StreamStateMachine.MAX_HISTORY + 4}", recent.last().op)
    }

    @Test
    fun `timestamps come from the injected clock`() {
        var t = 1_000L
        val m = StreamStateMachine(now = { t })
        t = 42L
        val transition = m.transition(StreamState.StartingServer, "op")
        assertEquals(42L, transition.timestampMs)
    }
}
