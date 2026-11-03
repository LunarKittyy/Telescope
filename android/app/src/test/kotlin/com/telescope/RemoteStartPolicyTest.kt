package com.telescope

import androidx.core.app.NotificationManagerCompat
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

class RemoteStartPolicyTest {
    @Test
    fun refusesInTheBackgroundWithoutNotifications() {
        assertFalse(RemoteStartPolicy.allowed(foreground = false, notificationsShown = false))
    }

    @Test
    fun allowsWithNotificationsOrInTheForeground() {
        assertTrue(RemoteStartPolicy.allowed(foreground = false, notificationsShown = true))
        assertTrue(RemoteStartPolicy.allowed(foreground = true, notificationsShown = false))
        assertTrue(RemoteStartPolicy.allowed(foreground = true, notificationsShown = true))
    }

    @Test
    fun aDisabledChannelCountsAsNoNotification() {
        assertFalse(RemoteStartPolicy.channelShown(NotificationManagerCompat.IMPORTANCE_NONE))
        assertTrue(RemoteStartPolicy.channelShown(NotificationManagerCompat.IMPORTANCE_LOW))
        assertTrue(RemoteStartPolicy.channelShown(null))
        assertFalse(RemoteStartPolicy.allowed(foreground = false, notificationsShown = RemoteStartPolicy.channelShown(0)))
    }

    @Test
    fun refusalUsesTheCodeTheDesktopKnows() {
        assertEquals("start_refused", RemoteStartPolicy.REFUSED)
    }
}
