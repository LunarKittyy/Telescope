package com.telescope

import android.content.Context
import android.content.SharedPreferences

/**
 * The stream settings that have to outlive [MainActivity]'s spinners.
 *
 * A desktop-initiated start (see [SessionServer]) can arrive while the
 * activity is gone, so it has no spinner selection to read. Rather than give
 * the remote path its own defaults that would silently diverge from what the
 * user last chose on the phone, [MainActivity.startStream] records the
 * selection here on every local start and both paths launch from the same
 * record.
 *
 * Shares the existing `telescope` preferences file, which already held
 * [KEY_LOCAL_ONLY].
 */
object StreamPrefs {
    private const val FILE = "telescope"

    const val KEY_LOCAL_ONLY = "local_only"
    private const val KEY_CAMERA_ID = "last_camera_id"
    private const val KEY_LOGICAL_ID = "last_logical_id"
    private const val KEY_WIDTH = "last_width"
    private const val KEY_HEIGHT = "last_height"
    private const val KEY_OIS = "last_ois"

    /** One remembered "start the stream like this" selection. */
    data class Selection(
        val cameraId: String,
        val logicalId: String,
        val width: Int,
        val height: Int,
        val ois: Boolean,
    )

    fun of(context: Context): SharedPreferences =
        context.getSharedPreferences(FILE, Context.MODE_PRIVATE)

    fun localOnly(context: Context): Boolean =
        of(context).getBoolean(KEY_LOCAL_ONLY, false)

    fun setLocalOnly(context: Context, value: Boolean) {
        of(context).edit().putBoolean(KEY_LOCAL_ONLY, value).apply()
    }

    fun saveSelection(context: Context, selection: Selection) {
        of(context).edit()
            .putString(KEY_CAMERA_ID, selection.cameraId)
            .putString(KEY_LOGICAL_ID, selection.logicalId)
            .putInt(KEY_WIDTH, selection.width)
            .putInt(KEY_HEIGHT, selection.height)
            .putBoolean(KEY_OIS, selection.ois)
            .apply()
    }

    /** Null until the user has started a stream at least once on this phone;
     *  callers then fall back to [CameraStreamService]'s own extra defaults
     *  rather than guessing a camera id here. */
    fun lastSelection(context: Context): Selection? {
        val p = of(context)
        val cameraId = p.getString(KEY_CAMERA_ID, null) ?: return null
        return Selection(
            cameraId = cameraId,
            logicalId = p.getString(KEY_LOGICAL_ID, "") ?: "",
            width = p.getInt(KEY_WIDTH, 1920),
            height = p.getInt(KEY_HEIGHT, 1080),
            ois = p.getBoolean(KEY_OIS, true),
        )
    }
}
