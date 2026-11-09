package com.telescope

import java.io.File
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assumptions.assumeTrue
import org.junit.jupiter.api.Test

// The phone's colours are generated from the desktop's theme.slint (tools/gen_look.py); this fails when the two have drifted apart
class LookTokensTest {
    @Test
    fun `generated looks match the desktop theme`() {
        val script = File("../tools/gen_look.py")
        val theme = File("../../rust/crates/ui-kit/ui/theme.slint")
        assumeTrue(script.exists() && theme.exists(), "needs the whole repository")
        val process = try {
            ProcessBuilder("python3", script.path, "--check").redirectErrorStream(true).start()
        } catch (_: java.io.IOException) {
            assumeTrue(false, "needs python3")
            return
        }
        val output = process.inputStream.bufferedReader().readText()
        assertEquals(0, process.waitFor(), output)
    }
}
