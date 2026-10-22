import os
import threading
import time
from pathlib import Path

import numpy as np
import pytest

from telescope import vision
from telescope.vision import FAILED, LOADING, MISSING, READY, Person, Vision


class _Backend:
    version = "fake"

    def __init__(self):
        self.segmented, self.closed, self.timestamps = [], [], []

    def segmenter(self):
        def segment(frame, ts_ms):
            self.segmented.append(frame)
            self.timestamps.append(ts_ms)
            return np.full(frame.shape[:2], 200, np.uint8)
        return segment, lambda: self.closed.append("segmenter")

    def pose(self):
        def detect(frame, _ts_ms):
            return [Person({"nose": (0.5, 0.25, 0.9), "left_wrist": (0.1, 0.5, 0.2)})]
        return detect, lambda: self.closed.append("pose")


def _ready(**kw):
    backend, states, people = _Backend(), [], []
    v = Vision(on_people=lambda key, found: people.append((key, found)), on_state=states.append,
               find=lambda: backend, **kw)
    v.load()
    _wait(lambda: v.state() == READY)
    return v, backend, states, people


def _wait(condition, timeout=2.0):
    end = time.monotonic() + timeout
    while not condition():
        assert time.monotonic() < end, "timed out"
        time.sleep(0.005)


def _in_stream(key, seq, fn):
    token = vision.current_frame.set((key, seq))
    try:
        return fn()
    finally:
        vision.current_frame.reset(token)


def test_a_mask_is_worked_out_once_per_frame_and_per_stream():
    v, backend, states, _people = _ready()
    frame = np.zeros((4, 6, 3), np.uint8)
    first = _in_stream("a", 1, lambda: v.person_mask(frame))
    again = _in_stream("a", 1, lambda: v.person_mask(frame))
    other = _in_stream("b", 1, lambda: v.person_mask(frame))
    _in_stream("a", 2, lambda: v.person_mask(frame))
    assert first is again and first.shape == (4, 6) and first.dtype == np.uint8
    assert other is not first
    assert len(backend.segmented) == 3
    assert backend.timestamps[2] > backend.timestamps[0]  # each stream's go up, as VIDEO mode needs
    assert states == [LOADING, READY]


def test_before_its_ready_or_outside_a_stream_there_is_no_mask():
    gate = threading.Event()
    states = []

    def slow():
        gate.wait()
        return _Backend()
    v = Vision(on_people=lambda *_: None, on_state=states.append, find=slow)
    frame = np.zeros((4, 6, 3), np.uint8)
    assert _in_stream("a", 1, lambda: v.person_mask(frame)) is None  # starts loading
    assert v.state() == LOADING
    gate.set()
    _wait(lambda: v.state() == READY)
    assert v.person_mask(frame) is None  # not called from a frame step


def test_without_a_pack_its_missing_and_tries_again_later():
    tries = []

    def find():
        tries.append(1)
        raise FileNotFoundError("no vision pack in /home/someone/.local/share/telescope/vision/3.6.0")
    v = Vision(on_people=lambda *_: None, on_state=lambda _s: None, find=find)
    v.load()
    _wait(lambda: v.state() == MISSING)
    assert v.diagnostics() == {"Vision": "not downloaded"}  # no path (it has the user's name in it)
    v.load()  # asked again on the next frame: it doesn't look again straight away
    assert len(tries) == 1
    v._looked_at -= vision._LOOK_AGAIN_S
    v.load()
    _wait(lambda: len(tries) == 2)


def test_a_broken_mediapipe_fails_once_and_names_only_the_error():
    tries = []

    def find():
        tries.append(1)
        raise OSError("libmediapipe.so: cannot open /home/someone/x")
    v = Vision(on_people=lambda *_: None, on_state=lambda _s: None, find=find)
    v.load()
    _wait(lambda: v.state() == FAILED)
    v.load()
    assert len(tries) == 1
    assert v.diagnostics() == {"Vision": "didn't load (OSError)"}


def test_auto_framing_loads_a_pack_that_arrives_while_it_is_on():
    backend, people, available, tries = _Backend(), [], [False], []

    def find():
        tries.append(1)
        if not available[0]:
            raise FileNotFoundError("pack not downloaded")
        return backend

    v = Vision(on_people=lambda key, found: people.append((key, found)), on_state=lambda _s: None, find=find)
    try:
        v.want_people("Auto framing", True)
        _wait(lambda: v.state() == MISSING)
        available[0] = True
        frame = np.zeros((40, 60, 3), np.uint8)
        _in_stream("a", 1, lambda: v.feed_raw(frame))
        assert tries == [1]  # absent packs are not looked for on every video frame
        v._looked_at -= vision._LOOK_AGAIN_S
        _in_stream("a", 2, lambda: v.feed_raw(frame))
        _wait(lambda: v.state() == READY)
        _in_stream("a", 3, lambda: v.feed_raw(frame))
        _wait(lambda: people)
        assert people[0][0] == "a"
    finally:
        v.shutdown()


