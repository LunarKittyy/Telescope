package com.telescope

import android.os.SystemClock

// The components that can open Camera2 exclusively inside this app.
enum class CameraOwner {
    STREAM,
    PROBE,
}

// A unique reservation token returned when ownership is granted; only this id can adopt or release it.
data class CameraLease(
    val id: Long,
    val owner: CameraOwner,
)

// Exclusive camera coordinator shared between stream startup and the Camera2 probe so a remote stream start cannot race a probe.
// An unadopted lease (stream start queued in an intent before onStartCommand runs) expires after pendingMs so a lost start cannot wedge the camera.
open class CameraOwnerLock(
    private val clock: () -> Long,
    private val pendingMs: Long = StartWindow.PENDING_MS,
) {
    private data class Active(
        val id: Long,
        val owner: CameraOwner,
        var adopted: Boolean,
        val acquiredAtMs: Long,
    )

    private var nextId = 0L
    private val active = LinkedHashMap<Long, Active>()

    private fun expireStale() {
        if (active.isEmpty()) return
        val now = clock()
        val iter = active.values.iterator()
        while (iter.hasNext()) {
            val current = iter.next()
            if (!current.adopted && now - current.acquiredAtMs >= pendingMs) {
                iter.remove()
            }
        }
    }

    private fun currentOwner(): CameraOwner? = active.values.firstOrNull()?.owner

    // Grants ownership when the camera is free, or adds another STREAM lease while STREAM already holds it
    // (duplicate starts during startup or Local-only restart while the previous controller finishes teardown).
    // STREAM and PROBE remain mutually exclusive, and PROBE is always single-holder.
    @Synchronized
    fun tryAcquire(owner: CameraOwner, adopted: Boolean = true): CameraLease? {
        expireStale()
        val current = currentOwner()
        if (current != null && !(owner == CameraOwner.STREAM && current == CameraOwner.STREAM)) return null
        val id = ++nextId
        active[id] = Active(id = id, owner = owner, adopted = adopted, acquiredAtMs = clock())
        return CameraLease(id = id, owner = owner)
    }

    // Promotes a pending lease once the owning service has actually started.
    // If an unadopted lease expired and nobody else holds the camera, re-acquires under the same leaseId
    // so a slow start still succeeds; returns false only when another incompatible owner holds the camera.
    @Synchronized
    fun adopt(leaseId: Long, owner: CameraOwner): Boolean {
        if (leaseId <= 0L) return false
        expireStale()
        val existing = active[leaseId]
        if (existing != null) {
            if (existing.owner != owner) return false
            existing.adopted = true
            return true
        }
        val current = currentOwner()
        if (current != null && !(owner == CameraOwner.STREAM && current == CameraOwner.STREAM)) return false
        if (leaseId > nextId) nextId = leaseId
        active[leaseId] = Active(id = leaseId, owner = owner, adopted = true, acquiredAtMs = clock())
        return true
    }

    // Releases ownership only when leaseId is still active; safe to call repeatedly.
    @Synchronized
    fun release(leaseId: Long): Boolean = active.remove(leaseId) != null

    // Releases a pending lease that has not been adopted yet (e.g. stop-before-start abort); leaves adopted leases intact.
    @Synchronized
    fun releaseIfUnadopted(leaseId: Long): Boolean {
        val existing = active[leaseId] ?: return false
        if (existing.adopted) return false
        active.remove(leaseId)
        return true
    }

    @Synchronized
    fun holder(): CameraOwner? {
        expireStale()
        return currentOwner()
    }

    @Synchronized
    fun isHeld(): Boolean = holder() != null

    @Synchronized
    fun isHeldBy(owner: CameraOwner): Boolean = holder() == owner
}

object CameraOwnership : CameraOwnerLock({ SystemClock.elapsedRealtime() })
