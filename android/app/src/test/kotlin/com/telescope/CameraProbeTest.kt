package com.telescope

import java.io.File
import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executors
import java.util.concurrent.RejectedExecutionException
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicReference
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNotNull
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir

class CameraProbeTest {

    @Test
    fun `orders main back lens first, then back, physical back, front and external`() {
        val lenses = listOf(
            ProbeLens("8", null, "Ext ~35mm", LensLabel.EXTERNAL, 35, false, "", 2_000_000L),
            ProbeLens("1", null, "Front ~26mm", LensLabel.FRONT, 26, false, "", 10_000_000L),
            ProbeLens("5", "0", "Back ~13mm [phys]", LensLabel.BACK, 13, false, LensLabel.PHYSICAL, 16_000_000L),
            ProbeLens("6", "0", "Back ~26mm OIS [phys]", LensLabel.BACK, 26, true, LensLabel.PHYSICAL, 12_000_000L),
            ProbeLens("10", null, "Back ~70mm OIS", LensLabel.BACK, 70, true, "", 12_000_000L),
            ProbeLens("2", null, "Back ~13mm", LensLabel.BACK, 13, false, "", 16_000_000L),
            ProbeLens("0", null, "Back ~26mm OIS [auto]", LensLabel.BACK, 26, true, LensLabel.AUTO, 12_000_000L),
            ProbeLens("7", "1", "Front ~22mm [phys]", LensLabel.FRONT, 22, false, LensLabel.PHYSICAL, 8_000_000L),
        )

        val ordered = ProbePlan.orderLenses(lenses)
        assertEquals(listOf("0", "2", "10", "6", "5", "1", "7", "8"), ordered.map { it.id })
    }

    @Test
    fun `picks standard focal length with OIS first when camera id 0 is not back-facing`() {
        val lenses = listOf(
            ProbeLens("0", null, "Front ~24mm", LensLabel.FRONT, 24, false, "", 12_000_000L),
            ProbeLens("1", null, "Back ~13mm", LensLabel.BACK, 13, false, "", 16_000_000L),
            ProbeLens("3", null, "Back ~52mm OIS", LensLabel.BACK, 52, true, "", 12_000_000L),
            ProbeLens("2", null, "Back ~25mm OIS", LensLabel.BACK, 25, true, "", 12_000_000L),
        )

        val ordered = ProbePlan.orderLenses(lenses)
        assertEquals(listOf("2", "1", "3", "0"), ordered.map { it.id })
    }

    @Test
    fun `bounds exposure steps by both sensor range and max frame duration`() {
        // Sensor claims 30 s max exposure, but max frame duration caps it at 10 s.
        val steps = ProbePlan.exposureSteps(
            minExposureNs = 100_000L,
            maxExposureNs = 30_000_000_000L,
            maxFrameDurationNs = 10_000_000_000L,
            minFrameDurationNs = 33_333_333L,
        )
        assertEquals(
            listOf(
                33_333_333L,
                125_000_000L,
                500_000_000L,
                1_000_000_000L,
                2_000_000_000L,
                4_000_000_000L,
                8_000_000_000L,
                10_000_000_000L,
            ),
            steps.map { it.askedExposureNs },
        )
        // Timeout is twice the frame duration plus 2 s.
        val last = steps.last()
        assertEquals(10_000_000_000L, last.askedFrameDurationNs)
        assertEquals(22_000L, last.timeoutMs)

        // When sensor max is tighter than max frame duration, sensor max wins and adds a capped ceiling step.
        val shortSteps = ProbePlan.exposureSteps(
            minExposureNs = 100_000L,
            maxExposureNs = 350_000_000L,
            maxFrameDurationNs = 1_000_000_000L,
            minFrameDurationNs = 50_000_000L,
        )
        assertEquals(listOf(33_333_333L, 125_000_000L, 350_000_000L), shortSteps.map { it.askedExposureNs })
        assertEquals(50_000_000L, shortSteps.first().askedFrameDurationNs)
    }

    @Test
    fun `detects clamping from echoed exposure or real timestamp gap`() {
        assertFalse(ProbePlan.isExposureClamped(1_000_000_000L, 995_000_000L, 1_005_000_000L))
        assertTrue(ProbePlan.isExposureClamped(1_000_000_000L, 500_000_000L, 1_000_000_000L))
        assertTrue(ProbePlan.isExposureClamped(1_000_000_000L, 1_000_000_000L, 500_000_000L))
    }

