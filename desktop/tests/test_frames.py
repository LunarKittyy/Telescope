"""Frames from other streams (HostFrames): every running stream's newest frame, safe to read from any thread."""

import threading
import time

import numpy as np
import pytest

import telescope.stream as stream
from telescope import vision
from telescope.frames import Frames


def _worker(steps=()) -> stream.StreamWorker:
    return stream.StreamWorker("url", None, None, 30, list(steps))


def _frame(value: int, size=(4, 6)) -> np.ndarray:
    return np.full((*size, 3), value % 256, np.uint8)


def test_a_published_frame_carries_a_number_and_when_it_was_made():
    a, b = _worker(), _worker()
    before = time.monotonic()
    a._publish(1, _frame(1))
    b._publish(1, _frame(2))
    a._publish(2, _frame(3))
    frames = Frames()
    frames.set_streams([("a", "Phone A", a), ("b", "Phone B", b)])
    frame_a, frame_b = frames.frames()
    assert (frame_a.source_id, frame_a.name, frame_b.source_id) == ("a", "Phone A", "b")
    assert frame_a.number > frame_b.number  # numbered across every stream, in the order they came
    assert frame_a.frame[0, 0, 0] == 3 and before <= frame_a.time <= time.monotonic()


def test_frames_are_handed_out_read_only_and_without_copying():
    worker = _worker()
    worker._publish(1, _frame(7))
    frames = Frames()
    frames.set_streams([("a", "A", worker)])
    shared = frames.frame("a")
    assert np.shares_memory(shared.frame, worker.latest_frame())
    with pytest.raises(ValueError):
        shared.frame[0, 0, 0] = 1
    own = frames.frame("a", copy=True)
    own.frame[0, 0, 0] = 1  # yours to change, and the stream's is left alone
    assert not np.shares_memory(own.frame, worker.latest_frame()) and worker.latest_frame()[0, 0, 0] == 7


def test_streams_without_a_picture_or_that_stopped_are_left_out():
    sending, starting = _worker(), _worker()
    sending._publish(1, _frame(1))
    frames = Frames()
    frames.set_streams([("a", "A", sending), ("b", "B", starting), ("c", "Camera off", None)])
    assert [f.source_id for f in frames.frames()] == ["a"]
    assert frames.frame("b") is None and frames.frame("c") is None and frames.frame("nope") is None
    frames.set_streams([("b", "B", starting)])  # a stopped
    assert frames.frames() == [] and frames.frame("a") is None


def test_a_dropped_stream_keeps_its_last_frame_which_only_gets_older():
    worker = _worker()
    worker._publish(1, _frame(1))
    frames = Frames()
    frames.set_streams([("a", "A", worker)])
    seen = frames.frame("a")
    assert seen.age(seen.time + 5) == 5
    assert frames.frame("a").number == seen.number  # nothing new came


def test_a_frame_step_can_tell_which_stream_its_frame_is_from():
    frames = Frames()
    heard = []
    step = lambda frame: heard.append(frames.source_of_current()) or frame  # noqa: E731
    a, b = _worker([step]), _worker([step])
    frames.set_streams([("a", "A", a), ("b", "B", b)])
    a._publish(1, _frame(1))
    b._publish(1, _frame(1))
    assert heard == ["a", "b"]
    assert frames.source_of_current() is None  # outside the frame steps
    token = vision.current_frame.set((object(), 1))  # a worker it doesn't know (already let go)
    try:
        assert frames.source_of_current() is None
    finally:
        vision.current_frame.reset(token)


def test_reading_while_streams_publish_and_come_and_go_never_sees_a_torn_frame():
    workers = [_worker(), _worker(), _worker()]
    frames = Frames()
    frames.set_streams([(str(i), str(i), w) for i, w in enumerate(workers)])
    stop, problems, seen = threading.Event(), [], set()

    def publish(worker, base):
        seq = 0
        while not stop.is_set():
            seq += 1
            worker._publish(seq, _frame(base + seq, size=(8, 8)))

    def read():
        numbers = {}  # source id: the newest frame number this reader has seen of it
        while not stop.is_set():
            for sf in frames.frames():
                if not (sf.frame == sf.frame[0, 0, 0]).all():
                    problems.append("a frame changed while it was being read")
                last = numbers.setdefault(sf.source_id, 0)
                if sf.number < last:
                    problems.append("a stream's frame number went backwards")
                numbers[sf.source_id] = sf.number
                seen.add(sf.source_id)

    threads = [threading.Thread(target=publish, args=(w, i * 50)) for i, w in enumerate(workers)]
    threads += [threading.Thread(target=read) for _ in range(2)]
    for t in threads:
        t.start()
    deadline = time.monotonic() + 0.5
    flip = 0
    while time.monotonic() < deadline:
        flip += 1
        live = workers if flip % 2 else workers[:1]  # streams stopping and starting meanwhile
        frames.set_streams([(str(i), str(i), w) for i, w in enumerate(live)])
    stop.set()
    for t in threads:
        t.join(5)
    assert problems == []
    assert seen == {"0", "1", "2"}


def test_the_window_shares_its_streams_as_they_start_change_and_stop(qapp, config_home, monkeypatch):
    from types import SimpleNamespace

    from telescope import app as app_module
    from telescope.session import StreamSession

    monkeypatch.setattr(app_module.QSystemTrayIcon, "isSystemTrayAvailable", lambda: False)
    win = app_module.TelescopeWindow()
    try:
        worker = _worker()
        worker._publish(1, _frame(9))
        source = SimpleNamespace(id="test:cam", name="Test camera", session=None)
        win._sessions.add(source, StreamSession(id=1, url="u", client=None, worker=worker, source=source))
        assert [(f.source_id, f.name) for f in win.stream_frames()] == [("test:cam", "Test camera")]
        assert win.stream_frame("test:cam").frame[0, 0, 0] == 9
        win._sessions.add(source, StreamSession(id=1, url="u", client=None, worker=None, source=source))
        assert win.stream_frames() == []  # the camera went off
        win._sessions.forget(source)
        assert win.stream_frame("test:cam") is None
    finally:
        win._tray = None
        win.deleteLater()
