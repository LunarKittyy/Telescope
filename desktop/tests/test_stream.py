import threading

import numpy as np
import pytest

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

    result = stream.vcam.fit_frame(frame, 5, 3)

    assert result is frame


def test_fit_frame_resizes_same_aspect_ratio():
    frame = np.full((2, 4, 3), 17, dtype=np.uint8)

    result = stream.vcam.fit_frame(frame, 8, 4)

    assert result.shape == (4, 8, 3)
    assert np.all(result == 17)


def test_fit_frame_letterboxes_and_preserves_dtype():
    frame = np.full((2, 4, 3), 255, dtype=np.uint16)

    result = stream.vcam.fit_frame(frame, 4, 4)

    assert result.shape == (4, 4, 3)
    assert result.dtype == np.uint16
    assert np.all(result[0] == 0)
    assert np.all(result[1:3] == 255)
    assert np.all(result[3] == 0)


def test_fit_frame_pillarboxes_narrow_input():
    frame = np.full((4, 2, 3), 9, dtype=np.uint8)

    result = stream.vcam.fit_frame(frame, 4, 4)

    assert np.all(result[:, 0] == 0)
    assert np.all(result[:, 1:3] == 9)
    assert np.all(result[:, 3] == 0)


def test_a_new_pipeline_takes_over_from_the_next_frame():
    worker = stream.StreamWorker("url", None, None, 30, [lambda f: f + 1])
    worker.set_pipeline([lambda f: f * 10])
    assert worker._process(np.array([2]))[0] == 20


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
    monkeypatch.setattr(stream, "DROP_RETRY_DELAY", 0)
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


def test_frames_stopping_for_a_photo_reconnect_quietly(monkeypatch):
    captures = iter([_Capture([(True, np.zeros((1, 1, 3), dtype=np.uint8))]) for _ in range(2)])
    worker = stream.StreamWorker("url", None, None, 30)
    monkeypatch.setattr(stream, "DROP_RETRY_DELAY", 0)
    monkeypatch.setattr(worker, "_open_cap", lambda: next(captures))
    said = []
    worker.status.connect(lambda kind, _msg: said.append(kind))

    worker.hold_for_photo(5)
    assert worker._reconnect_cap(threading.Event()) is not None
    worker._photo_until = 0  # the hold ran out: a stop is a dropped stream again
    assert worker._reconnect_cap(threading.Event()) is not None
    assert said == ["info", "reconnecting"]


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
    assert cameras[0].sent[0].shape == _sent_shape(4, 4)
    # Names the camera as other apps list it: the card label on Linux, the device name elsewhere.
    shown_as = vcam.V4L2_PHONE_LABEL if vcam.IS_LINUX else "fake-vcam"
    assert any(kind == "ok" and msg.endswith(f"fps to {shown_as}") for kind, msg in statuses)
    assert statuses[-1] == ("idle", "Not streaming")


def test_run_opens_at_the_size_a_reader_holds_the_camera_at(monkeypatch):
    frame = np.full((2, 4, 3), 200, dtype=np.uint8)
    cap = _Capture([(True, frame)] * 20)
    worker = stream.StreamWorker("url", None, None, 24, canvas_width=4, canvas_height=4)
    monkeypatch.setattr(worker, "_open_cap", lambda: cap)
    monkeypatch.setattr(vcam, "locked_size", lambda *_: (6, 6))
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
    assert cameras[0].sent[0].shape == _sent_shape(6, 6) and opened == [(6, 6)]


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


def test_another_slot_opens_its_own_camera(monkeypatch):
    opened = []
    monkeypatch.setattr(vcam.pyvirtualcam, "Camera", lambda **kwargs: opened.append(kwargs["device"]))
    monkeypatch.setattr(vcam, "IS_LINUX", False)
    stream.StreamWorker("url", None, None, 30, slot=2)._open_vcam(4, 4)
    assert opened == ["Telescope #3"]
    monkeypatch.setattr(vcam, "IS_LINUX", True)
    monkeypatch.setattr(vcam, "slot_device", lambda slot: f"/dev/video{40 + slot}")
    stream.StreamWorker("url", None, None, 30, slot=2)._open_vcam(4, 4)
    assert opened[-1] == "/dev/video42"


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
    monkeypatch.setattr(vcam, "locked_size", lambda *_: None)
    monkeypatch.setattr(stream, "_vcam_format", lambda *_: (vcam.pyvirtualcam.PixelFormat.BGR, None))  # frames stay readable
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