    @Test
    fun `matches results and images by exact timestamp in either arrival order`() {
        val matcher = TimestampMatcher<String>(maxPending = 4)

        // Result before image
        assertNull(matcher.onResult(1_000_000_000L, "frame1"))
        val m1 = matcher.onImage(1_000_000_000L)
        assertNotNull(m1)
        assertEquals("frame1", m1!!.payload)

        // Image before result, interleaved with another frame
        assertNull(matcher.onImage(3_000_000_000L))
        assertNull(matcher.onResult(2_000_000_000L, "frame2"))
        val m3 = matcher.onResult(3_000_000_000L, "frame3")
        val m2 = matcher.onImage(2_000_000_000L)
        assertEquals("frame3", m3!!.payload)
        assertEquals("frame2", m2!!.payload)

        // Dropped image leaves an unmatched result
        assertNull(matcher.onResult(4_000_000_000L, "dropped"))
        assertEquals(3, matcher.matched.size)
        assertEquals(3, matcher.imagesSeen)
        assertEquals(4, matcher.resultsSeen)
        assertEquals(1, matcher.unmatchedResultsCount)
        assertEquals(0, matcher.unmatchedImagesCount)

        // Sensor gaps and FPS are computed from sorted matched timestamps
        assertEquals(listOf(1_000_000_000L, 1_000_000_000L), matcher.timestampGapsNs())
        assertEquals(1.0, matcher.sensorFps()!!, 1e-6)
    }

    @Test
    fun `evicts oldest unmatched entries when pending queue exceeds bound`() {
        val matcher = TimestampMatcher<Int>(maxPending = 2)
        matcher.onResult(10L, 1)
        matcher.onResult(20L, 2)
        matcher.onResult(30L, 3)

        assertEquals(1, matcher.evictedResults)
        assertEquals(3, matcher.unmatchedResultsCount)
        // Oldest entry (10L) was evicted so a late image does not match it.
        assertNull(matcher.onImage(10L))
        assertNotNull(matcher.onImage(20L))
        assertNotNull(matcher.onImage(30L))
        assertEquals(2, matcher.matched.size)
    }

    @Test
    fun `filters vendor keys and keyword matches and chooses least processing modes`() {
        val keys = listOf(
            "android.sensor.exposureTime",
            "android.control.aeMode",
            "com.samsung.sensor.longShutter",
            "com.vendor.camera.nightMode",
            "com.vendor.camera.fooBar",
            "android.control.aeExposureCompensation",
        )
        val selected = ProbePlan.selectKeys(keys)
        assertEquals(
            listOf("com.samsung.sensor.longShutter", "com.vendor.camera.fooBar", "com.vendor.camera.nightMode"),
            selected.vendorKeys,
        )
        assertEquals(
            listOf(
                "android.control.aeExposureCompensation",
                "android.sensor.exposureTime",
                "com.samsung.sensor.longShutter",
                "com.vendor.camera.nightMode",
            ),
            selected.keywordKeys,
        )

        assertEquals(0, ProbePlan.leastNrMode(listOf(1, 2, 0, 3)))
        assertEquals(3, ProbePlan.leastNrMode(listOf(1, 2, 3)))
        assertEquals(1, ProbePlan.leastNrMode(listOf(1, 2)))
        assertEquals(0, ProbePlan.leastEdgeMode(listOf(2, 1, 0)))
        assertEquals(1, ProbePlan.leastEdgeMode(listOf(2, 1)))
    }

