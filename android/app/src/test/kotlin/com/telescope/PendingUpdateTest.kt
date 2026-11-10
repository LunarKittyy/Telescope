package com.telescope

import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

class PendingUpdateTest {
    @Test fun installsWhenAllowedAndIdle() = assertTrue(shouldResumePendingUpdate(true, true, false))
    @Test fun waitsWhileStreaming() = assertFalse(shouldResumePendingUpdate(true, true, true))
    @Test fun waitsForPermission() = assertFalse(shouldResumePendingUpdate(true, false, false))
    @Test fun nothingPending() = assertFalse(shouldResumePendingUpdate(false, true, false))
}
