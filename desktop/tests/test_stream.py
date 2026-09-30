import threading

import numpy as np

import telescope.stream as stream
import telescope.vcam as vcam


class _Capture:
    def __init__(self, frames=(), opened=True, last_frame_bytes=0):
        self.frames = list(frames)
        self.opened = opened
        self.released = False
        # Mirrors MjpegReader.last_frame_bytes; StreamWorker reads after read() to accumulate throughput.
        self.last_frame_bytes = last_frame_bytes

    def isOpened(self):
        return self.opened

    def read(self):
        if self.frames:
            return self.frames.pop(0)
        return False, None

    def read_packet(self):
        return self.read()

    @staticmethod
    def decode(frame):
        return frame

    def release(self):
        self.released = True


def test_fit_frame_returns_matching_frame_without_copy():
    frame = np.ones((3, 5, 3), dtype=np.uint8)

    result = stream._fit_frame(frame, 5, 3)

    assert result is frame


def test_fit_frame_resizes_same_aspect_ratio():
    frame = np.full((2, 4, 3), 17, dtype=np.uint8)

    result = stream._fit_frame(frame, 8, 4)

    assert result.shape == (4, 8, 3)
    assert np.all(result == 17)


def test_fit_frame_letterboxes_and_preserves_dtype():
    frame = np.full((2, 4, 3), 255, dtype=np.uint16)

    result = stream._fit_frame(frame, 4, 4)

    assert result.shape == (4, 4, 3)
    assert result.dtype == np.uint16
    assert np.all(result[0] == 0)
    assert np.all(result[1:3] == 255)
    assert np.all(result[3] == 0)


def test_fit_frame_pillarboxes_narrow_input():
    frame = np.full((4, 2, 3), 9, dtype=np.uint8)

    result = stream._fit_frame(frame, 4, 4)

    assert np.all(result[:, 0] == 0)
    assert np.all(result[:, 1:3] == 9)
    assert np.all(result[:, 3] == 0)


def test_worker_processes_pipeline_in_order():
    calls = []

    def first(frame):
        calls.append("first")
        return frame + 2

    def second(frame):
        calls.append("second")
        return frame * 3

    worker = stream.StreamWorker("url", None, None, 30, [first, second])

    result = worker._process(np.array([1]))

    assert calls == ["first", "second"]
    assert result.tolist() == [9]


def test_update_output_distinguishes_omitted_from_pass_through():
    worker = stream.StreamWorker("url", 1280, 720, 30)

    worker.update_output(width=None)

    assert worker._width is None
    assert worker._height == 720
    assert worker._fps == 30
    assert not worker._restart_vcam.is_set()

    worker.update_output(fps=60)
    assert worker._fps == 60
    assert worker._restart_vcam.is_set()


def test_the_same_fps_again_does_not_reopen_the_virtual_camera():
    worker = stream.StreamWorker("url", 1280, 720, 30)
    worker.update_output(fps=30)
    assert not worker._restart_vcam.is_set()


def test_request_stop_sets_both_stop_signals():
    worker = stream.StreamWorker("url", None, None, 30)

    worker.request_stop()

    assert worker._stop_flag is True
    assert worker._restart_vcam.is_set()


def test_open_cap_constructs_authenticated_reader_and_opens(monkeypatch):
    calls = []

    class _FakeReader:
        def __init__(self, url, auth):
            calls.append((url, auth))
            self.opened = False

        def open(self):
            self.opened = True
            return True

        def isOpened(self):
            return self.opened

    monkeypatch.setattr(stream, "MjpegReader", _FakeReader)
    worker = stream.StreamWorker("http://phone/v1/video", None, None, 30, auth="tok123")

    reader = worker._open_cap()

    assert calls == [("http://phone/v1/video", "tok123")]
    assert reader.opened is True


def test_reconnect_returns_first_capture_with_a_readable_frame(monkeypatch):
    bad = _Capture(opened=False)
    empty = _Capture([(False, None)])
    good = _Capture([(True, np.zeros((1, 1, 3), dtype=np.uint8))])
    captures = iter([bad, empty, good])
    worker = stream.StreamWorker("url", None, None, 30)
    monkeypatch.setattr(stream, "RECONNECT_DELAY", 0)
    monkeypatch.setattr(worker, "_open_cap", lambda: next(captures))

    result = worker._reconnect_cap(threading.Event())

    assert result is good
    assert bad.released is True
    assert empty.released is True
    assert good.released is False


