package com.telescope

import android.content.Context

/**
 * Persists the single active pairing bearer token issued by the desktop
 * during QR pairing. One phone has one active paired desktop; saving a new
 * token always overwrites the previous one, which is the rotation/
 * revocation mechanism when re-pairing or resetting.
 */
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
