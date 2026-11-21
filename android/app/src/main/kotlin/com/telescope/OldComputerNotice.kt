package com.telescope

// A computer with an older Telescope knocked on one of the old ports. It can't use this phone until it's updated, so the main
// screen says so for as long as it keeps trying (an older desktop checks every few seconds).
class OldComputerNotice(private val clock: () -> Long = System::currentTimeMillis) {
    @Volatile private var name: String = ""
    @Volatile private var at: Long? = null

    // name is null when the computer's token isn't known (it never paired with this phone)
    fun knocked(name: String?) {
        this.name = name.orEmpty()
        at = clock()
    }

    // The computer's name ("" when unknown) while it is still knocking, else null
    fun current(): String? {
        val last = at ?: return null
        return if (clock() - last <= SHOW_MS) name else null
    }

    companion object {
        const val SHOW_MS = 120_000L
    }
}