    @Test
    fun `builds a full report with all planned sections and no em or en dashes`() {
        val main = ProbeLens("0", null, "Back ~26mm OIS [auto]", LensLabel.BACK, 26, true, LensLabel.AUTO)
        val phys = ProbeLens("5", "0", "Back ~13mm [phys]", LensLabel.BACK, 13, false, LensLabel.PHYSICAL)
        val acc = ProbeSessionAccumulator(
            header = "Telescope diagnostics\nApp version: 5.1.0-dev.1 (build 1)\nDevice: Test Phone, Android 15 (SDK 35)\n",
            startedAtMs = 1_700_000_000_000L,
            initialThermal = ProbeThermalSnapshot(status = 0, headroom = 0.25f, batteryTempC = 30.5),
            orderedLenses = listOf(main, phys),
        )

        acc.beginLens(main)
        acc.setCharacteristics(
            CharacteristicsReport(
                hwLevel = "FULL",
                exposureMinNs = 100_000L,
                exposureMaxNs = 30_000_000_000L,
                maxFrameDurationNs = 30_000_000_000L,
                effectiveExposureMaxNs = 30_000_000_000L,
                isoMin = 50,
                isoMax = 3200,
                maxAnalogIso = 800,
                postRawBoostMin = 100,
                postRawBoostMax = 400,
                aeFpsRanges = listOf("15-30", "30-30"),
                highSpeedModes = listOf("1920x1080 at 30-120 120-120"),
                capabilities = listOf("MANUAL_SENSOR", "RAW"),
                timestampSource = "REALTIME",
                minFocusDistance = 10.0f,
                physicalCameraIds = listOf("5"),
            ),
        )
        acc.setDurations(
            listOf(
                FormatDurationEntry("YUV_420_888", "largest", ProbeSize(4000, 3000), 33_333_333L, 0L),
                FormatDurationEntry("RAW_SENSOR", "1080p", null, null, null, "1920x1080 unavailable"),
            ),
        )
        acc.beginExposureSweep(ProbeSize(640, 480))
        acc.addExposureStep(
            ExposureStepResult(
                askedExposureNs = 1_000_000_000L,
                askedFrameDurationNs = 1_000_000_000L,
                iso = 100,
                outcome = ProbeOutcome.OK,
                matchedFrames = 3,
                imagesSeen = 3,
                resultsSeen = 3,
                echoedExposuresNs = listOf(1_000_000_000L, 1_000_000_000L, 1_000_000_000L),
                echoedFrameDurationsNs = listOf(1_000_000_000L, 1_000_000_000L, 1_000_000_000L),
                timestampGapsNs = listOf(1_000_000_000L, 1_000_000_000L),
            ),
        )
        acc.addExposureStep(
            ExposureStepResult(
                askedExposureNs = 2_000_000_000L,
                askedFrameDurationNs = 2_000_000_000L,
                iso = 100,
                outcome = ProbeOutcome.CLAMPED,
                matchedFrames = 3,
                imagesSeen = 3,
                resultsSeen = 3,
                echoedExposuresNs = listOf(1_000_000_000L, 1_000_000_000L, 1_000_000_000L),
                echoedFrameDurationsNs = listOf(1_000_000_000L, 1_000_000_000L, 1_000_000_000L),
                timestampGapsNs = listOf(1_000_000_000L, 1_000_000_000L),
            ),
        )
        acc.finishExposureSweep(ProbeOutcome.CLAMPED, ProbeSize(640, 480), stopReason = "clamped at 2.000s")
        acc.addBurst(
            BurstFormatResult(
                formatName = "YUV_420_888",
                size = ProbeSize(4000, 3000),
                outcome = ProbeOutcome.OK,
                elapsedMs = 3_000L,
                imagesSeen = 30,
                resultsSeen = 30,
                matchedFrames = 30,
                bufferLost = 1,
                captureFailed = 0,
                unmatchedResults = 0,
                sensorFps = 10.0,
                wallFps = 10.0,
            ),
        )
        acc.addBurst(
            BurstFormatResult(
                formatName = "RAW_SENSOR",
                size = null,
                outcome = ProbeOutcome.UNSUPPORTED,
                elapsedMs = 0L,
                imagesSeen = 0,
                resultsSeen = 0,
                matchedFrames = 0,
                bufferLost = 0,
                captureFailed = 0,
                unmatchedResults = 0,
                sensorFps = null,
                wallFps = null,
                detail = "no RAW capability",
            ),
        )
        acc.setVendorKeys(
            VendorKeysReport(
                requestKeys = VendorKeySelection(listOf("com.vendor.longShutter"), listOf("com.vendor.longShutter")),
                resultKeys = VendorKeySelection(emptyList(), emptyList()),
                sessionKeys = VendorKeySelection(emptyList(), emptyList()),
                extensions = listOf(ExtensionEntry("NIGHT(4)", ProbeSize(3840, 2160), null)),
            ),
        )
        acc.setProcessing(
            ProcessingReport(
                advertisedNr = listOf(0, 1, 2),
                advertisedEdge = listOf(0, 1),
                leastNr = 0,
                leastEdge = 0,
                checks = listOf(
                    ProcessingCheckEntry("JPEG", ProbeSize(4000, 3000), "OFF", 0, 0, ProbeOutcome.OK, 1, 0),
                ),
            ),
        )
        acc.setSessionChecks(
            SessionChecksReport(
                checks = listOf(
                    SessionCheckEntry("stream(PRIV 1920x1080) + full YUV(4000x3000)", ProbeOutcome.OK, "supported"),
                    SessionCheckEntry("stream(PRIV 1920x1080) + full RAW(none)", ProbeOutcome.UNSUPPORTED, "no RAW capability"),
                ),
            ),
        )
        acc.finishLens(ProbeOutcome.OK)

        acc.beginLens(phys)
        acc.setSessionChecks(SessionChecksReport(skippedReason = "requires Android 10"))
        acc.finishExposureSweep(ProbeOutcome.TIMEOUT, ProbeSize(640, 480), detail = "no matched frame within 4000 ms")
        acc.finishLens(ProbeOutcome.OK)

        val data = acc.snapshot(
            status = ProbeRunStatus.COMPLETED,
            stopReason = null,
            finishedAtMs = 1_700_000_065_000L,
            finalThermal = ProbeThermalSnapshot(status = 1, headroom = 0.45f, batteryTempC = 34.0),
        )
        val report = CameraProbeReport.format(data)

        assertTrue(report.contains("Report type: Camera2 probe"))
        assertTrue(report.contains("Lenses completed: 2/2"))
        assertTrue(report.contains("== Lens 1/2: id=0 (Back ~26mm OIS [auto]) =="))
        assertTrue(report.contains("== Lens 2/2: id=5 via logical 0 (Back ~13mm [phys]) =="))
        assertTrue(report.contains("RAW_SENSOR 1080p: skipped (1920x1080 unavailable)"))
        assertTrue(report.contains("clamped"))
        assertTrue(report.contains("unsupported"))
        assertTrue(report.contains("timeout"))
        assertTrue(report.contains("vendor key names alone do not establish a wider usable exposure range"))
        assertTrue(report.contains("echoed modes describe reported settings and cannot prove pixel processing"))
        assertFalse(report.contains("\u2014"))
        assertFalse(report.contains("\u2013"))
    }

