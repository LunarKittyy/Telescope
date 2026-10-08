package com.telescope

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Test

class HttpWireTest {

    private fun parse(body: String) = HttpWire.parseJsonParams(body.toByteArray(Charsets.UTF_8))

    @Test
    fun `a flat object comes back as strings`() {
        assertEquals(mapOf("action" to "iso", "value" to "100"), parse(" \n{\"action\":\"iso\",\"value\":100}"))
    }

    @Test
    fun `a body that isn't an object is refused before parsing`() {
        assertNull(parse("[".repeat(4000)))
        assertNull(parse("\"start\""))
        assertNull(parse(""))
    }

    @Test
    fun `nesting deep enough to overflow the stack is refused, not thrown`() {
        // Run on a small stack so the overflow happens at a depth the body cap allows
        var result: Map<String, String>? = mapOf()
        var thrown: Throwable? = null
        val t = Thread(null, {
            try { result = parse("{\"a\":" + "[".repeat(100_000)) } catch (e: Throwable) { thrown = e }
        }, "deep-json", 64 * 1024)
        t.start()
        t.join()
        assertNull(thrown)
        assertNull(result)
    }
}
