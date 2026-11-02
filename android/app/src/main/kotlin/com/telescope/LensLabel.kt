package com.telescope

// The name of a lens, like "Back ~24mm OIS [auto]". [english] is what goes over the wire in `label` for desktops
// that don't read the separate fields; [local] is for this phone's own screens, in its language.
object LensLabel {
    const val BACK = "back"
    const val FRONT = "front"
    const val EXTERNAL = "external"
    const val PHYSICAL = "physical"
    const val AUTO = "auto"

    fun english(facing: String, focalMm: Int, ois: Boolean, kind: String): String {
        val side = when (facing) { BACK -> "Back"; FRONT -> "Front"; EXTERNAL -> "Ext"; else -> "?" }
        val tail = when (kind) { PHYSICAL -> " [phys]"; AUTO -> " [auto]"; else -> "" }
        return "$side ${if (focalMm > 0) "~${focalMm}mm" else "?"}${if (ois) " OIS" else ""}$tail"
    }

    fun local(strings: Strings, facing: String, focalMm: Int, ois: Boolean, kind: String): String {
        val side = when (facing) {
            BACK -> strings.get(R.string.lens_back)
            FRONT -> strings.get(R.string.lens_front)
            EXTERNAL -> strings.get(R.string.lens_external)
            else -> "?"
        }
        val focal = if (focalMm > 0) strings.get(R.string.lens_focal, focalMm) else "?"
        // The pills leave out the "[phys]" mark: it is only in the diagnostics
        return listOfNotNull(side, focal, if (ois) "OIS" else null,
            if (kind == AUTO) strings.get(R.string.lens_auto) else null).joinToString(" ")
    }

    // The preview's lens tile, split like the desktop's (split_lens_label): the focal length, then the rest under it
    fun tile(strings: Strings, facing: String, focalMm: Int, ois: Boolean, kind: String): Pair<String, String> {
        val side = when (facing) {
            BACK -> strings.get(R.string.lens_back)
            FRONT -> strings.get(R.string.lens_front)
            EXTERNAL -> strings.get(R.string.lens_external)
            else -> "?"
        }
        val focal = if (focalMm > 0) strings.get(R.string.lens_focal, focalMm) else "?"
        return focal to listOfNotNull(side, if (ois) "OIS" else null,
            if (kind == AUTO) strings.get(R.string.lens_auto) else null).joinToString(" · ")
    }
}
