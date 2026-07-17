import threading

import cv2
import numpy as np

import telescope.stream as stream


class _Capture:
    def __init__(self, frames=(), opened=True):
        self.frames = list(frames)
        self.opened = opened
        self.released = False

    def isOpened(self):
        return self.opened

    def read(self):
        if self.frames:
            return self.frames.pop(0)
        return False, None

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


def test_request_stop_sets_both_stop_signals():
    worker = stream.StreamWorker("url", None, None, 30)

    worker.request_stop()

    assert worker._stop_flag is True
    assert worker._restart_vcam.is_set()


def test_open_cap_passes_ffmpeg_timeouts(monkeypatch):
    calls = []
    sentinel = object()
    monkeypatch.setattr(stream.cv2, "VideoCapture", lambda *args: calls.append(args) or sentinel)
    worker = stream.StreamWorker("http://phone/video", None, None, 30)

    assert worker._open_cap() is sentinel
    assert calls == [(
        "http://phone/video",
        cv2.CAP_FFMPEG,
        [cv2.CAP_PROP_OPEN_TIMEOUT_MSEC, 3000,
         cv2.CAP_PROP_READ_TIMEOUT_MSEC, 1000],
    )]


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


def test_reconnect_stops_without_opening_when_cancelled(monkeypatch):
    worker = stream.StreamWorker("url", None, None, 30)
    stop = threading.Event()
    stop.set()
    monkeypatch.setattr(worker, "_open_cap", lambda: (_ for _ in ()).throw(AssertionError()))

    assert worker._reconnect_cap(stop) is None


def test_stream_reader_resizes_converts_colour_and_runs_pipeline():
    # BGR [1, 2, 3] must become RGB [3, 2, 1].
    raw = np.tile(np.array([[[1, 2, 3]]], dtype=np.uint8), (2, 2, 1))
    cap = _Capture([(True, raw)])
    worker = stream.StreamWorker("url", 4, 3, 30, [lambda frame: frame + 1])

    # The second read fails and reconnect exits because this flag is set there.
    def no_reconnect(_stop):
        worker._stop_flag = True
        return None

    worker._reconnect_cap = no_reconnect
    worker._stream_reader(cap, threading.Event())

    assert cap.released is True
    assert worker._latest_rgb.shape == (3, 4, 3)
    assert worker._latest_rgb[0, 0].tolist() == [4, 3, 2]


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

    # The capture's next failed read invokes reconnect; use that boundary to stop.
    worker._reconnect_cap = lambda _event: stop_after_error()
    worker._stream_reader(cap, threading.Event())

    assert worker._latest_rgb is None
    assert cap.released is True


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

        def send(self, rgb):
            self.sent.append(rgb.copy())

        def sleep_until_next_frame(self):
            worker.request_stop()

    monkeypatch.setattr(stream.pyvirtualcam, "Camera", FakeCamera)
    statuses = []
    worker.status.connect(lambda kind, msg: statuses.append((kind, msg)))

    worker.run()

    assert cameras[0].kwargs["width"] == 4
    assert cameras[0].kwargs["height"] == 4
    assert cameras[0].kwargs["fps"] == 24
    assert cameras[0].sent[0].shape == (4, 4, 3)
    assert any(kind == "ok" and "fake-vcam" in msg for kind, msg in statuses)
    assert statuses[-1] == ("idle", "Stopped.")
