"""The virtual camera: the wait screen, letting go for a driver reload, and telling whether an app reads it."""

import base64
import ctypes
import os
import sys
import threading
import time
import types

import numpy as np
import pytest

import telescope.vcam as vcam

_locked_size = vcam.locked_size  # conftest stubs it out for every test

# 8x4, three frames: red 100 ms, blue 300 ms, green 50 ms.
_GIF = base64.b64decode(
    "R0lGODlhCAAEAIEAAP8AAAAAAAAAAAAAACH/C05FVFNDQVBFMi4wAwEAAAAh+QQACgAAACwAAAAACAAEAAAIDAABCBxIsKDBgwQDAgAh+QQBHgABA"
    "CwAAAAACAAEAIEAAP8AAAAAAAAAAAAIDAABCBxIsKDBgwQDAgAh+QQBBQABACwAAAAACAAEAIEA/wAAAAAAAAAAAAAIDAABCBxIsKDBgwQDAgA7")


def _wait_for(cond, timeout=3.0):
    deadline = time.monotonic() + timeout
    while not cond():
        if time.monotonic() > deadline:
            raise AssertionError("timed out")
        time.sleep(0.01)


# ── Frames ────────────────────────────────────────────────────────────────────

def test_no_image_is_the_default_screen_at_the_camera_size(qapp):
    frames = vcam.load_frames(None, 64, 36)
    assert len(frames) == 1
    frame, seconds = frames[0]
    assert frame.shape == (36, 64, 3) and frame.dtype == np.uint8
    assert frame.std() > 0  # there's text on it
    assert seconds == vcam.STILL_PERIOD


def test_an_image_that_wont_load_falls_back_to_the_default(qapp, tmp_path):
    bad = tmp_path / "nope.png"
    bad.write_bytes(b"not an image")
    assert len(vcam.load_frames(str(bad), 64, 36)) == 1
    assert len(vcam.load_frames(str(tmp_path / "missing.gif"), 64, 36)) == 1


def test_a_photo_over_qts_allocation_limit_still_loads(qapp, tmp_path):
    from PyQt6.QtGui import QColor, QImage, QImageReader

    img = QImage(1200, 900, QImage.Format.Format_RGB32)
    img.fill(QColor(10, 200, 30))
    path = tmp_path / "big.jpg"
    assert img.save(str(path), "JPG", 95)
    old = QImageReader.allocationLimit()
    QImageReader.setAllocationLimit(1)  # MB: the 4.3 MB decode no longer fits
    try:
        assert QImageReader(str(path)).read().isNull()
        frames = vcam.read_image_frames(str(path), 64, 36)
        assert len(frames) == 1 and frames[0][0].shape == (36, 64, 3)
        assert abs(int(frames[0][0][18, 32][1]) - 200) < 12  # the photo, not the default screen
    finally:
        QImageReader.setAllocationLimit(old)


def test_a_normal_image_is_not_prescaled(qapp, tmp_path):
    from PyQt6.QtGui import QColor, QImage, QImageReader

    img = QImage(200, 100, QImage.Format.Format_RGB32)
    img.fill(QColor(255, 0, 0))
    path = tmp_path / "ok.png"
    img.save(str(path))
    reader = QImageReader(str(path))
    vcam._limit_decode_size(reader, 64)
    assert not reader.scaledSize().isValid()


def test_read_image_frames_is_empty_for_an_image_that_wont_load(qapp, tmp_path):
    bad = tmp_path / "bad.png"
    bad.write_bytes(b"not an image")
    assert vcam.read_image_frames(str(bad), 64, 36) == []


