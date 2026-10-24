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


# A bit more than Windows' clock step (about 16 ms), so turning a clock back by an interval is past it there too
_TICK = 0.02


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
    _in_stream("a", 2, lambda: v.person_mask(np.full((4, 6, 3), 255, np.uint8)))
    assert first is again and first.shape == (4, 6) and first.dtype == np.uint8
    assert other is not first
    assert len(backend.segmented) == 3
    assert backend.timestamps[2] > backend.timestamps[0]  # each stream's go up, as VIDEO mode needs
    assert states == [LOADING, READY]


@pytest.mark.parametrize("h, w, given", [(720, 1280, (144, 256)), (1080, 1920, (144, 256)), (1280, 720, (455, 256)),
                                         (100, 160, (100, 160))])
def test_the_segmenter_gets_a_small_copy_and_the_mask_comes_back_at_the_frames_size(h, w, given):
    v, backend, _states, _people = _ready()
    mask = _in_stream("a", 1, lambda: v.person_mask(np.zeros((h, w, 3), np.uint8)))
    assert backend.segmented[0].shape[:2] == given  # just big enough to cover the model's input, same shape
    assert mask.shape == (h, w) and mask.dtype == np.uint8


def test_while_nothing_moves_the_mask_is_worked_out_less_often():
    v, backend, _states, _people = _ready()
    still = np.full((720, 1280, 3), 80, np.uint8)
    masks = [_in_stream("a", seq, lambda: v.person_mask(still)) for seq in (1, 2, 3)]
    assert len(backend.segmented) == 1 and masks[1] is masks[0] and masks[2] is masks[0]
    v._streams["a"].mask_at -= vision._MASK_STILL_EVERY_S + _TICK
    _in_stream("a", 4, lambda: v.person_mask(still))
    assert len(backend.segmented) == 2  # still, but it's been a while
    moved = still.copy()
    moved[200:500, 400:700] = 200  # someone's arm
    _in_stream("a", 5, lambda: v.person_mask(moved))
    assert len(backend.segmented) == 3  # straight away


def test_more_of_the_last_mask_carries_over_while_nothing_moves():
    v, backend, _states, _people = _ready()
    values = iter((200, 0, 0))
    backend.segmenter = lambda: (lambda frame, _ts: np.full(frame.shape[:2], next(values), np.uint8), lambda: None)
    still = np.full((720, 1280, 3), 80, np.uint8)
    _in_stream("a", 1, lambda: v.person_mask(still))
    v._streams["a"].mask_at -= vision._MASK_STILL_EVERY_S + _TICK
    calm = _in_stream("a", 2, lambda: v.person_mask(still))
    assert calm[0, 0] == round(200 * vision._MASK_CARRY_STILL)
    moved = still.copy()
    moved[200:500, 400:700] = 200
    quick = _in_stream("a", 3, lambda: v.person_mask(moved))
    assert quick[0, 0] == round(calm[0, 0] * vision._MASK_CARRY)


def test_camera_noise_alone_doesnt_count_as_moving():
    rng = np.random.default_rng(1)
    base = rng.integers(60, 200, (720, 1280, 3), dtype=np.uint8)
    motion = vision._Motion()
    assert motion.moved(base)  # nothing to compare with yet
    motion.used()
    noisy = np.clip(base.astype(int) + rng.integers(-15, 16, base.shape), 0, 255).astype(np.uint8)
    assert not motion.moved(noisy)
    moved = base.copy()
    moved[300:420, 600:720] = 255  # about 1.5% of the picture
    assert motion.moved(moved)


def test_model_input_looks_like_one_area_shrink():
    import cv2
    rng = np.random.default_rng(2)
    frame = cv2.GaussianBlur(rng.integers(0, 255, (1080, 1920, 3), dtype=np.uint8), (0, 0), 3)
    small = vision.model_input(frame)
    area = cv2.resize(frame, (256, 144), interpolation=cv2.INTER_AREA)
    assert small.shape == area.shape and np.abs(small.astype(int) - area).mean() < 1.0
    tiny = np.zeros((90, 160, 3), np.uint8)
    assert vision.model_input(tiny) is tiny  # already small enough


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