    @Test
    fun `cancellation drives real runner, writes interrupted checkpoint, keeps partial lens and releases ownership`(
        @TempDir tempDir: File,
    ) {
        val lock = CameraOwnerLock(clock = { 1_000L })
        val lens1 = ProbeLens("0", null, "Back ~26mm OIS", LensLabel.BACK, 26, true, "")
        val lens2 = ProbeLens("2", null, "Back ~13mm", LensLabel.BACK, 13, false, "")
        val lens3 = ProbeLens("1", null, "Front ~24mm", LensLabel.FRONT, 24, false, "")
        val cacheFile = File(tempDir, CameraProbeCache.FILE_NAME)

        val enteredLens2 = CountDownLatch(1)
        val finishedLatch = CountDownLatch(1)
        val finishedData = AtomicReference<ProbeReportData?>()
        val finishedText = AtomicReference<String?>()
        val savedFlag = AtomicBoolean(false)

        val backend = FakeProbeBackend(
            tempDir = tempDir,
            lenses = listOf(lens3, lens2, lens1),
        ) { lens, acc, host ->
            when (lens.id) {
                "0" -> {
                    acc.addExposureStep(
                        ExposureStepResult(
                            askedExposureNs = 33_333_333L,
                            askedFrameDurationNs = 33_333_333L,
                            iso = 100,
                            outcome = ProbeOutcome.OK,
                            matchedFrames = 3,
                            imagesSeen = 3,
                            resultsSeen = 3,
                            echoedExposuresNs = listOf(33_333_333L),
                            echoedFrameDurationsNs = listOf(33_333_333L),
                            timestampGapsNs = listOf(33_333_333L, 33_333_333L),
                        ),
                    )
                    acc.finishExposureSweep(ProbeOutcome.OK, ProbeSize(640, 480))
                    host.onLensFinished(ProbeOutcome.OK)
                }
                "2" -> {
                    acc.beginExposureSweep(ProbeSize(640, 480))
                    acc.addExposureStep(
                        ExposureStepResult(
                            askedExposureNs = 125_000_000L,
                            askedFrameDurationNs = 125_000_000L,
                            iso = 100,
                            outcome = ProbeOutcome.OK,
                            matchedFrames = 3,
                            imagesSeen = 3,
                            resultsSeen = 3,
                            echoedExposuresNs = listOf(125_000_000L),
                            echoedFrameDurationsNs = listOf(125_000_000L),
                            timestampGapsNs = listOf(125_000_000L, 125_000_000L),
                        ),
                    )
                    enteredLens2.countDown()
                }
            }
        }

        val runner = CameraProbeRunner(
            backend = backend,
            listener = object : CameraProbeRunner.Listener {
                override fun onProgress(progress: ProbeProgress) {}
                override fun onFinished(data: ProbeReportData, reportText: String, savedToCache: Boolean) {
                    finishedData.set(data)
                    finishedText.set(reportText)
                    savedFlag.set(savedToCache)
                    finishedLatch.countDown()
                }
            },
            ownership = lock,
        )

        assertEquals(CameraProbeRunner.StartResult.Started, runner.start())
        assertTrue(enteredLens2.await(5, TimeUnit.SECONDS))

        // While running, probe holds ownership exclusively and the incremental checkpoint uses "interrupted" status.
        assertTrue(lock.isHeldBy(CameraOwner.PROBE))
        assertNull(lock.tryAcquire(CameraOwner.STREAM, adopted = false))
        val checkpointText = awaitCacheContaining(cacheFile, "Status: interrupted")
        assertNotNull(checkpointText)
        assertTrue(checkpointText!!.contains("Status: interrupted"))
        assertFalse(checkpointText.contains("Status: cancelled"))

        runner.cancel("Cancelled by user")
        assertTrue(finishedLatch.await(5, TimeUnit.SECONDS))

        // Ownership is released and a stream can acquire immediately afterwards.
        assertFalse(lock.isHeld())
        val streamLease = lock.tryAcquire(CameraOwner.STREAM, adopted = false)
        assertNotNull(streamLease)
        assertTrue(lock.release(streamLease!!.id))
        assertTrue(backend.closeCount.get() >= 2)
        assertTrue(backend.lastAbort.get())

        val partial = finishedData.get()!!
        assertEquals(ProbeRunStatus.CANCELLED, partial.status)
        assertEquals(1, partial.completedLenses)
        assertEquals(3, partial.totalLenses)
        assertEquals(2, partial.lenses.size)
        assertEquals("0", partial.lenses[0].lens.id)
        assertEquals(ProbeOutcome.OK, partial.lenses[0].status)
        assertEquals("2", partial.lenses[1].lens.id)
        assertEquals(ProbeOutcome.CANCELLED, partial.lenses[1].status)
        assertEquals(1, partial.lenses[1].exposureSweep?.steps?.size)

        // Cached partial report is readable and contains both completed lens 1 and partial lens 2.
        assertTrue(savedFlag.get())
        val cached = CameraProbeCache.read(cacheFile)
        assertEquals(finishedText.get(), cached)
        assertTrue(cached!!.contains("Status: cancelled (Cancelled by user)"))
        assertTrue(cached.contains("Lenses completed: 1/3"))
        assertTrue(cached.contains("id=0"))
        assertTrue(cached.contains("id=2"))
        assertTrue(cached.contains("125.00ms"))
        assertFalse(File(tempDir, "${CameraProbeCache.FILE_NAME}.tmp").exists())
    }