def test_an_animation_keeps_its_frames_and_timing_fitted_to_the_camera(qapp, tmp_path):
    gif = tmp_path / "wait.gif"
    gif.write_bytes(_GIF)
    frames = vcam.load_frames(str(gif), 16, 16)
    assert [round(s, 3) for _, s in frames] == [0.1, 0.3, round(vcam.MIN_GIF_PERIOD, 3)]
    red = frames[0][0]
    assert red.shape == (16, 16, 3)
    assert tuple(red[8, 8]) == (255, 0, 0)     # the 8x4 image scaled to fit, centred
    assert tuple(red[0, 8]) != (255, 0, 0)     # letterboxed above and below


def test_a_long_animation_is_thinned_to_the_memory_budget(qapp, tmp_path, monkeypatch):
    gif = tmp_path / "wait.gif"
    gif.write_bytes(_GIF)
    monkeypatch.setattr(vcam, "FRAME_BUDGET", 16 * 16 * 3 * 2)  # room for two frames of three
    frames = vcam.load_frames(str(gif), 16, 16)
    assert len(frames) == 2
    assert round(sum(s for _, s in frames), 3) == round(0.1 + 0.3 + vcam.MIN_GIF_PERIOD, 3)  # same loop length


def test_convert_runs_on_each_frame_as_it_loads(qapp):
    frames = vcam.load_frames(None, 16, 16, convert=lambda f: f[:, :, 0])
    assert frames[0][0].shape == (16, 16)


# ── The wait screen ───────────────────────────────────────────────────────────

class _Camera:
    def __init__(self, log, w, h, fps, fmt):
        self.log, self.size, self.fmt = log, (w, h), fmt
        self.sent = []
        self.closed = False
        log.append(self)

    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.closed = True
        return False

    def send(self, frame):
        self.sent.append(frame)


def _screen(loader=None, fail=0, locked=lambda: None, filter_open=lambda: True):
    cams, attempts = [], []

    def open_camera(w, h, fps, fmt):
        attempts.append((w, h))
        if len(attempts) <= fail:
            raise RuntimeError("no device")
        return _Camera(cams, w, h, fps, fmt)
    loader = loader or (lambda path, w, h, convert=None: [((convert or (lambda f: f))(
        np.zeros((h, w, 3), np.uint8)), vcam.STILL_PERIOD)])
    screen = vcam.WaitScreen(open_camera=open_camera, loader=loader, locked=locked,
                             filter_probe=lambda: filter_open)
    return screen, cams, attempts


def test_holds_the_camera_until_stopped(qapp):
    screen, cams, _ = _screen()
    screen.show((64, 36), None)
    _wait_for(lambda: cams and cams[0].sent)
    assert cams[0].size == (64, 36) and screen.running
    screen.stop()
    assert cams[0].closed and not screen.running


def test_a_wait_screen_on_an_extra_camera_opens_that_one(qapp):
    opened, asked = [], []

    def open_camera(w, h, fps, fmt, slot=0):
        opened.append(slot)
        return _Camera([], w, h, fps, fmt)
    loader = lambda path, w, h, convert=None: [(np.zeros((h, w, 3), np.uint8), vcam.STILL_PERIOD)]  # noqa: E731
    screen = vcam.WaitScreen(open_camera=open_camera, loader=loader,
                             locked=lambda slot=0: asked.append(slot), slot=2)
    screen.show((64, 36), None)
    _wait_for(lambda: opened)
    screen.stop()
    assert opened[0] == 2 and asked[0] == 2


def test_a_wait_screen_that_failed_to_prepare_starts_again_when_asked(qapp):
    calls = []

    def loader(path, w, h, convert=None):
        calls.append(1)
        if len(calls) == 1:
            raise OSError("image unreadable for a moment")
        return [((convert or (lambda f: f))(np.zeros((h, w, 3), np.uint8)), vcam.STILL_PERIOD)]

    screen, cams, _ = _screen(loader=loader)
    screen.show((64, 36), None)
    _wait_for(lambda: not screen.running)
    screen.show((64, 36), None)
    _wait_for(lambda: cams and cams[0].sent)
    screen.stop()


