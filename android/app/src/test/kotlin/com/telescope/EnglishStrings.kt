package com.telescope

import java.io.File
import java.util.Locale
import javax.xml.parsers.DocumentBuilderFactory
import org.w3c.dom.Element

// Strings read straight from res/values/strings.xml, so tests check the English text the app ships.
object EnglishStrings : Strings {
    private val doc = DocumentBuilderFactory.newInstance().newDocumentBuilder()
        .parse(File("src/main/res/values/strings.xml")).documentElement

    private fun name(type: Class<*>, id: Int): String =
        type.fields.first { it.getInt(null) == id }.name

    private fun element(tag: String, name: String): Element {
        val nodes = doc.getElementsByTagName(tag)
        return (0 until nodes.length).map { nodes.item(it) as Element }.first { it.getAttribute("name") == name }
    }

    // Undoes the escaping aapt undoes: \' \" \n and \\
    private fun unescape(raw: String) = raw.replace(Regex("""\\(.)""")) {
        when (val c = it.groupValues[1]) { "n" -> "\n"; "t" -> "\t"; else -> c }
    }

    override fun get(id: Int, vararg args: Any): String =
        String.format(Locale.ROOT, unescape(element("string", name(R.string::class.java, id)).textContent), *args)

    override fun plural(id: Int, count: Int, vararg args: Any): String {
        val items = element("plurals", name(R.plurals::class.java, id)).getElementsByTagName("item")
        val wanted = if (count == 1) "one" else "other"
        val item = (0 until items.length).map { items.item(it) as Element }.first { it.getAttribute("quantity") == wanted }
        return String.format(Locale.ROOT, unescape(item.textContent), *args)
    }
}