    @Test
    fun `cancel during a pending open keeps ownership until the late callback closes the device`(
        @TempDir tempDir: File,
    ) {
        val lock = CameraOwnerLock(clock = { 1_000L })
        val lens = ProbeLens("0", null, "Back ~26mm", LensLabel.BACK, 26, true, "")
        val opening = CountDownLatch(1)
        val finishedLatch = CountDownLatch(1)
        val token = AtomicReference<Int?>()
        val hostRef = AtomicReference<ProbeLensHost?>()
        lateinit var backend: FakeProbeBackend
        backend = FakeProbeBackend(tempDir, listOf(lens)) { _, _, host ->
            token.set(backend.beginPendingOpen())
            hostRef.set(host)
            opening.countDown()
        }
        val runner = CameraProbeRunner(
            backend = backend,
            listener = object : CameraProbeRunner.Listener {
                override fun onProgress(progress: ProbeProgress) {}
                override fun onFinished(data: ProbeReportData, reportText: String, savedToCache: Boolean) {
                    finishedLatch.countDown()
                }
            },
            ownership = lock,
        )
        assertEquals(CameraProbeRunner.StartResult.Started, runner.start())
        assertTrue(opening.await(5, TimeUnit.SECONDS))

        runner.cancel()
        assertTrue(finishedLatch.await(5, TimeUnit.SECONDS))
        Thread.sleep(100)

        // The open has not reported back yet, so streams must stay locked out.
        assertTrue(lock.isHeldBy(CameraOwner.PROBE))
        assertNull(lock.tryAcquire(CameraOwner.STREAM, adopted = false))

        backend.deliverLateOpen(token.get()!!, hostRef.get()!!.worker)
        val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(5)
        while (lock.isHeld() && System.nanoTime() < deadline) Thread.sleep(10)
        assertEquals(1, backend.lateClosedDevices.get())
        assertFalse(lock.isHeld())
    }

    @Test
    fun `pending open tracker waits for every open and settles at once when idle`() {
        val tracker = PendingOpenTracker()
        var settled = 0
        tracker.whenSettled { settled++ }
        assertEquals(1, settled)

        val a = tracker.begin()
        val b = tracker.begin()
        tracker.whenSettled { settled++ }
        tracker.resolve(a)
        assertEquals(1, settled)
        tracker.resolve(a)
        assertEquals(1, settled)
        tracker.resolve(b)
        assertEquals(2, settled)
    }

    @Test
    fun `a stale or late disconnect does not fail the run`() {
        assertTrue(ProbeOpenPolicy.disconnectIsFatal(runFinished = false, isCurrentLens = true))
        assertFalse(ProbeOpenPolicy.disconnectIsFatal(runFinished = false, isCurrentLens = false))
        assertFalse(ProbeOpenPolicy.disconnectIsFatal(runFinished = true, isCurrentLens = true))
    }