def _sent_shape(w: int, h: int) -> tuple:
    """What the camera gets for a w x h frame: I420 planes on Linux, BGR elsewhere."""
    return (h * 3 // 2, w) if vcam.IS_LINUX else (h, w, 3)


@pytest.mark.parametrize("linux, size, fmt", [
    (True, (4, 4), "I420"),
    (True, (5, 4), "BGR"),  # I420 needs even sizes
    (False, (4, 4), "BGR"),
])
def test_the_virtual_camera_opens_in_its_own_format(monkeypatch, linux, size, fmt):
    monkeypatch.setattr(vcam, "IS_LINUX", linux)
    opened = []
    monkeypatch.setattr(vcam.pyvirtualcam, "Camera", lambda **kwargs: opened.append(kwargs["fmt"]))
    stream.StreamWorker("url", None, None, 30)._open_vcam(*size)

    assert opened == [getattr(vcam.pyvirtualcam.PixelFormat, fmt)]


def test_frames_reach_an_i420_camera_converted():
    fmt, to_cam = stream._vcam_format(4, 2) if vcam.IS_LINUX else (None, None)
    if to_cam is None:
        pytest.skip("I420 is only used on Linux")
    frame = np.zeros((2, 4, 3), np.uint8)
    frame[:] = (255, 255, 255)

    out = to_cam(frame)

    assert out.shape == (3, 4) and (out[:2] == 235).all()  # white's luma in the video range OpenCV uses


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
    monkeypatch.setattr(vcam, "locked_size", lambda *_: None)
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


def test_a_parked_worker_lets_go_of_the_camera_and_takes_it_back_when_unparked(monkeypatch):
    import time
    frame = np.full((2, 4, 3), 9, dtype=np.uint8)

    class _EndlessCapture(_Capture):
        def read(self):
            time.sleep(0.01)
            return True, frame

    worker = stream.StreamWorker("url", None, None, 24, canvas_width=4, canvas_height=4)
    monkeypatch.setattr(worker, "_open_cap", lambda: _EndlessCapture())
    monkeypatch.setattr(vcam, "locked_size", lambda *_: None)
    events = []

    class FakeCamera:
        device = "fake-vcam"

        def __init__(self, **_kwargs):
            events.append("open")

        def __enter__(self):
            return self

        def __exit__(self, *_args):
            events.append("close")
            return False

        def send(self, _frame):
            pass

    def wait_for(cond):
        deadline = time.monotonic() + 3
        while not cond() and time.monotonic() < deadline:
            time.sleep(0.01)
        return cond()

    monkeypatch.setattr(vcam.pyvirtualcam, "Camera", FakeCamera)
    runner = threading.Thread(target=worker.run)
    runner.start()
    try:
        assert wait_for(lambda: events == ["open"])
        worker.set_parked(True)
        assert events == ["open", "close"]  # let go before set_parked returns, so the wait screen can open it
        time.sleep(0.2)
        assert events == ["open", "close"]  # and stays let go while the reader carries on
        worker.set_parked(False)
        assert wait_for(lambda: events == ["open", "close", "open"])
    finally:
        worker.request_stop()
        runner.join(timeout=5)
    assert not runner.is_alive()


class _Live(_Capture):
    """Frames filled with value, one every `every` seconds until released (or `gap_after` frames, then a long pause)."""

    def __init__(self, value, every=0.01, opened=True, gap_after=None):
        super().__init__(opened=opened)
        self.value, self.every, self.gap_after, self.count = value, every, gap_after, 0

    def read(self):
        if self.released:
            return False, None
        self.count += 1
        stream.time.sleep(self.every if self.gap_after is None or self.count != self.gap_after else 1.0)
        return True, np.full((2, 2, 3), self.value, dtype=np.uint8)

    def open(self):
        pass


_handover_workers = []


@pytest.fixture(autouse=True)
def _handovers_end():
    """A test's hand-over threads end before it lets go of their worker, so the GUI thread destroys it."""
    yield
    for worker in _handover_workers:
        worker._stop_flag = True
        worker.join_handover()
    _handover_workers.clear()


def _handover_worker(monkeypatch, new_reader):
    monkeypatch.setattr(stream, "HANDOVER_HEALTHY_S", 0.2)
    monkeypatch.setattr(stream, "HANDOVER_TIMEOUT_S", 1.5)
    worker = stream.StreamWorker("https://10.0.0.5:8080/v1/video", None, None, 30)
    _handover_workers.append(worker)
    monkeypatch.setattr(worker, "_new_reader", lambda url: new_reader)
    moved, failed = [], []
    worker.moved.connect(moved.append)
    worker.move_failed.connect(failed.append)
    return worker, moved, failed


def _until(cond, timeout=3.0):
    from PyQt6.QtCore import QCoreApplication
    end = stream.time.monotonic() + timeout
    while not cond() and stream.time.monotonic() < end:
        QCoreApplication.processEvents()  # the worker's signals come from its threads, queued
        stream.time.sleep(0.01)
    return cond()


def test_hand_over_moves_to_the_new_route_once_it_keeps_frames_coming(qapp, monkeypatch):
    old, new = _Live(1), _Live(2)
    worker, moved, failed = _handover_worker(monkeypatch, new)
    reader = threading.Thread(target=worker._stream_reader, args=(old, threading.Event(), threading.Event()))
    reader.start()
    try:
        assert _until(lambda: worker._latest is not None and worker._latest[0, 0, 0] == 1)
        assert worker.hand_over("https://127.0.0.1:41000/v1/video")
        assert not worker.hand_over("https://127.0.0.1:41000/v1/video")  # one at a time
        assert _until(lambda: moved == ["https://127.0.0.1:41000/v1/video"])
        assert _until(lambda: worker._latest[0, 0, 0] == 2)
        assert old.released and not new.released
        assert worker.url == "https://127.0.0.1:41000/v1/video"
        assert failed == []
    finally:
        worker._stop_flag = True
        reader.join(3)


def test_hand_over_gives_up_on_a_route_with_gaps_and_keeps_the_old_one(qapp, monkeypatch):
    old, new = _Live(1), _Live(2, gap_after=5)
    monkeypatch.setattr(stream, "HANDOVER_GAP_S", 0.5)
    worker, moved, failed = _handover_worker(monkeypatch, new)
    monkeypatch.setattr(stream, "HANDOVER_HEALTHY_S", 1.0)
    monkeypatch.setattr(stream, "HANDOVER_TIMEOUT_S", 1.2)  # the gap leaves too little time to be healthy again
    reader = threading.Thread(target=worker._stream_reader, args=(old, threading.Event(), threading.Event()))
    reader.start()
    try:
        assert worker.hand_over("https://127.0.0.1:41000/v1/video")
        assert _until(lambda: failed == ["https://127.0.0.1:41000/v1/video"])
        assert moved == [] and new.released and not old.released
        assert worker._latest[0, 0, 0] == 1
        assert worker.hand_over("https://127.0.0.1:41000/v1/video")  # it can try again later
    finally:
        worker._stop_flag = True
        reader.join(3)


def test_hand_over_fails_on_a_route_that_wont_open(qapp, monkeypatch):
    worker, moved, failed = _handover_worker(monkeypatch, _Live(2, opened=False))
    assert worker.hand_over("https://127.0.0.1:41000/v1/video")
    assert _until(lambda: failed == ["https://127.0.0.1:41000/v1/video"])
    assert moved == []


def test_a_stream_that_drops_mid_hand_over_carries_on_on_the_new_route(qapp, monkeypatch):
    new = _Live(2)
    worker, moved, _failed = _handover_worker(monkeypatch, new)
    monkeypatch.setattr(stream, "DROP_RETRY_DELAY", 0)
    monkeypatch.setattr(worker, "_open_cap", lambda: _Capture(opened=False))
    assert worker.hand_over("https://127.0.0.1:41000/v1/video")
    assert _until(lambda: worker._handover_ready is not None)
    assert worker._reconnect_cap(threading.Event()) is new
    assert moved == ["https://127.0.0.1:41000/v1/video"]


def test_a_stop_doesnt_wait_for_a_ready_hand_over_to_be_taken(qapp, monkeypatch):
    new = _Live(2)
    worker, moved, failed = _handover_worker(monkeypatch, new)
    assert worker.hand_over("https://127.0.0.1:41000/v1/video")
    assert _until(lambda: worker._handover_ready is not None)
    worker.request_stop()
    start = stream.time.monotonic()
    worker.join_handover()
    assert stream.time.monotonic() - start < 1.0  # the GUI thread waits on this when it ends the worker
    assert new.released and moved == [] and failed == []


def test_sources_from_plugins_dont_hand_over():
    worker = stream.StreamWorker("url", None, None, 30, open_reader=lambda: _Capture())
    assert not worker.hand_over("https://127.0.0.1:41000/v1/video")


def test_an_h264_stream_says_it_is_getting_ready_while_pyav_still_loads(monkeypatch):
    loads = []
    monkeypatch.setattr(stream.h264_reader, "loaded", lambda: False)
    monkeypatch.setattr(stream.h264_reader, "load_av", lambda: loads.append(1))
    worker = stream.StreamWorker("http://127.0.0.1:41000/v1/video.h264", None, None, 30)
    statuses = []
    worker.status.connect(lambda kind, text: (statuses.append(text), worker.request_stop()))
    worker._run()
    assert statuses[0].startswith("Getting ready")
    assert loads == [1]


def test_an_mjpeg_stream_does_not_wait_for_pyav(monkeypatch):
    monkeypatch.setattr(stream.h264_reader, "loaded", lambda: False)
    monkeypatch.setattr(stream.h264_reader, "load_av", lambda: pytest.fail("MJPEG doesn't need PyAV"))
    worker = stream.StreamWorker("http://127.0.0.1:41000/v1/video", None, None, 30)
    statuses = []
    worker.status.connect(lambda kind, text: (statuses.append(text), worker.request_stop()))
    worker._run()
    assert not statuses[0].startswith("Getting ready")
