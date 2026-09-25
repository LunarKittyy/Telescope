package com.telescope

// The first-run setup card's rules, kept free of Android types so they're JVM-tested.
object SetupSteps {
    enum class Action { NONE, ASK, SETTINGS }

    // What a step's button does. Android shows its own prompt before the first ask and again while
    // shouldShowRequestPermissionRationale is true; after a second "Don't allow" only the app's
    // settings page can grant it.
    fun action(granted: Boolean, askedBefore: Boolean, showRationale: Boolean): Action = when {
        granted -> Action.NONE
        !askedBefore || showRationale -> Action.ASK
        else -> Action.SETTINGS
    }

    // The step whose button is the filled one: the first not done yet, or -1 when all are.
    fun current(done: List<Boolean>): Int = done.indexOfFirst { !it }
}