def test_retarget_reconnects_to_the_new_url_without_waiting_out_the_delay(monkeypatch):
    good = _Capture([(True, np.zeros((1, 1, 3), dtype=np.uint8))])
    worker = stream.StreamWorker("http://127.0.0.1:41000/v1/video", None, None, 30)
    opened = []
    monkeypatch.setattr(worker, "_open_cap", lambda: opened.append(worker.url) or good)
    slept = []
    monkeypatch.setattr(stream.time, "sleep", slept.append)

    worker.retarget("http://10.0.0.5:8080/v1/video")

    assert worker._reconnect_cap(threading.Event()) is good
    assert opened == ["http://10.0.0.5:8080/v1/video"]
    assert slept == []


def test_reconnect_stops_without_opening_when_cancelled(monkeypatch):
    worker = stream.StreamWorker("url", None, None, 30)
    stop = threading.Event()
    stop.set()
    monkeypatch.setattr(worker, "_open_cap", lambda: (_ for _ in ()).throw(AssertionError()))

    assert worker._reconnect_cap(stop) is None


def test_stream_reader_resizes_and_runs_pipeline_keeping_bgr():
    # The virtual camera takes BGR, so the decoder's order goes through untouched.
    raw = np.tile(np.array([[[1, 2, 3]]], dtype=np.uint8), (2, 2, 1))
    cap = _Capture([(True, raw)])
    worker = stream.StreamWorker("url", 4, 3, 30, [lambda frame: frame + 1])

    # Second read fails and reconnect exits due to _stop_flag.
    def no_reconnect(_stop):
        worker._stop_flag = True
        return None

    worker._reconnect_cap = no_reconnect
    worker._stream_reader(cap, threading.Event(), threading.Event())

    assert cap.released is True
    assert worker._latest.shape == (3, 4, 3)
    assert worker._latest[0, 0].tolist() == [2, 3, 4]


def test_stream_reader_accumulates_bytes_from_successful_reads():
    raw = np.zeros((2, 2, 3), dtype=np.uint8)
    cap = _Capture([(True, raw), (True, raw)], last_frame_bytes=12_345)
    worker = stream.StreamWorker("url", None, None, 30)

    def no_reconnect(_stop):
        worker._stop_flag = True
        return None

    worker._reconnect_cap = no_reconnect
    worker._stream_reader(cap, threading.Event(), threading.Event())

    assert worker._bytes_total == 2 * 12_345


def test_stream_reader_drops_pipeline_errors_and_releases_capture():
    raw = np.zeros((2, 2, 3), dtype=np.uint8)
    cap = _Capture([(True, raw)])
    worker = stream.StreamWorker(
        "url", None, None, 30,
        [lambda _frame: (_ for _ in ()).throw(RuntimeError("bad transform"))],
    )
    worker._stop_flag = False

    def stop_after_error():
        worker._stop_flag = True

    # Next failed read invokes reconnect; use that to stop.
    worker._reconnect_cap = lambda _event: stop_after_error()
    worker._stream_reader(cap, threading.Event(), threading.Event())

    assert worker._latest is None
    assert cap.released is True


def test_stream_reader_emits_reconnected_signal_on_successful_reconnect():
    raw = np.zeros((2, 2, 3), dtype=np.uint8)
    first_cap = _Capture([(False, None)])
    second_cap = _Capture([(True, raw)])
    worker = stream.StreamWorker("url", None, None, 30)
    reconnected = []
    worker.reconnected.connect(lambda: reconnected.append(True))

    reconnect_calls = []

    def fake_reconnect(_stop):
        reconnect_calls.append(True)
        if len(reconnect_calls) == 1:
            return second_cap
        worker._stop_flag = True
        return None

    worker._reconnect_cap = fake_reconnect
    worker._stream_reader(first_cap, threading.Event(), threading.Event())

    assert reconnected == [True]
    assert first_cap.released is True
    assert second_cap.released is True


