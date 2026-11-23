package com.telescope

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Test

class DiagnosticsExtrasTest {
    @Test fun rawNoteNamesTheSizeWhenTheLensHasRaw() =
        assertEquals(", RAW 4000x3000", DiagnosticsExtras.rawNote(true, 4000 to 3000))

    @Test fun rawNoteSaysNoRawWhenReadButMissing() = assertEquals(", no RAW", DiagnosticsExtras.rawNote(true, null))

    @Test fun rawNoteIsEmptyWhenTheLensCouldNotBeRead() = assertEquals("", DiagnosticsExtras.rawNote(false, null))

    @Test fun encodersListHardwareBeforeSoftwarePerCodec() = assertEquals(
        "Video encoders: H.264 hw c2.qti.avc.encoder; H.264 sw c2.android.avc.encoder; HEVC hw c2.qti.hevc.encoder",
        DiagnosticsExtras.encoderLine(listOf(
            EncoderEntry("HEVC", "c2.qti.hevc.encoder", true),
            EncoderEntry("H.264", "c2.android.avc.encoder", false),
            EncoderEntry("H.264", "c2.qti.avc.encoder", true),
        )),
    )

    @Test fun noEncodersIsSaidPlainly() = assertEquals("Video encoders: none found", DiagnosticsExtras.encoderLine(emptyList()))
}