@pytest.mark.parametrize("linux", [True, False])
def test_mirror_flips_the_screen_left_to_right(qapp, monkeypatch, linux):
    monkeypatch.setattr(vcam, "IS_LINUX", linux)
    image = np.zeros((36, 64, 3), np.uint8)
    image[:, :32] = 255  # left half white
    loader = lambda path, w, h, convert=None: [((convert or (lambda f: f))(image), vcam.STILL_PERIOD)]  # noqa: E731
    screen, cams, _ = _screen(loader=loader)
    screen.show((64, 36), None, mirror=True)
    _wait_for(lambda: cams and cams[0].sent)
    screen.stop()
    frame = cams[0].sent[0]
    left, right = (frame[:, :32], frame[:, 32:]) if not linux else (frame[:36, :32], frame[:36, 32:])
    assert left.mean() < right.mean()  # the white half is on the right now


def test_opens_at_the_size_a_reader_holds_the_camera_at(qapp):
    locked = [(640, 480)]
    screen, cams, _ = _screen(locked=lambda: locked[0])
    screen.show((64, 36), None)
    _wait_for(lambda: cams and cams[0].sent)
    screen.stop()
    assert cams[0].size == (640, 480) and cams[0].sent[0].shape[1] == 640


@pytest.mark.parametrize("text, size", [
    ("YU12:1280x720@30\n", (1280, 720)),
    ("", None),
    ("garbage", None),
])
def test_locked_size_reads_the_drivers_format(monkeypatch, tmp_path, text, size):
    monkeypatch.setattr(vcam, "IS_LINUX", True)
    fmt = tmp_path / "format"
    fmt.write_text(text)
    real_open = open
    monkeypatch.setattr("builtins.open", lambda path, *a, **k: real_open(
        fmt if str(path).startswith("/sys/class/video4linux/") else path, *a, **k))
    assert _locked_size() == size


def test_extra_slots_are_found_by_their_label(monkeypatch, tmp_path):
    monkeypatch.setattr(vcam, "IS_LINUX", True)
    for node, label in (("video11", "Phone Camera"), ("video20", "Phone Camera 3"), ("video21", "Other")):
        (tmp_path / node).mkdir()
        (tmp_path / node / "name").write_text(label + "\n")
    monkeypatch.setattr(vcam.glob, "glob", lambda _pattern: [str(p) for p in tmp_path.glob("video*/name")])
    real_open = open
    monkeypatch.setattr("builtins.open", lambda path, *a, **k: real_open(
        tmp_path / str(path).split("/")[-2] / "name" if str(path).startswith("/sys/") else path, *a, **k))
    assert vcam.slot_device(0) == vcam.V4L2_PHONE_DEV
    assert vcam.slot_device(2) == "/dev/video20"
    assert vcam.slot_device(1) is None
    assert vcam.slot_ready(2) and not vcam.slot_ready(1) and vcam.slot_ready(0)
    with pytest.raises(RuntimeError, match="Phone Camera 2"):
        vcam.open_camera(4, 4, 30, slot=1)


def test_windows_slots_need_their_numbered_registration(monkeypatch):
    monkeypatch.setattr(vcam, "IS_LINUX", False)
    monkeypatch.setattr(vcam, "_uc_names", lambda: {0: "Telescope", 2: "Telescope #2"})
    assert vcam.slot_label(1) == "Telescope #2"
    assert vcam.slot_ready(1) and not vcam.slot_ready(2)


def test_locked_size_is_linux_only(monkeypatch):
    monkeypatch.setattr(vcam, "IS_LINUX", False)
    assert _locked_size() is None