    @Test
    fun `thermal policy refuses hot start and stops mid-run when heat rises`() {
        assertFalse(ProbeThermalPolicy.tooHotToStart(ProbeThermalSnapshot(status = 0, headroom = 0.5f, batteryTempC = 35.0)))
        assertFalse(ProbeThermalPolicy.tooHotToStart(ProbeThermalSnapshot(status = 2, headroom = 0.8f, batteryTempC = 41.9)))
        assertTrue(ProbeThermalPolicy.tooHotToStart(ProbeThermalSnapshot(status = HeatStep.SEVERE, headroom = 0.5f, batteryTempC = 35.0)))
        assertTrue(ProbeThermalPolicy.tooHotToStart(ProbeThermalSnapshot(status = 1, headroom = 1.0f, batteryTempC = 35.0)))
        assertTrue(ProbeThermalPolicy.tooHotToStart(ProbeThermalSnapshot(status = 0, headroom = Float.NaN, batteryTempC = 42.0)))

        assertFalse(ProbeThermalPolicy.tooHotToContinue(ProbeThermalSnapshot(status = 2, headroom = 0.9f, batteryTempC = 43.5)))
        assertTrue(ProbeThermalPolicy.tooHotToContinue(ProbeThermalSnapshot(status = HeatStep.SEVERE, headroom = 0.5f, batteryTempC = 35.0)))
        assertTrue(ProbeThermalPolicy.tooHotToContinue(ProbeThermalSnapshot(status = 0, headroom = Float.NaN, batteryTempC = 44.0)))
    }

    @Test
    fun `unexpected runner failure preserves partial results and releases ownership`(@TempDir tempDir: File) {
        val lock = CameraOwnerLock(clock = { 1_000L })
        val lens1 = ProbeLens("0", null, "Back ~26mm OIS", LensLabel.BACK, 26, true, "")
        val lens2 = ProbeLens("2", null, "Back ~13mm", LensLabel.BACK, 13, false, "")
        val finishedLatch = CountDownLatch(1)
        val finishedData = AtomicReference<ProbeReportData?>()

        val backend = FakeProbeBackend(
            tempDir = tempDir,
            lenses = listOf(lens1, lens2),
        ) { lens, acc, host ->
            when (lens.id) {
                "0" -> host.onLensFinished(ProbeOutcome.OK)
                "2" -> {
                    acc.setDurations(
                        listOf(FormatDurationEntry("JPEG", "largest", ProbeSize(4000, 3000), 33_333_333L, 0L)),
                    )
                    throw IllegalStateException("simulated HAL crash")
                }
            }
        }

        val runner = CameraProbeRunner(
            backend = backend,
            listener = object : CameraProbeRunner.Listener {
                override fun onProgress(progress: ProbeProgress) {}
                override fun onFinished(data: ProbeReportData, reportText: String, savedToCache: Boolean) {
                    finishedData.set(data)
                    finishedLatch.countDown()
                }
            },
            ownership = lock,
        )

        assertEquals(CameraProbeRunner.StartResult.Started, runner.start())
        assertTrue(finishedLatch.await(5, TimeUnit.SECONDS))

        assertFalse(lock.isHeld())
        val streamLease = lock.tryAcquire(CameraOwner.STREAM, adopted = false)
        assertNotNull(streamLease)
        assertTrue(lock.release(streamLease!!.id))
        assertTrue(backend.lastAbort.get())

        val snapshot = finishedData.get()!!
        assertEquals(ProbeRunStatus.FAILED, snapshot.status)
        assertTrue(snapshot.stopReason!!.contains("simulated HAL crash"))
        assertEquals(1, snapshot.completedLenses)
        assertEquals(2, snapshot.lenses.size)
        assertEquals(ProbeOutcome.ERROR, snapshot.lenses[1].status)
        assertEquals(1, snapshot.lenses[1].durations.size)
    }