@pytest.mark.parametrize("error", [None, FileNotFoundError, OSError])
def test_a_loader_finishing_after_shutdown_does_not_notify_the_window(error):
    entered, finish = threading.Event(), threading.Event()
    states, loaders = [], []

    def find():
        loaders.append(threading.current_thread())
        entered.set()
        assert finish.wait(2)
        if error:
            raise error("test load failed")
        return _Backend()

    v = Vision(on_people=lambda *_: None, on_state=states.append, find=find)
    v.load()
    try:
        assert entered.wait(2)
        v.shutdown()
    finally:
        finish.set()
        if loaders:
            loaders[0].join(2)
    assert not loaders[0].is_alive()
    assert states == [LOADING]
    v.load()
    assert len(loaders) == 1


def test_shutdown_waits_for_a_state_callback_already_in_progress():
    entered, finish, stopped = threading.Event(), threading.Event(), threading.Event()
    loaders = []

    def find():
        loaders.append(threading.current_thread())
        return _Backend()

    def on_state(state):
        if state == READY:
            entered.set()
            assert finish.wait(2)

    v = Vision(on_people=lambda *_: None, on_state=on_state, find=find)
    v.load()
    assert entered.wait(2)

    def stop():
        v.shutdown()
        stopped.set()

    shutdown = threading.Thread(target=stop)
    shutdown.start()
    try:
        assert not stopped.wait(0.05)
    finally:
        finish.set()
        shutdown.join(2)
        loaders[0].join(2)
    assert stopped.is_set()


def test_people_detection_finishing_after_shutdown_does_not_notify_the_window():
    entered, finish = threading.Event(), threading.Event()
    v, backend, _states, people = _ready()

    def pose():
        def detect(_frame, _timestamp):
            entered.set()
            assert finish.wait(2)
            return [Person({})]
        return detect, lambda: backend.closed.append("pose")

    backend.pose = pose
    v.want_people("test", True)
    _in_stream("a", 1, lambda: v.feed_raw(np.zeros((8, 8, 3), np.uint8)))
    worker = v._streams["a"].people_thread
    try:
        assert entered.wait(2)
        v.shutdown()
    finally:
        finish.set()
        worker.join(2)
    assert not worker.is_alive()
    assert people == [] and backend.closed == ["pose"]


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


def test_people_are_looked_for_less_often_while_nothing_moves(monkeypatch):
    v, _backend, _states, _people = _ready()
    v.want_people("x", True)
    monkeypatch.setattr(vision.threading, "Thread", lambda **kw: type("T", (), {"start": lambda self: None})())
    still = np.full((720, 1280, 3), 80, np.uint8)
    moved = still.copy()
    moved[200:500, 400:700] = 200
    sent = []

    def feed(seq, frame, ago):
        _in_stream("a", seq, lambda: v.feed_raw(frame))
        sv = v._streams["a"]
        sent.append(sv.people_frame is not None)
        sv.people_frame = None
        sv.people_at -= ago + _TICK  # time passing until the next one
    feed(1, still, vision._PEOPLE_EVERY_S)
    feed(2, still, vision._PEOPLE_STILL_EVERY_S)
    feed(3, still, vision._PEOPLE_EVERY_S)
    feed(4, moved, 0)
    assert sent == [True, False, True, True]


