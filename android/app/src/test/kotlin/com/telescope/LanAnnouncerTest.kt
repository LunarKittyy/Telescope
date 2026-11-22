package com.telescope

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

class LanAnnouncerTest {
    private fun bytes(s: String) = s.toByteArray(Charsets.UTF_8).size

    @Test
    fun `a short phone name is announced whole`() {
        assertEquals("Telescope Pixel 8 Pro", LanAnnouncer.serviceName("Pixel 8 Pro"))
    }

    @Test
    fun `a long name is cut to one DNS label instead of failing the announcement`() {
        val name = LanAnnouncer.serviceName("Marie-Christine's work phone for the Thursday evening classes")
        assertEquals(63, bytes(name))
        assertTrue(name.startsWith("Telescope Marie-Christine"))
    }

    @Test
    fun `the cut never splits a character`() {
        // Three bytes per character, emoji made of surrogate pairs, and two-byte accents
        for (phone in listOf("小明的华为手机".repeat(4), "📷".repeat(30), "ab" + "é".repeat(40))) {
            val name = LanAnnouncer.serviceName(phone)
            assertTrue(bytes(name) <= 63, name)
            assertEquals(name, String(name.toByteArray(Charsets.UTF_8), Charsets.UTF_8), "no broken character in $name")
            assertTrue(phone.startsWith(name.removePrefix("Telescope ")), name)
        }
    }
}
