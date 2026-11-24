package com.telescope

import android.content.pm.ServiceInfo
import android.os.Build
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

class ForegroundTest {
    @Test
    fun cameraAloneWithoutMic() {
        assertEquals(ServiceInfo.FOREGROUND_SERVICE_TYPE_CAMERA, Foreground.types(false))
    }

    @Test
    fun micAddedWhenAllowed() {
        assertEquals(
            ServiceInfo.FOREGROUND_SERVICE_TYPE_CAMERA or ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE,
            Foreground.types(true),
        )
    }

    @Test
    fun micCountsBeforeAndroid11() {
        assertTrue(Foreground.micInForce(Build.VERSION_CODES.Q, false))
    }

    @Test
    fun micNeedsPermissionFromAndroid11() {
        assertFalse(Foreground.micInForce(Build.VERSION_CODES.R, false))
        assertTrue(Foreground.micInForce(Build.VERSION_CODES.R, true))
    }
}