def test_run_streams_a_frame_and_stops_cleanly(monkeypatch):
    frame = np.full((2, 4, 3), [10, 20, 30], dtype=np.uint8)
    cap = _Capture([(True, frame)] + [(True, frame)] * 20)
    worker = stream.StreamWorker(
        "url", None, None, 24,
        canvas_width=4, canvas_height=4,
    )
    monkeypatch.setattr(worker, "_open_cap", lambda: cap)

    cameras = []

    class FakeCamera:
        device = "fake-vcam"

        def __init__(self, **kwargs):
            self.kwargs = kwargs
            self.sent = []
            cameras.append(self)

        def __enter__(self):
            return self

        def __exit__(self, *_args):
            return False

        def send(self, frame):
            self.sent.append(frame.copy())
            worker.request_stop()

    monkeypatch.setattr(vcam.pyvirtualcam, "Camera", FakeCamera)
    statuses = []
    worker.status.connect(lambda kind, msg: statuses.append((kind, msg)))

    worker.run()

    assert cameras[0].kwargs["width"] == 4
    assert cameras[0].kwargs["height"] == 4
    assert cameras[0].kwargs["fps"] == 24
    assert cameras[0].sent[0].shape == (4, 4, 3)
    # Names the camera as other apps list it: the card label on Linux, the device name elsewhere.
    shown_as = vcam.V4L2_PHONE_LABEL if vcam.IS_LINUX else "fake-vcam"
    assert any(kind == "ok" and msg.endswith(f"fps to {shown_as}") for kind, msg in statuses)
    assert statuses[-1] == ("idle", "Not streaming")


def test_run_opens_at_the_size_a_reader_holds_the_camera_at(monkeypatch):
    frame = np.full((2, 4, 3), 200, dtype=np.uint8)
    cap = _Capture([(True, frame)] * 20)
    worker = stream.StreamWorker("url", None, None, 24, canvas_width=4, canvas_height=4)
    monkeypatch.setattr(worker, "_open_cap", lambda: cap)
    monkeypatch.setattr(vcam, "locked_size", lambda: (6, 6))
    cameras = []

    class FakeCamera:
        device = "fake-vcam"

        def __init__(self, **kwargs):
            self.kwargs, self.sent = kwargs, []
            cameras.append(self)

        def __enter__(self):
            return self

        def __exit__(self, *_args):
            return False

        def send(self, frame):
            self.sent.append(frame.copy())
            worker.request_stop()

    monkeypatch.setattr(vcam.pyvirtualcam, "Camera", FakeCamera)
    opened = []
    worker.vcam_opened.connect(lambda w, h: opened.append((w, h)))
    worker.run()

    assert (cameras[0].kwargs["width"], cameras[0].kwargs["height"]) == (6, 6)
    assert cameras[0].sent[0].shape == (6, 6, 3) and opened == [(6, 6)]


def test_fps_change_rebuilds_the_vcam_without_reopening_the_phone_stream(monkeypatch):
    frame = np.full((2, 4, 3), [10, 20, 30], dtype=np.uint8)
    feed = threading.Event()

    class _EndlessCapture(_Capture):
        def read(self):
            feed.wait(0.01)
            return True, frame

    opened = []
    worker = stream.StreamWorker("url", None, None, 24, canvas_width=4, canvas_height=4)
    monkeypatch.setattr(worker, "_open_cap", lambda: opened.append(1) or _EndlessCapture())

    cameras = []

    class FakeCamera:
        device = "fake-vcam"

        def __init__(self, **kwargs):
            self.kwargs = kwargs
            self.ticks = 0
            cameras.append(self)

        def __enter__(self):
            return self

        def __exit__(self, *_args):
            return False

        def send(self, frame):
            self.ticks += 1
            if len(cameras) == 1 and self.ticks == 2:
                worker.update_output(fps=15)
            elif len(cameras) == 2:
                worker.request_stop()

    monkeypatch.setattr(vcam.pyvirtualcam, "Camera", FakeCamera)
    worker.run()

    assert [c.kwargs["fps"] for c in cameras] == [24, 15]
    assert len(opened) == 1  # same phone connection throughout


def test_windows_opens_the_camera_named_telescope_or_any_older_registration(monkeypatch):
    monkeypatch.setattr(vcam, "IS_LINUX", False)
    opened = []

    def camera(**kwargs):
        opened.append(kwargs["device"])
        if kwargs["device"] == vcam.UC_NAME and len(opened) == 1:
            raise RuntimeError("No camera registered with this name.")
        return kwargs["device"]
    monkeypatch.setattr(vcam.pyvirtualcam, "Camera", camera)
    worker = stream.StreamWorker("url", None, None, 30)
    assert worker._open_vcam(4, 4) is None
    assert opened == ["Telescope", None]
    assert worker._open_vcam(4, 4) == "Telescope"

    monkeypatch.setattr(vcam, "IS_LINUX", True)
    assert worker._open_vcam(4, 4) == vcam.V4L2_PHONE_DEV