def test_people_come_from_their_own_thread_only_while_wanted(monkeypatch):
    v, backend, _states, people = _ready()
    frame = np.zeros((720, 1280, 3), np.uint8)
    _in_stream("a", 1, lambda: v.feed_raw(frame))
    assert people == []  # nobody asked
    v.want_people("Auto framing", True)
    _in_stream("a", 2, lambda: v.feed_raw(frame))
    _wait(lambda: people)
    key, found = people[0]
    assert key == "a" and found[0].point("nose") == (0.5, 0.25)
    assert found[0].point("left_wrist") is None  # the model wasn't sure
    v.drop_stream("a")
    _wait(lambda: "pose" in backend.closed)
    v.want_people("Auto framing", False)
    assert not v.wants_people()


def test_people_are_found_upright_and_put_where_they_are_shown():
    v, _backend, _states, people = _ready()
    v.want_people("Auto framing", True)
    seen = []

    def prepare(frame):
        seen.append(frame.shape)
        return np.ascontiguousarray(np.rot90(frame))
    _in_stream("a", 1, lambda: v.feed_raw(np.zeros((720, 1280, 3), np.uint8), prepare, lambda x, y: (1 - y, x)))
    _wait(lambda: people)
    assert seen == [(360, 640, 3)]
    assert people[0][1][0].point("nose") == (0.75, 0.5)


def test_frames_for_people_are_spaced_out_and_made_small(monkeypatch):
    v, _backend, _states, _people = _ready()
    v.want_people("x", True)
    sizes = []
    monkeypatch.setattr(vision.threading, "Thread", lambda **kw: type("T", (), {"start": lambda self: None})())
    frame = np.zeros((1080, 1920, 3), np.uint8)
    for seq in range(5):
        _in_stream("a", seq, lambda: v.feed_raw(frame))
        sv = v._streams["a"]
        if sv.people_frame is not None:
            sizes.append(sv.people_frame[0].shape)
            sv.people_frame = None
    assert sizes == [(360, 640, 3)]


def test_a_segmenter_that_cant_load_fails_once_and_names_the_missing_library():
    v, backend, states, _people = _ready()
    tries = []

    def broken():
        tries.append(1)
        raise OSError("libGLESv2.so.2: cannot open shared object file: No such file or directory")
    backend.segmenter = broken
    frame = np.zeros((4, 6, 3), np.uint8)
    for seq in range(3):
        assert _in_stream("a", seq, lambda: v.person_mask(frame)) is None
    assert tries == [1] and v.state() == vision.FAILED and states[-1] == vision.FAILED
    assert "OSError: libGLESv2.so.2 missing" in str(v.diagnostics())


def test_a_failure_message_with_a_path_in_it_stays_out_of_diagnostics():
    v = Vision(on_people=lambda *_a: None, on_state=lambda _s: None,
               find=lambda: (_ for _ in ()).throw(OSError("/home/someone/vision/site: broken")))
    v.load()
    _wait(lambda: v.state() == vision.FAILED)
    assert "someone" not in str(v.diagnostics()) and "OSError" in str(v.diagnostics())


def test_dropping_a_stream_closes_its_segmenter():
    v, backend, _states, _people = _ready()
    _in_stream("a", 1, lambda: v.person_mask(np.zeros((4, 6, 3), np.uint8)))
    v.drop_stream("a")
    assert backend.closed == ["segmenter"]
    v.drop_stream("a")  # twice is fine


@pytest.mark.skipif(not os.environ.get("TELESCOPE_VISION_PACK"), reason="set TELESCOPE_VISION_PACK to a pack folder")
def test_the_real_models_find_a_person_shaped_blob():
    backend = vision.MediaPipeBackend(Path(os.environ["TELESCOPE_VISION_PACK"]))
    segment, close = backend.segmenter()
    frame = np.full((720, 1280, 3), 90, np.uint8)
    mask = segment(frame, 1)
    close()
    assert mask.shape == (720, 1280) and mask.dtype == np.uint8
    detect, close = backend.pose()
    assert detect(frame[:360, :640].copy(), 1) == []  # nobody in a grey frame
    close()


def test_the_worker_says_which_stream_a_frame_is_from_while_the_plugins_run(qapp):
    from telescope.stream import StreamWorker
    seen = []
    step = lambda f: seen.append(vision.current_frame.get()) or f  # noqa: E731
    worker = StreamWorker("url", None, None, 30, frame_pipeline=[step])
    worker._publish(7, np.zeros((4, 6, 3), np.uint8))
    assert seen == [(worker, 7)]
    assert vision.current_frame.get() is None