    @Test
    fun `mid-run thermal stop preserves partial results and releases ownership`(@TempDir tempDir: File) {
        val lock = CameraOwnerLock(clock = { 1_000L })
        val lens1 = ProbeLens("0", null, "Back ~26mm OIS", LensLabel.BACK, 26, true, "")
        val lens2 = ProbeLens("2", null, "Back ~13mm", LensLabel.BACK, 13, false, "")
        val enteredLens2 = CountDownLatch(1)
        val finishedLatch = CountDownLatch(1)
        val finishedData = AtomicReference<ProbeReportData?>()
        val finishedText = AtomicReference<String?>()

        val backend = FakeProbeBackend(
            tempDir = tempDir,
            lenses = listOf(lens1, lens2),
        ) { lens, acc, host ->
            when (lens.id) {
                "0" -> host.onLensFinished(ProbeOutcome.OK)
                "2" -> {
                    acc.setDurations(
                        listOf(FormatDurationEntry("JPEG", "largest", ProbeSize(4000, 3000), 33_333_333L, 0L)),
                    )
                    enteredLens2.countDown()
                }
            }
        }

        val runner = CameraProbeRunner(
            backend = backend,
            listener = object : CameraProbeRunner.Listener {
                override fun onProgress(progress: ProbeProgress) {}
                override fun onFinished(data: ProbeReportData, reportText: String, savedToCache: Boolean) {
                    finishedData.set(data)
                    finishedText.set(reportText)
                    finishedLatch.countDown()
                }
            },
            ownership = lock,
        )

        assertEquals(CameraProbeRunner.StartResult.Started, runner.start())
        assertTrue(enteredLens2.await(5, TimeUnit.SECONDS))
        assertTrue(lock.isHeldBy(CameraOwner.PROBE))

        backend.triggerThermal(ProbeThermalSnapshot(status = HeatStep.SEVERE, batteryTempC = 44.5))
        assertTrue(finishedLatch.await(5, TimeUnit.SECONDS))

        assertFalse(lock.isHeld())
        val streamLease = lock.tryAcquire(CameraOwner.STREAM, adopted = false)
        assertNotNull(streamLease)
        assertTrue(lock.release(streamLease!!.id))
        assertTrue(backend.lastAbort.get())

        val snapshot = finishedData.get()!!
        assertEquals(ProbeRunStatus.THERMAL_STOP, snapshot.status)
        assertEquals(1, snapshot.completedLenses)
        assertEquals(2, snapshot.lenses.size)
        assertEquals(ProbeOutcome.THERMAL_STOP, snapshot.lenses[1].status)
        assertTrue(finishedText.get()!!.contains("Status: thermal_stop"))
    }

    @Test
    fun `session coordinator keeps probe alive across activity detach and reattach`(@TempDir tempDir: File) {
        val lock = CameraOwnerLock(clock = { 1_000L })
        val lens1 = ProbeLens("0", null, "Back ~26mm OIS", LensLabel.BACK, 26, true, "")
        val lens2 = ProbeLens("2", null, "Back ~13mm", LensLabel.BACK, 13, false, "")
        val enteredLens2 = CountDownLatch(1)
        val releaseLens2 = CountDownLatch(1)
        val secondFinished = CountDownLatch(1)
        val finishedResult = AtomicReference<ProbeSessionCoordinator.FinishedResult?>()

        val backend = FakeProbeBackend(
            tempDir = tempDir,
            lenses = listOf(lens1, lens2),
        ) { lens, _, host ->
            when (lens.id) {
                "0" -> host.onLensFinished(ProbeOutcome.OK)
                "2" -> {
                    host.postProgress("Long exposure step")
                    enteredLens2.countDown()
                    Thread {
                        releaseLens2.await(5, TimeUnit.SECONDS)
                        host.worker.post { host.onLensFinished(ProbeOutcome.OK) }
                    }.start()
                }
            }
        }

        val coordinator = ProbeSessionCoordinator(
            monoClock = { 5_000L },
            runnerFactory = { listener -> CameraProbeRunner(backend = backend, listener = listener, ownership = lock) },
        )

        val firstListener = object : ProbeSessionCoordinator.Listener {
            override fun onProgress(progress: ProbeProgress) {}
            override fun onFinished(result: ProbeSessionCoordinator.FinishedResult) {}
        }
        coordinator.attach(firstListener)
        assertEquals(CameraProbeRunner.StartResult.Started, coordinator.start())
        assertTrue(enteredLens2.await(5, TimeUnit.SECONDS))

        // Simulate Activity recreation: old instance detaches, probe stays running and keeps ownership.
        coordinator.detach(firstListener)
        assertTrue(coordinator.isRunning())
        assertTrue(lock.isHeldBy(CameraOwner.PROBE))

        // Recreated Activity attaches and sees live running state plus last progress.
        val secondListener = object : ProbeSessionCoordinator.Listener {
            override fun onProgress(progress: ProbeProgress) {}
            override fun onFinished(result: ProbeSessionCoordinator.FinishedResult) {
                finishedResult.set(result)
                secondFinished.countDown()
            }
        }
        val restored = coordinator.attach(secondListener)
        assertTrue(restored.running)
        assertFalse(restored.cancelling)
        assertEquals(5_000L, restored.startedMonoMs)
        assertNotNull(restored.lastProgress)

        releaseLens2.countDown()
        assertTrue(secondFinished.await(5, TimeUnit.SECONDS))
        assertFalse(coordinator.isRunning())
        assertFalse(lock.isHeld())
        assertEquals(ProbeRunStatus.COMPLETED, finishedResult.get()!!.data.status)
        assertEquals(2, finishedResult.get()!!.data.completedLenses)
    }

    private fun awaitCacheContaining(file: File, needle: String): String? {
        val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(2)
        while (System.nanoTime() < deadline) {
            val content = CameraProbeCache.read(file)
            if (content != null && content.contains(needle)) return content
            Thread.sleep(10)
        }
        return CameraProbeCache.read(file)
    }

