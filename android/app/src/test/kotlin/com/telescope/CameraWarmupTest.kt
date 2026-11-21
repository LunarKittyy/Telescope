package com.telescope

import org.junit.jupiter.api.AfterEach
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertSame
import org.junit.jupiter.api.Test

class CameraWarmupTest {
    private fun entry(id: String) = CameraEntry(id, null, "ID $id", false, 50, 3200, 100_000L, 1_000_000_000L)

    @AfterEach
    fun reset() = CameraWarmup.forget()

    @Test
    fun `a list missing a lens is not kept, so the next read looks again`() {
        val ids = listOf("0", "1")
        var reads = 0
        val partial = CameraCatalogue(listOf(entry("0")), complete = false)
        val full = CameraCatalogue(listOf(entry("0"), entry("1")), complete = true)

        assertSame(partial, CameraWarmup.catalogue(ids, 0L) { reads++; partial })
        assertSame(full, CameraWarmup.catalogue(ids, 1_000L) { reads++; full })
        assertEquals(2, reads)
    }

    @Test
    fun `a complete list is reused while it is recent and the ids match`() {
        val ids = listOf("0", "1")
        var reads = 0
        val full = CameraCatalogue(listOf(entry("0"), entry("1")), complete = true)

        CameraWarmup.catalogue(ids, 0L) { reads++; full }
        CameraWarmup.catalogue(ids, 60_000L) { reads++; full }
        assertEquals(1, reads)
        CameraWarmup.catalogue(listOf("0"), 61_000L) { reads++; full }
        assertEquals(2, reads)
    }
}