def test_newest_hands_over_only_the_latest_item():
    newest, done = stream._Newest(), threading.Event()
    newest.put("old")
    newest.put("new")
    done.set()

    assert newest.take(done) == "new"
    assert newest.take(done) is None


def test_a_frame_that_finishes_after_a_newer_one_is_dropped():
    worker = stream.StreamWorker("url", None, None, 30)
    newer = np.full((1, 1, 3), 2, dtype=np.uint8)

    worker._publish(5, newer)
    worker._publish(4, np.full((1, 1, 3), 1, dtype=np.uint8))

    assert worker._latest is newer
    assert worker._frames_received == 1


def test_a_slow_decoder_skips_to_the_newest_packet(monkeypatch):
    import time
    packets = [np.full((1, 1, 3), i, dtype=np.uint8) for i in range(1, 6)]

    class _SlowCapture(_Capture):
        parallel_decode = True

        @staticmethod
        def decode(frame):
            time.sleep(0.1)
            return frame

    cap = _SlowCapture([(True, p) for p in packets])
    worker = stream.StreamWorker("url", None, None, 30)

    def no_reconnect(_stop):
        worker._stop_flag = True
        return None

    worker._reconnect_cap = no_reconnect
    worker._stream_reader(cap, threading.Event(), threading.Event())

    assert worker._latest[0, 0, 0] == 5
    assert worker._frames_received < 5


def test_frames_that_arrived_together_all_count_as_arrived():
    packets = [np.full((1, 1, 3), i, dtype=np.uint8) for i in range(1, 4)]

    class _BurstCapture(_Capture):
        last_frame_count = 3  # each read took in three frames and kept the newest

    cap = _BurstCapture([(True, p) for p in packets])
    worker = stream.StreamWorker("url", None, None, 30)

    def no_reconnect(_stop):
        worker._stop_flag = True
        return None

    worker._reconnect_cap = no_reconnect
    worker._stream_reader(cap, threading.Event(), threading.Event())

    assert worker._frames_arrived == 9 and worker._frames_received <= 3


def test_a_new_frame_goes_to_the_camera_without_waiting_for_the_next_tick(monkeypatch):
    import time
    worker = stream.StreamWorker("url", None, None, 2)  # a tick every 0.5 s would be far too late
    worker._latest = np.zeros((2, 2, 3), dtype=np.uint8)
    sent = []

    class FakeCamera:
        device = "fake-vcam"

        def __init__(self, **_kwargs):
            pass

        def __enter__(self):
            return self

        def __exit__(self, *_args):
            return False

        def send(self, frame):
            sent.append((time.monotonic(), frame[0, 0, 0]))

    monkeypatch.setattr(vcam.pyvirtualcam, "Camera", FakeCamera)
    monkeypatch.setattr(vcam, "locked_size", lambda: None)
    feeder = threading.Thread(target=worker._run_vcam)
    feeder.start()
    try:
        time.sleep(0.3)  # past the burst pacing (half a period) after the first send
        published_at = time.monotonic()
        worker._publish(next(worker._seq), np.full((2, 2, 3), 7, dtype=np.uint8))
        deadline = published_at + 0.5
        while time.monotonic() < deadline and not any(v == 7 for _, v in sent):
            time.sleep(0.01)
    finally:
        worker.request_stop()
        feeder.join(timeout=3)

    assert any(v == 7 and t - published_at < 0.1 for t, v in sent)


def test_the_virtual_camera_opens_in_bgr(monkeypatch):
    opened = []
    monkeypatch.setattr(vcam.pyvirtualcam, "Camera", lambda **kwargs: opened.append(kwargs["fmt"]))
    stream.StreamWorker("url", None, None, 30)._open_vcam(4, 4)

    assert opened == [vcam.pyvirtualcam.PixelFormat.BGR]


def test_a_crashing_reader_wakes_the_vcam_instead_of_freezing_it():
    class Broken(_Capture):
        def read_packet(self):
            raise RuntimeError("bad packet")

    worker = stream.StreamWorker("url", None, None, 30)
    cap, done = Broken(), threading.Event()
    worker._stream_reader(cap, threading.Event(), done)

    assert done.is_set() and cap.released
    assert worker._restart_vcam.is_set() and worker._frame_ready.is_set()


def test_a_crash_in_the_worker_ends_with_an_error_and_idle(monkeypatch):
    worker = stream.StreamWorker("url", None, None, 30)
    monkeypatch.setattr(worker, "_open_cap", lambda: (_ for _ in ()).throw(RuntimeError("boom")))
    statuses = []
    worker.status.connect(lambda kind, msg: statuses.append((kind, msg)))

    worker.run()

    assert ("error", "Stream error: boom") in statuses
    assert statuses[-1] == ("idle", "Not streaming")


