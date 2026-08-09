package com.telescope

import android.content.Context

// Persists pairing token; excluded from Android backup and device-transfer rules.
object TokenStore {
    private const val PREFS = "telescope_pairing"
    private const val KEY_TOKEN = "active_token"

    fun get(context: Context): String? =
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).getString(KEY_TOKEN, null)

    fun save(context: Context, token: String) {
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).edit()
            .putString(KEY_TOKEN, token).apply()
    }

    fun clear(context: Context) {
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).edit()
            .remove(KEY_TOKEN).apply()
    }
}