def test_power_saving_runs_the_models_less_often_even_while_something_moves(monkeypatch):
    v, backend, _states, _people = _ready()
    monkeypatch.setattr(vision, "_saving", True)
    moving = [np.full((720, 1280, 3), 80, np.uint8) for _ in range(3)]
    for i, frame in enumerate(moving):
        frame[100 * i:100 * i + 300, 400:700] = 200  # an arm going up
    for seq, frame in enumerate(moving):
        _in_stream("a", seq, lambda: v.person_mask(frame))
    assert len(backend.segmented) == 1  # moving, but not 15 times a second yet
    v._streams["a"].mask_at -= vision._MASK_STILL_EVERY_S + _TICK
    _in_stream("a", 9, lambda: v.person_mask(moving[0]))
    assert len(backend.segmented) == 2
    assert "power saving" in v.diagnostics()["Vision"]

    v.want_people("x", True)
    monkeypatch.setattr(vision.threading, "Thread", lambda **kw: type("T", (), {"start": lambda self: None})())
    sent = []

    def feed(seq, frame, ago):
        _in_stream("a", seq, lambda: v.feed_raw(frame))
        sv = v._streams["a"]
        sent.append(sv.people_frame is not None)
        sv.people_frame = None
        sv.people_at -= ago + _TICK
    feed(10, moving[0], vision._PEOPLE_EVERY_S)
    feed(11, moving[1], vision._SAVING_PEOPLE_EVERY_S)
    feed(12, moving[2], 0)
    assert sent == [True, False, True]  # moving, but every _SAVING_PEOPLE_EVERY_S instead of _PEOPLE_EVERY_S


class _CountingBackend(_Backend):
    def __init__(self):
        super().__init__()
        self.made = []

    def segmenter(self):
        self.made.append("segmenter")
        return super().segmenter()

    def pose(self):
        self.made.append("pose")
        return super().pose()


def test_a_mask_nobody_asks_for_closes_after_a_while_and_comes_back_when_asked(monkeypatch):
    backend = _CountingBackend()
    v = Vision(on_people=lambda *_: None, on_state=lambda _s: None, find=lambda: backend)
    v.load()
    _wait(lambda: v.state() == READY)
    released = []
    monkeypatch.setattr(vision, "_give_back_memory", lambda: released.append(1))
    frame = np.zeros((720, 1280, 3), np.uint8)
    _in_stream("a", 1, lambda: v.person_mask(frame))
    asked = v._streams["a"].mask_asked
    v.tidy(asked + vision._IDLE_CLOSE_S - 1)  # Background switched off a moment ago: kept
    assert backend.closed == [] and released == []
    v.tidy(asked + vision._IDLE_CLOSE_S + _TICK)
    assert backend.closed == ["segmenter"] and released == [1]
    assert _in_stream("a", 2, lambda: v.person_mask(frame)) is not None  # switched back on: made again
    assert backend.made == ["segmenter", "segmenter"]
    v.shutdown()


def test_flipping_auto_framing_on_and_off_keeps_the_pose_model_loaded():
    backend = _CountingBackend()
    people = []
    v = Vision(on_people=lambda key, found: people.append(found), on_state=lambda _s: None, find=lambda: backend)
    v.load()
    _wait(lambda: v.state() == READY)
    frame = np.zeros((720, 1280, 3), np.uint8)
    for seq in range(20):
        v.want_people("Auto framing", seq % 2 == 0)  # someone spamming the switch
        _in_stream("a", seq, lambda: v.feed_raw(frame))
        v._streams["a"].people_at -= vision._PEOPLE_EVERY_S + _TICK
        v.tidy()
    _wait(lambda: people)
    assert backend.made == ["pose"] and "pose" not in backend.closed


def test_a_pose_model_nobody_feeds_closes_in_its_own_thread_and_comes_back(monkeypatch):
    backend = _CountingBackend()
    people = []
    v = Vision(on_people=lambda key, found: people.append(found), on_state=lambda _s: None, find=lambda: backend)
    v.load()
    _wait(lambda: v.state() == READY)
    released = []
    monkeypatch.setattr(vision, "_give_back_memory", lambda: released.append(1))
    v.want_people("Auto framing", True)
    frame = np.zeros((720, 1280, 3), np.uint8)
    _in_stream("a", 1, lambda: v.feed_raw(frame))
    _wait(lambda: people)
    v.want_people("Auto framing", False)
    v.tidy(v._streams["a"].people_fed + vision._IDLE_CLOSE_S + _TICK)
    _wait(lambda: "pose" in backend.closed and released)
    assert v._streams["a"].people_thread is None
    v.want_people("Auto framing", True)
    people.clear()
    _in_stream("a", 2, lambda: v.feed_raw(frame))
    _wait(lambda: people)
    assert backend.made == ["pose", "pose"]
    v.shutdown()


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