def test_linux_sends_the_cameras_own_format_prepared_once(qapp, monkeypatch):
    monkeypatch.setattr(vcam, "IS_LINUX", True)
    screen, cams, _ = _screen()
    screen.show((64, 36), None)
    _wait_for(lambda: cams and cams[0].sent)
    screen.stop()
    assert cams[0].fmt == vcam.pyvirtualcam.PixelFormat.I420
    assert cams[0].sent[0].shape == (36 * 3 // 2, 64)


def test_odd_sizes_and_windows_send_rgb(qapp, monkeypatch):
    monkeypatch.setattr(vcam, "IS_LINUX", True)
    screen, cams, _ = _screen()
    screen.show((63, 36), None)
    _wait_for(lambda: cams and cams[0].sent)
    screen.stop()
    assert cams[0].fmt == vcam.pyvirtualcam.PixelFormat.RGB


def test_sends_rarely_until_someone_watches(qapp, monkeypatch):
    monkeypatch.setattr(vcam, "IDLE_PERIOD", 0.3)
    screen, cams, _ = _screen()
    screen.show((8, 8), None)
    _wait_for(lambda: cams and cams[0].sent)
    time.sleep(0.2)
    assert len(cams[0].sent) == 1
    screen.set_watched(True)  # sends straight away, then at the still rate
    _wait_for(lambda: len(cams[0].sent) >= 3, timeout=1.0)
    screen.stop()


def test_windows_sends_the_moment_an_app_opens_the_camera(qapp, monkeypatch):
    monkeypatch.setattr(vcam, "IDLE_PERIOD", 5.0)
    monkeypatch.setattr(vcam, "FILTER_POLL", 0.01)
    polls = []
    screen, cams, _ = _screen(filter_open=lambda: polls.append(1) or len(polls) > 5)
    screen.show((8, 8), None)
    _wait_for(lambda: cams and len(cams[0].sent) >= 2, timeout=1.0)  # not the 5 s idle pace
    time.sleep(0.1)
    assert len(cams[0].sent) == 2 and len(polls) == 7  # then one look per idle send, not every 10 ms
    screen.stop()


def test_a_filter_never_found_still_gets_the_idle_pace(qapp, monkeypatch):
    monkeypatch.setattr(vcam, "IDLE_PERIOD", 0.1)
    monkeypatch.setattr(vcam, "FILTER_POLL", 0.01)
    screen, cams, _ = _screen(filter_open=lambda: False)
    screen.show((8, 8), None)
    _wait_for(lambda: cams and len(cams[0].sent) >= 3, timeout=1.0)
    screen.stop()


def test_a_camera_that_wont_open_is_tried_again(qapp, monkeypatch):
    monkeypatch.setattr(vcam, "RETRY_OPEN", 0.01)
    screen, cams, attempts = _screen(fail=2)
    screen.show((8, 8), None)
    _wait_for(lambda: cams)
    screen.stop()
    assert len(attempts) == 3


def test_showing_the_same_thing_again_keeps_the_camera_open(qapp):
    screen, cams, _ = _screen()
    screen.show((8, 8), None)
    _wait_for(lambda: cams)
    screen.show((8, 8), None)
    assert len(cams) == 1
    screen.show((16, 16), None)  # a new size reopens it
    _wait_for(lambda: len(cams) == 2)
    screen.stop()
    assert cams[0].closed and cams[1].size == (16, 16)


def test_lets_go_for_a_driver_reload_and_takes_it_back(qapp):
    screen, cams, _ = _screen()
    screen.show((8, 8), None)
    _wait_for(lambda: cams)
    with vcam.device_released():
        assert cams[0].closed and not screen.running
        screen.show((8, 8), None)  # e.g. a stream stopping meanwhile: waits for the reload
        assert not screen.running
    _wait_for(lambda: len(cams) == 2)
    screen.stop()


def test_a_stopped_screen_stays_stopped_after_a_reload(qapp):
    screen, cams, _ = _screen()
    screen.show((8, 8), None)
    _wait_for(lambda: cams)
    screen.stop()
    with vcam.device_released():
        pass
    assert not screen.running and len(cams) == 1


# ── Watching ──────────────────────────────────────────────────────────────────

def test_watch_reports_only_changes():
    seen = []
    watch = vcam.CameraWatch(seen.append)
    for v in (True, True, False, False, True):
        watch._report(v)
    assert seen == [True, False, True]
    assert watch.watched is True


def test_old_drivers_fall_back_to_looking_for_other_processes(monkeypatch):
    seen = []
    holders = iter([[], [4242], []])
    monkeypatch.setattr(vcam, "camera_holders", lambda _dev: next(holders, []))
    monkeypatch.setattr(vcam, "PROC_SCAN_PERIOD", 0.01)
    watch = vcam.CameraWatch(seen.append)
    stop = threading.Event()
    t = threading.Thread(target=watch._scan_proc, args=(stop,))
    t.start()
    _wait_for(lambda: len(seen) >= 3)
    stop.set()
    t.join()
    assert seen[:3] == [False, True, False]


def test_camera_holders_skips_this_process(tmp_path):
    assert vcam.camera_holders(str(tmp_path / "missing")) == []
    assert os.getpid() not in vcam.camera_holders("/dev/null")


def test_unitycapture_event_follows_the_camera_number(monkeypatch):
    names = {0x10: "Unity Video Capture", 0x12: "Telescope"}
    offset_key = lambda key: int(key[-3:-1], 16)  # noqa: E731

    class Key:
        def __init__(self, key):
            if offset_key(key) not in names:
                raise OSError
            self.key = key

        def __enter__(self):
            return self

        def __exit__(self, *_):
            return False

    fake = types.SimpleNamespace(
        HKEY_CLASSES_ROOT=object(),
        OpenKey=lambda _root, key: Key(key),
        QueryValueEx=lambda k, _n: (names[offset_key(k.key)], 1),
    )
    monkeypatch.setitem(sys.modules, "winreg", fake)
    monkeypatch.setattr(sys, "maxsize", 2**63 - 1)
    assert vcam._uc_object_name("Want", "Telescope") == "UnityCapture_Want1"
    assert vcam._uc_object_name("Data", "Something else") == "UnityCapture_Data"
    names.pop(0x10)
    assert vcam._uc_object_name("Want", "Something else") == "UnityCapture_Want1"


def test_windows_watch_counts_a_camera_no_app_has_opened_as_unwatched(monkeypatch):
    # UnityCapture's "Want" event only exists once an app's filter has opened the camera, so a stream started
    # before any app came along still needs an answer, or the idle stop never starts (#115).
    k32 = types.SimpleNamespace(
        OpenEventW=lambda *_a: None,  # no app's filter has made the event
        WaitForSingleObject=lambda *_a: 0x102,  # WAIT_TIMEOUT: no app wants a frame
        CloseHandle=lambda _h: None,
    )
    for fn in vars(k32).values():
        fn.restype = fn.argtypes = None
    monkeypatch.setattr(ctypes, "WinDLL", lambda *_a, **_k: k32, raising=False)
    monkeypatch.setattr(vcam, "_uc_object_name", lambda kind: "UnityCapture_" + kind)
    monkeypatch.setattr(vcam, "RETRY_OPEN", 0.01)
    seen = []
    watch = vcam.CameraWatch(seen.append)
    stop = threading.Event()
    t = threading.Thread(target=watch._run_windows, args=(stop,))
    t.start()
    time.sleep(0.05)
    stop.set()
    t.join()
    assert seen == [False]


@pytest.mark.skipif(not vcam.IS_LINUX, reason="v4l2loopback")
def test_linux_watch_retries_while_the_device_is_missing(monkeypatch, tmp_path):
    monkeypatch.setattr(vcam, "V4L2_PHONE_DEV", str(tmp_path / "video99"))
    monkeypatch.setattr(vcam, "RETRY_OPEN", 0.01)
    watch = vcam.CameraWatch(lambda _w: None)
    watch.start()
    time.sleep(0.05)
    watch.stop()
    assert not watch.running
