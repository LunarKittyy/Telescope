package com.telescope

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNotNull
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

class UpdateLogicTest {

    private fun manifest(
        versionCode: Int = 120,
        url: String = "https://github.com/x/Telescope.apk",
        sha: String = "a".repeat(64),
        channel: String = "nightly",
    ) = """
        {"schema":1,"version":"0.6.0","build":$versionCode,"channel":"$channel","versionName":"0.6.0-$channel.$versionCode",
         "commit":"abc","date":"2026-01-01T00:00:00Z","protocol":2,"notes":"https://notes",
         "assets":[{"name":"Telescope.apk","url":"$url","sha256":"$sha","size":1234},
                   {"name":"Telescope-linux.tar.gz","url":"https://x/l","sha256":"${"b".repeat(64)}","size":9}],
         "android":{"versionCode":$versionCode,"versionName":"0.6.0-$channel.$versionCode","asset":"Telescope.apk"}}
    """.trimIndent()

    @Test
    fun `parses the release manifest and finds the apk`() {
        val parsed = UpdateLogic.parse(manifest())
        assertNotNull(parsed)
        val m = parsed!!
        assertEquals(1234L, UpdateLogic.apkAsset(m)!!.size)
        assertEquals("0.6.0 nightly 120", UpdateLogic.displayVersion(m))
        assertEquals("0.6.0", UpdateLogic.displayVersion(UpdateLogic.parse(manifest(channel = "stable"))!!))
    }

    @Test
    fun `refuses manifests it can't trust`() {
        assertNull(UpdateLogic.parse("not json"))
        assertNull(UpdateLogic.parse(manifest(url = "http://insecure/Telescope.apk")))
        assertNull(UpdateLogic.parse(manifest(sha = "short")))
        assertNull(UpdateLogic.parse("""{"version":"1"}"""))
    }

    @Test
    fun `only a higher version code is an update`() {
        val m = UpdateLogic.parse(manifest(versionCode = 120))!!
        assertTrue(UpdateLogic.isNewer(m, 100))
        assertFalse(UpdateLogic.isNewer(m, 120))
        assertFalse(UpdateLogic.isNewer(m, 130))
    }

    @Test
    fun `channel urls and defaults`() {
        assertTrue(UpdateLogic.manifestUrl("stable").contains("/releases/latest/download/manifest.json"))
        assertTrue(UpdateLogic.manifestUrl("nightly").contains("/releases/download/nightly/manifest.json"))
        assertEquals("nightly", UpdateLogic.defaultChannel("nightly"))
        assertEquals("stable", UpdateLogic.defaultChannel("dev"))
        assertEquals("stable", UpdateLogic.defaultChannel("stable"))
    }

    @Test
    fun `sha256 and daily check`() {
        assertEquals(
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            UpdateLogic.sha256Hex("abc".byteInputStream()),
        )
        assertTrue(UpdateLogic.checkDue(0, UpdateLogic.DAY_MS))
        assertFalse(UpdateLogic.checkDue(1000, 1000 + UpdateLogic.DAY_MS - 1))
    }
}
