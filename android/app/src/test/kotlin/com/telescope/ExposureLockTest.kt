package com.telescope

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

class ExposureLockTest {
    private fun asked() = ExposureLock().also { assertTrue(it.request(available = true, now = 0)) }

    @Test
    fun `a lock starts off`() {
        val lock = ExposureLock()
        assertEquals("off", lock.wire)
        assertFalse(lock.asked)
    }

    @Test
    fun `asking puts the locks in the request but is not yet a lock`() {
        val lock = asked()
        assertEquals(ExposureLock.Phase.LOCKING, lock.phase)
        assertEquals("locking", lock.wire)
        assertTrue(lock.asked)
    }

    @Test
    fun `both states locked on a request that carried the locks confirms it`() {
        val lock = asked()
        lock.onResult(requestGeneration = 1, aeLocked = true, awbLocked = true)
        assertEquals("locked", lock.wire)
    }

    @Test
    fun `one part still settling keeps it locking`() {
        val lock = asked()
        lock.onResult(requestGeneration = 1, aeLocked = true, awbLocked = false)
        assertEquals("locking", lock.wire)
        lock.onResult(requestGeneration = 1, aeLocked = false, awbLocked = true)
        assertEquals("locking", lock.wire)
        lock.onResult(requestGeneration = 1, aeLocked = true, awbLocked = true)
        assertEquals("locked", lock.wire)
    }

    @Test
    fun `a result of a request from before the locks does not confirm`() {
        val lock = asked()
        lock.onResult(requestGeneration = null, aeLocked = true, awbLocked = true)
        assertEquals("locking", lock.wire)
    }

    @Test
    fun `a result with nothing asked changes nothing`() {
        val lock = ExposureLock()
        lock.onResult(requestGeneration = 1, aeLocked = true, awbLocked = true)
        assertEquals("off", lock.wire)
    }

    @Test
    fun `asking again while locked keeps it locked`() {
        val lock = asked()
        lock.onResult(1, true, true)
        assertTrue(lock.request(available = true, now = 0))
        assertEquals("locked", lock.wire)
    }

    @Test
    fun `unlocking is immediate and a late locked result does not bring it back`() {
        val lock = asked()
        lock.onResult(1, true, true)
        lock.release()
        assertEquals("off", lock.wire)
        assertFalse(lock.asked)
        lock.onResult(requestGeneration = 1, aeLocked = true, awbLocked = true)
        assertEquals("off", lock.wire)
    }

    @Test
    fun `a lens that cannot lock refuses and stays off`() {
        val lock = ExposureLock()
        assertFalse(lock.request(available = false, now = 0))
        assertEquals("off", lock.wire)
    }

    @Test
    fun `both locks have to be available`() {
        assertTrue(ExposureLock.available(aeLockAvailable = true, awbLockAvailable = true))
        assertFalse(ExposureLock.available(aeLockAvailable = true, awbLockAvailable = false))
        assertFalse(ExposureLock.available(aeLockAvailable = false, awbLockAvailable = true))
    }

    @Test
    fun `another lens, a reopen or the camera going off lets go but a rebuilt session keeps it`() {
        for (cause in StillRebuild.Cause.values()) {
            val lock = asked()
            lock.onResult(1, true, true)
            lock.onRebuild(cause)
            val kept = cause == StillRebuild.Cause.SESSION
            assertEquals(if (kept) "locked" else "off", lock.wire, "after $cause")
        }
    }

    @Test
    fun `a lock asked for again after a lens change has to be confirmed again`() {
        val lock = asked()
        lock.onResult(1, true, true)
        lock.onRebuild(StillRebuild.Cause.LENS)
        assertTrue(lock.request(available = true, now = 0))
        assertEquals("locking", lock.wire)
    }

    @Test
    fun `a result of an earlier lock's request does not confirm a later one`() {
        val lock = asked()
        lock.onResult(requestGeneration = 1, aeLocked = true, awbLocked = true)
        lock.release()
        assertTrue(lock.request(available = true, now = 10))
        assertEquals(2, lock.generation)
        lock.onResult(requestGeneration = 1, aeLocked = true, awbLocked = true)
        assertEquals("locking", lock.wire, "a frame still on its way from the first lock")
        lock.onResult(requestGeneration = 2, aeLocked = true, awbLocked = true)
        assertEquals("locked", lock.wire)
    }

    @Test
    fun `a lock the desktop stops renewing lapses`() {
        val lock = asked()
        lock.onResult(1, true, true)
        assertFalse(lock.lapse(ExposureLock.LEASE_MS))
        assertEquals("locked", lock.wire)
        assertTrue(lock.lapse(ExposureLock.LEASE_MS + 1))
        assertEquals("off", lock.wire)
        assertFalse(lock.lapse(ExposureLock.LEASE_MS * 5), "nothing to let go of")
    }

    @Test
    fun `asking again renews the lease and keeps the generation`() {
        val lock = asked()
        assertTrue(lock.request(available = true, now = ExposureLock.LEASE_MS - 1))
        assertEquals(1, lock.generation)
        assertFalse(lock.lapse(ExposureLock.LEASE_MS + 5))
        assertTrue(lock.lapse(ExposureLock.LEASE_MS * 2))
    }

    @Test
    fun `a camera that leaves the white balance state out is taken at its lock`() {
        assertTrue(ExposureLock.awbHeld(byHand = false, state = null))
        assertTrue(ExposureLock.awbHeld(byHand = false, state = 3))
        assertFalse(ExposureLock.awbHeld(byHand = false, state = 2))
        assertTrue(ExposureLock.awbHeld(byHand = true, state = 2))
    }

    @Test
    fun `auto exposure has to say it holds unless it is set by hand`() {
        assertFalse(ExposureLock.aeHeld(byHand = false, state = null))
        assertFalse(ExposureLock.aeHeld(byHand = false, state = 2))
        assertTrue(ExposureLock.aeHeld(byHand = false, state = 3))
        assertTrue(ExposureLock.aeHeld(byHand = true, state = null))
    }
}