def test_the_fps_readout_counts_frames_from_the_phone_not_resends(monkeypatch):
    import time
    worker = stream.StreamWorker("url", None, None, 60)
    worker._latest = np.zeros((2, 2, 3), dtype=np.uint8)  # one frame, then the phone sends nothing new
    statuses = []
    from PyQt6.QtCore import Qt
    # Direct: emitted from the feeder thread, with no event loop here to deliver a queued call
    worker.status.connect(lambda kind, msg: statuses.append((kind, msg)), Qt.ConnectionType.DirectConnection)

    class FakeCamera:
        device = "fake-vcam"

        def __init__(self, **_kwargs):
            pass

        def __enter__(self):
            return self

        def __exit__(self, *_args):
            return False

        def send(self, _frame):
            pass

    monkeypatch.setattr(vcam.pyvirtualcam, "Camera", FakeCamera)
    monkeypatch.setattr(vcam, "locked_size", lambda: None)
    feeder = threading.Thread(target=worker._run_vcam)
    feeder.start()
    try:
        deadline = time.monotonic() + 3.0
        while time.monotonic() < deadline and not any(k == "fps" for k, _ in statuses):
            time.sleep(0.05)
    finally:
        worker.request_stop()
        feeder.join(timeout=3)

    fps = [msg for kind, msg in statuses if kind == "fps"]
    assert fps and fps[0].startswith("0.0 fps")  # the last frame was resent ~30 times a second meanwhile


class _Clock:
    def __init__(self, monkeypatch):
        self.now = 1000.0
        monkeypatch.setattr(stream.time, "monotonic", lambda: self.now)

    def flow(self, worker, seconds: float):
        """Frames arriving every 0.1 s for this long, the last one at the end."""
        for _ in range(round(seconds * 10)):
            self.now += 0.1
            worker._count_arrived(1)


def test_the_link_is_judged_only_once_frames_have_flowed_for_a_moment_after_start(monkeypatch):
    clock = _Clock(monkeypatch)
    worker = stream.StreamWorker("url", 1280, 720, 30)
    clock.now += 4.0  # a slow phone: nothing for a while
    worker._count_arrived(1)
    assert not worker._judges_window(clock.now)  # frames only just started
    clock.flow(worker, stream.SETTLE_AFTER_FRAMES_S - 0.1)
    assert not worker._judges_window(clock.now)
    clock.flow(worker, 0.1)
    assert not worker._judges_window(clock.now - 1.0)  # a window that started while settling doesn't count
    assert worker._judges_window(clock.now)
    assert worker._settle_since is None


def test_old_lens_frames_before_the_gap_do_not_count_as_back(monkeypatch):
    clock = _Clock(monkeypatch)
    worker = stream.StreamWorker("url", 1280, 720, 30)
    worker._settle_since = None  # streaming along
    worker.settle()  # lens switch
    worker._count_arrived(1)  # the old lens's last frame
    clock.now += 3.0  # an older phone switching
    worker._count_arrived(1)
    clock.flow(worker, stream.SETTLE_AFTER_FRAMES_S - 0.5)
    assert not worker._judges_window(clock.now)
    clock.flow(worker, 0.5)
    assert worker._judges_window(clock.now)


def test_a_phone_that_never_gets_going_is_judged_after_the_cap(monkeypatch):
    clock = _Clock(monkeypatch)
    worker = stream.StreamWorker("url", 1280, 720, 30)
    clock.now += stream.SETTLE_MAX_S - 0.1
    assert not worker._judges_window(clock.now)
    clock.now += 0.1
    assert worker._judges_window(clock.now)


def test_a_gap_once_settled_is_not_excused(monkeypatch):
    clock = _Clock(monkeypatch)
    worker = stream.StreamWorker("url", 1280, 720, 30)
    worker._count_arrived(1)
    clock.flow(worker, stream.SETTLE_AFTER_FRAMES_S)
    clock.now += 3.0  # the link stalls after the phone got going
    worker._count_arrived(1)
    assert worker._judges_window(clock.now - 2.0)


def test_a_new_fps_settles_the_stream(monkeypatch):
    clock = _Clock(monkeypatch)
    worker = stream.StreamWorker("url", 1280, 720, 30)
    worker._settle_since = None
    worker.update_output(fps=60)
    assert worker._settle_since == clock.now
