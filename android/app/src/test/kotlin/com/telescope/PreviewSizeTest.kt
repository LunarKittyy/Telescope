package com.telescope

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Test

class PreviewSizeTest {
    private val offered = listOf(
        (4032 to 3024), (3840 to 2160), (2560 to 1440), (1920 to 1080), (1280 to 720), (960 to 540), (640 to 480),
    )
    private val uhd = (3840 to 2160)

    @Test
    fun `a 4K stream on a full HD phone previews in full HD`() {
        assertEquals((1920 to 1080), PreviewSize.pick(uhd, offered, 1080, 2400))
    }

    @Test
    fun `a bigger screen gets the next size up`() {
        assertEquals((2560 to 1440), PreviewSize.pick(uhd, offered, 1440, 3200))
    }

    @Test
    fun `a size a quarter short of the screen is still taken when nothing is closer`() {
        val noQhd = offered - (2560 to 1440)
        assertEquals((1920 to 1080), PreviewSize.pick(uhd, noQhd, 1440, 3040))
    }

    @Test
    fun `a stream the screen can show in full keeps its size`() {
        assertEquals((1920 to 1080), PreviewSize.pick((1920 to 1080), offered, 1080, 2400))
        assertEquals((1920 to 1080), PreviewSize.pick((1920 to 1080), offered, 1440, 3040))
    }

    @Test
    fun `the screen's turn makes no difference`() {
        assertEquals(PreviewSize.pick(uhd, offered, 1080, 2400), PreviewSize.pick(uhd, offered, 2400, 1080))
    }

    @Test
    fun `a stream upright on its side picks the same size turned`() {
        assertEquals((1080 to 1920), PreviewSize.pick((2160 to 3840), listOf((1080 to 1920), (2160 to 3840)), 1080, 2400))
    }

    @Test
    fun `no size with the stream's shape keeps the stream's size`() {
        assertEquals(uhd, PreviewSize.pick(uhd, listOf((4032 to 3024), (1600 to 1200), (640 to 480)), 1080, 2400))
    }

    @Test
    fun `an odd screen or an empty list keeps the stream's size`() {
        assertEquals(uhd, PreviewSize.pick(uhd, offered, 0, 0))
        assertEquals(uhd, PreviewSize.pick(uhd, emptyList(), 1080, 2400))
    }

    @Test
    fun `a small screen takes a small size but never a bigger one`() {
        assertEquals((960 to 540), PreviewSize.pick(uhd, offered, 360, 640))
        assertEquals((1920 to 1080), PreviewSize.pick((1920 to 1080), listOf((3840 to 2160), (1920 to 1080)), 360, 640))
    }
}
