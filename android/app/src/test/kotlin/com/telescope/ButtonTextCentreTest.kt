package com.telescope

import java.io.File
import java.nio.ByteBuffer
import javax.xml.parsers.DocumentBuilderFactory
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import org.w3c.dom.Element

// A label's capitals must sit in the middle of its button, as measured from the shipped font's own metrics
class ButtonTextCentreTest {
    private class Metrics(val perEm: Int, val hheaAscent: Int, val hheaDescent: Int, val yMax: Int, val yMin: Int, val capHeight: Int)

    private fun metrics(file: String): Metrics {
        val b = ByteBuffer.wrap(File("src/main/res/font/$file").readBytes())
        val tables = (0 until b.getShort(4).toInt()).associate { i ->
            val at = 12 + 16 * i
            String(ByteArray(4) { b.get(at + it) }) to b.getInt(at + 8)
        }
        val head = tables.getValue("head")
        val hhea = tables.getValue("hhea")
        val os2 = tables.getValue("OS/2")
        return Metrics(
            perEm = b.getShort(head + 18).toInt(),
            hheaAscent = b.getShort(hhea + 4).toInt(),
            hheaDescent = -b.getShort(hhea + 6).toInt(),
            yMax = b.getShort(head + 42).toInt(),
            yMin = -b.getShort(head + 38).toInt(),
            capHeight = b.getShort(os2 + 88).toInt(),
        )
    }

    // How far below the middle of its line the capitals sit, in dp at a 14 sp label; Android puts the line box in the button's middle
    private fun lowDp(m: Metrics, fontPadding: Boolean): Float {
        val ascent = if (fontPadding) m.yMax else m.hheaAscent
        val descent = if (fontPadding) m.yMin else m.hheaDescent
        val lineMiddle = (-ascent + descent) / 2f
        val capMiddle = -m.capHeight / 2f
        return (capMiddle - lineMiddle) * 14f / m.perEm
    }

    private fun style(name: String): Element {
        val doc = DocumentBuilderFactory.newInstance().newDocumentBuilder().parse(File("src/main/res/values/styles.xml"))
        val styles = doc.getElementsByTagName("style")
        return (0 until styles.length).map { styles.item(it) as Element }.first { it.getAttribute("name") == name }
    }

    private fun item(style: Element, name: String): String? {
        val items = style.getElementsByTagName("item")
        return (0 until items.length).map { items.item(it) as Element }.firstOrNull { it.getAttribute("name") == name }?.textContent
    }

    @Test
    fun `button style drops the font padding that sits the label low`() {
        val button = style("Widget.Telescope.Button")
        assertEquals("false", item(button, "android:includeFontPadding"))
        val m = metrics("ibm_plex_sans_semibold.ttf")
        val low = lowDp(m, fontPadding = false)
        assertTrue(low in -1f..1f, "capitals sit $low dp off the middle")
    }

    @Test
    fun `the font would sit the label low with its padding`() {
        // Guards the test itself: with padding the offset is past 1 dp, so the style item above is what keeps labels centred
        assertTrue(lowDp(metrics("ibm_plex_sans_semibold.ttf"), fontPadding = true) > 1f)
    }

    @Test
    fun `segmented options and dialog buttons drop the padding too`() {
        for (f in listOf("LookSegmented.kt", "Look.kt")) {
            assertTrue(File("src/main/kotlin/com/telescope/$f").readText().contains("includeFontPadding = false"), f)
        }
    }
}
