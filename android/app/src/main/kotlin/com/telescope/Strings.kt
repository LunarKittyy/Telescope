package com.telescope

import android.content.Context
import androidx.annotation.PluralsRes
import androidx.annotation.StringRes

// The app's texts (res/values/strings.xml), for code that builds messages without a Context, so its unit
// tests can check the English text against the same file.
interface Strings {
    fun get(@StringRes id: Int, vararg args: Any): String
    fun plural(@PluralsRes id: Int, count: Int, vararg args: Any): String
}

fun Context.strings(): Strings {
    val res = resources
    return object : Strings {
        override fun get(id: Int, vararg args: Any) = res.getString(id, *args)
        override fun plural(id: Int, count: Int, vararg args: Any) = res.getQuantityString(id, count, *args)
    }
}
