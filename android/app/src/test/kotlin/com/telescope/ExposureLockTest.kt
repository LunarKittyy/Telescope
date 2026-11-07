package com.telescope

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

class ExposureLockTest {
    private fun asked() = ExposureLock().also { assertTrue(it.request(available = true)) }

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
        lock.onResult(requestHadLocks = true, aeLocked = true, awbLocked = true)
        assertEquals("locked", lock.wire)
    }

    @Test
    fun `one part still settling keeps it locking`() {
        val lock = asked()
        lock.onResult(requestHadLocks = true, aeLocked = true, awbLocked = false)
        assertEquals("locking", lock.wire)
        lock.onResult(requestHadLocks = true, aeLocked = false, awbLocked = true)
        assertEquals("locking", lock.wire)
        lock.onResult(requestHadLocks = true, aeLocked = true, awbLocked = true)
        assertEquals("locked", lock.wire)
    }

    @Test
    fun `a result of a request from before the locks does not confirm`() {
        val lock = asked()
        lock.onResult(requestHadLocks = false, aeLocked = true, awbLocked = true)
        assertEquals("locking", lock.wire)
    }

    @Test
    fun `a result with nothing asked changes nothing`() {
        val lock = ExposureLock()
        lock.onResult(requestHadLocks = true, aeLocked = true, awbLocked = true)
        assertEquals("off", lock.wire)
    }

    @Test
    fun `asking again while locked keeps it locked`() {
        val lock = asked()
        lock.onResult(true, true, true)
        assertTrue(lock.request(available = true))
        assertEquals("locked", lock.wire)
    }

    @Test
    fun `unlocking is immediate and a late locked result does not bring it back`() {
        val lock = asked()
        lock.onResult(true, true, true)
        lock.release()
        assertEquals("off", lock.wire)
        assertFalse(lock.asked)
        lock.onResult(requestHadLocks = true, aeLocked = true, awbLocked = true)
        assertEquals("off", lock.wire)
    }

    @Test
    fun `a lens that cannot lock refuses and stays off`() {
        val lock = ExposureLock()
        assertFalse(lock.request(available = false))
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
            lock.onResult(true, true, true)
            lock.onRebuild(cause)
            val kept = cause == StillRebuild.Cause.SESSION
            assertEquals(if (kept) "locked" else "off", lock.wire, "after $cause")
        }
    }

    @Test
    fun `a lock asked for again after a lens change has to be confirmed again`() {
        val lock = asked()
        lock.onResult(true, true, true)
        lock.onRebuild(StillRebuild.Cause.LENS)
        assertTrue(lock.request(available = true))
        assertEquals("locking", lock.wire)
    }
}
