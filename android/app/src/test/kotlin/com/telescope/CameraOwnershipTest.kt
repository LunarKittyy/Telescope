package com.telescope

import java.util.concurrent.CountDownLatch
import java.util.concurrent.atomic.AtomicReference
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNotNull
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

class CameraOwnershipTest {

    private var now = 1_000L
    private val lock = CameraOwnerLock(clock = { now }, pendingMs = 5_000L)

    @Test
    fun `stream and probe are mutually exclusive`() {
        val probe = lock.tryAcquire(CameraOwner.PROBE)
        assertNotNull(probe)
        assertEquals(CameraOwner.PROBE, lock.holder())
        assertTrue(lock.isHeldBy(CameraOwner.PROBE))
        assertNull(lock.tryAcquire(CameraOwner.STREAM, adopted = false))
        assertNull(lock.tryAcquire(CameraOwner.PROBE))

        assertTrue(lock.release(probe!!.id))
        assertFalse(lock.isHeld())
        assertNull(lock.holder())

        val stream = lock.tryAcquire(CameraOwner.STREAM, adopted = false)
        assertNotNull(stream)
        assertNull(lock.tryAcquire(CameraOwner.PROBE))
        assertTrue(lock.release(stream!!.id))
        assertFalse(lock.isHeld())
    }

    @Test
    fun `unadopted stream lease expires after pending timeout and re-acquires on adopt when camera is free`() {
        val stream = lock.tryAcquire(CameraOwner.STREAM, adopted = false)!!
        now += 4_999L
        assertEquals(CameraOwner.STREAM, lock.holder())
        assertNull(lock.tryAcquire(CameraOwner.PROBE))

        now += 1L
        assertNull(lock.holder())
        // Nobody holds the camera, so a slow start adopting its expired lease re-acquires and succeeds.
        assertTrue(lock.adopt(stream.id, CameraOwner.STREAM))
        assertEquals(CameraOwner.STREAM, lock.holder())
        assertNull(lock.tryAcquire(CameraOwner.PROBE))
        assertTrue(lock.release(stream.id))
        assertNull(lock.holder())

        // If a probe took the camera after the unadopted lease expired, late adoption must still be refused.
        val slowStream = lock.tryAcquire(CameraOwner.STREAM, adopted = false)!!
        now += 5_000L
        assertNull(lock.holder())
        val probe = lock.tryAcquire(CameraOwner.PROBE)
        assertNotNull(probe)
        assertFalse(lock.adopt(slowStream.id, CameraOwner.STREAM))
        assertEquals(CameraOwner.PROBE, lock.holder())
        assertTrue(lock.release(probe!!.id))
    }

    @Test
    fun `overlapping stream leases during startup or teardown stay exclusive against probe`() {
        val first = lock.tryAcquire(CameraOwner.STREAM, adopted = false)!!
        val duplicate = lock.tryAcquire(CameraOwner.STREAM, adopted = false)
        assertNotNull(duplicate)
        assertNull(lock.tryAcquire(CameraOwner.PROBE))

        // First start adopts; duplicate start releases only its own lease.
        assertTrue(lock.adopt(first.id, CameraOwner.STREAM))
        assertTrue(lock.release(duplicate!!.id))
        assertEquals(CameraOwner.STREAM, lock.holder())
        assertNull(lock.tryAcquire(CameraOwner.PROBE))

        // Restart acquires a second stream lease while the old one is still tearing down.
        val restart = lock.tryAcquire(CameraOwner.STREAM, adopted = false)!!
        assertTrue(lock.release(first.id))
        assertEquals(CameraOwner.STREAM, lock.holder())
        assertNull(lock.tryAcquire(CameraOwner.PROBE))
        assertTrue(lock.adopt(restart.id, CameraOwner.STREAM))
        assertTrue(lock.release(restart.id))
        assertNull(lock.holder())
        assertNotNull(lock.tryAcquire(CameraOwner.PROBE))
    }

    @Test
    fun `releaseIfUnadopted clears pending lease at once without disturbing an adopted lease`() {
        val pending = lock.tryAcquire(CameraOwner.STREAM, adopted = false)!!
        assertTrue(lock.releaseIfUnadopted(pending.id))
        assertFalse(lock.isHeld())
        assertFalse(lock.releaseIfUnadopted(pending.id))

        val adopted = lock.tryAcquire(CameraOwner.STREAM, adopted = false)!!
        assertTrue(lock.adopt(adopted.id, CameraOwner.STREAM))
        assertFalse(lock.releaseIfUnadopted(adopted.id))
        assertEquals(CameraOwner.STREAM, lock.holder())
        assertTrue(lock.release(adopted.id))
        assertFalse(lock.isHeld())
    }

    @Test
    fun `adopted stream lease stays held indefinitely until released`() {
        val stream = lock.tryAcquire(CameraOwner.STREAM, adopted = false)!!
        assertTrue(lock.adopt(stream.id, CameraOwner.STREAM))

        now += 60_000L
        assertEquals(CameraOwner.STREAM, lock.holder())
        assertNull(lock.tryAcquire(CameraOwner.PROBE))

        assertTrue(lock.release(stream.id))
        assertNotNull(lock.tryAcquire(CameraOwner.PROBE))
    }

    @Test
    fun `release is idempotent and cannot clear another holder's lease`() {
        val first = lock.tryAcquire(CameraOwner.PROBE)!!
        assertTrue(lock.release(first.id))
        assertFalse(lock.release(first.id))

        val second = lock.tryAcquire(CameraOwner.STREAM, adopted = true)!!
        assertFalse(lock.release(first.id))
        assertEquals(CameraOwner.STREAM, lock.holder())
        assertTrue(lock.release(second.id))
        assertNull(lock.holder())
    }

    @Test
    fun `concurrent probe and remote stream start grant ownership to only one caller`() {
        repeat(50) {
            val shared = CameraOwnerLock(clock = { 1_000L })
            val ready = CountDownLatch(2)
            val go = CountDownLatch(1)
            val probeResult = AtomicReference<CameraLease?>()
            val streamResult = AtomicReference<CameraLease?>()

            val t1 = Thread {
                ready.countDown()
                go.await()
                probeResult.set(shared.tryAcquire(CameraOwner.PROBE, adopted = true))
            }
            val t2 = Thread {
                ready.countDown()
                go.await()
                streamResult.set(shared.tryAcquire(CameraOwner.STREAM, adopted = false))
            }
            t1.start()
            t2.start()
            ready.await()
            go.countDown()
            t1.join()
            t2.join()

            val winners = listOfNotNull(probeResult.get(), streamResult.get())
            assertEquals(1, winners.size)
            assertTrue(shared.release(winners.single().id))
            assertFalse(shared.isHeld())
        }
    }
}