    private class FakeProbeWorker(
        private val onUnhandledError: (Throwable) -> Unit,
    ) : ProbeWorker {
        private val executor = Executors.newSingleThreadScheduledExecutor { r ->
            Thread(r, "fake-probe-worker").apply { isDaemon = true }
        }

        override fun post(block: () -> Unit): Boolean {
            return try {
                executor.execute {
                    try {
                        block()
                    } catch (t: Throwable) {
                        onUnhandledError(t)
                    }
                }
                true
            } catch (_: RejectedExecutionException) {
                false
            }
        }

        override fun postDelayed(delayMs: Long, block: () -> Unit): ProbeCancellable {
            return try {
                val future = executor.schedule(
                    {
                        try {
                            block()
                        } catch (t: Throwable) {
                            onUnhandledError(t)
                        }
                    },
                    delayMs.coerceAtLeast(0L),
                    TimeUnit.MILLISECONDS,
                )
                ProbeCancellable { future.cancel(false) }
            } catch (_: RejectedExecutionException) {
                ProbeCancellable {}
            }
        }

        override fun quit() {
            executor.shutdown()
        }
    }

    private class FakeProbeBackend(
        private val tempDir: File,
        private val lenses: List<ProbeLens>,
        private val onProbeLens: (ProbeLens, ProbeSessionAccumulator, ProbeLensHost) -> Unit,
    ) : CameraProbeBackend {
        @Volatile
        var thermal = ProbeThermalSnapshot(status = 0, batteryTempC = 31.0)

        @Volatile
        private var thermalHandler: ((ProbeThermalSnapshot) -> Unit)? = null

        val closeCount = AtomicInteger(0)
        val lastAbort = AtomicBoolean(false)

        override val betweenLensesMs: Long = 0L
        override fun hasCameraPermission(): Boolean = true
        override fun isStreamActive(): Boolean = false

        override fun readThermalSnapshot(overrideStatus: Int?): ProbeThermalSnapshot =
            if (overrideStatus != null) thermal.copy(status = overrideStatus) else thermal

        override fun registerThermalMonitor(
            worker: ProbeWorker,
            onHot: (ProbeThermalSnapshot) -> Unit,
        ): ProbeCancellable {
            thermalHandler = onHot
            return ProbeCancellable { thermalHandler = null }
        }

        fun triggerThermal(snapshot: ProbeThermalSnapshot) {
            thermal = snapshot
            thermalHandler?.invoke(snapshot)
        }

        override fun reportHeader(): String = "Telescope diagnostics\n"
        override fun wallTimeMs(): Long = System.currentTimeMillis()
        override fun monoTimeMs(): Long = System.nanoTime() / 1_000_000L
        override fun cacheFile(): File = File(tempDir, CameraProbeCache.FILE_NAME)
        override fun logWarn(message: String, t: Throwable?) {}
        override fun createWorker(onUnhandledError: (Throwable) -> Unit): ProbeWorker = FakeProbeWorker(onUnhandledError)
        override fun postMain(block: () -> Unit) = block()
        override fun enumerateLenses(): List<ProbeLens> = lenses

        override fun probeLens(lens: ProbeLens, acc: ProbeSessionAccumulator, host: ProbeLensHost) {
            onProbeLens(lens, acc, host)
        }

        override fun closeActive(abort: Boolean) {
            closeCount.incrementAndGet()
            lastAbort.set(abort)
        }

        private val tracker = PendingOpenTracker()
        val lateClosedDevices = AtomicInteger(0)

        fun beginPendingOpen(): Int = tracker.begin()

        // Mirrors a late onOpened: it arrives on the worker thread and the device gets closed.
        fun deliverLateOpen(token: Int, worker: ProbeWorker) {
            worker.post {
                lateClosedDevices.incrementAndGet()
                tracker.resolve(token)
            }
        }

        override fun whenOpensSettled(onSettled: () -> Unit) = tracker.whenSettled(onSettled)
    }

    @Test
    fun `picks small YUV and stream sizes and truncates long report previews`() {
        val sizes = listOf(
            ProbeSize(4000, 3000),
            ProbeSize(1920, 1080),
            ProbeSize(1280, 720),
            ProbeSize(640, 480),
            ProbeSize(320, 240),
        )
        assertEquals(ProbeSize(640, 480), ProbePlan.pickSmallYuvSize(sizes))
        assertEquals(ProbeSize(1920, 1080), ProbePlan.pickStreamSize(sizes))
        assertEquals(ProbeSize(1280, 720), ProbePlan.pickStreamSize(sizes.filter { it != ProbeSize(1920, 1080) }))

        assertEquals("short", CameraProbeReport.preview("short", maxChars = 10))
        assertEquals("0123456789\n...", CameraProbeReport.preview("0123456789abcdef", maxChars = 10))
    }
}
