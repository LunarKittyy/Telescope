package com.telescope

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

class PairedComputerListTest {

    private fun pc(id: String, token: String) = PairedComputer(id, "PC $id", token, 0L)

    @Test
    fun `re-pairing the same computer replaces its entry and old token`() {
        val list = PairedComputerList().withAdded(pc("a", "old")).withAdded(pc("a", "new"))
        assertEquals(listOf("new"), list.computers.map { it.token })
        assertNull(list.matchToken("old"))
    }

    @Test
    fun `other computers survive adding and removing one`() {
        val list = PairedComputerList().withAdded(pc("a", "ta")).withAdded(pc("b", "tb")).without("a")
        assertEquals(listOf("b"), list.computers.map { it.id })
        assertEquals("b", list.matchToken("tb")?.id)
    }

    @Test
    fun `missing or empty tokens never match`() {
        val list = PairedComputerList().withAdded(pc("a", "ta"))
        assertNull(list.matchToken(null))
        assertNull(list.matchToken(""))
    }

    @Test
    fun `round-trips through storage and treats garbage as no pairings`() {
        val list = PairedComputerList().withAdded(pc("a", "ta")).withAdded(pc("b", "tb"))
        assertEquals(list.computers, PairedComputerList.decode(list.encode()).computers)
        assertEquals(emptyList<PairedComputer>(), PairedComputerList.decode("{not json").computers)
        assertEquals(emptyList<PairedComputer>(), PairedComputerList.decode(null).computers)
    }

    @Test
    fun `the newest three show until Show all unfolds the rest`() {
        val list = (1..5).fold(PairedComputerList()) { l, i -> l.withAdded(PairedComputer("$i", "PC $i", "t$i", i * 1000L)) }
        assertEquals(listOf("5", "4", "3"), list.shown(3, showAll = false).map { it.id })
        assertEquals(listOf("5", "4", "3", "2", "1"), list.shown(3, showAll = true).map { it.id })
        assertEquals(listOf("a"), PairedComputerList().withAdded(pc("a", "ta")).shown(3, showAll = false).map { it.id })
    }

    @Test
    fun `Remove all is offered from two computers on`() {
        assertFalse(PairedComputerList().offersRemoveAll)
        assertFalse(PairedComputerList().withAdded(pc("a", "ta")).offersRemoveAll)
        assertTrue(PairedComputerList().withAdded(pc("a", "ta")).withAdded(pc("b", "tb")).offersRemoveAll)
    }
}
