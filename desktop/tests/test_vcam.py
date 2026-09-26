"""The virtual camera: the wait screen, letting go for a driver reload, and telling whether an app reads it."""

import base64
import os
import sys
import threading
import time
import types

import numpy as np
import pytest

import telescope.vcam as vcam

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


def _screen(loader=None, fail=0):
    cams, attempts = [], []

    def open_camera(w, h, fps, fmt):
        attempts.append((w, h))
        if len(attempts) <= fail:
            raise RuntimeError("no device")
        return _Camera(cams, w, h, fps, fmt)
    loader = loader or (lambda path, w, h, convert=None: [((convert or (lambda f: f))(
        np.zeros((h, w, 3), np.uint8)), vcam.STILL_PERIOD)])
    return vcam.WaitScreen(open_camera=open_camera, loader=loader), cams, attempts


def test_holds_the_camera_until_stopped(qapp):
    screen, cams, _ = _screen()
    screen.show((64, 36), None)
    _wait_for(lambda: cams and cams[0].sent)
    assert cams[0].size == (64, 36) and screen.running
    screen.stop()
    assert cams[0].closed and not screen.running


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
    assert vcam._uc_want_event_name("Telescope") == "UnityCapture_Want1"
    assert vcam._uc_want_event_name("Something else") == "UnityCapture_Want"
    names.pop(0x10)
    assert vcam._uc_want_event_name("Something else") == "UnityCapture_Want1"


@pytest.mark.skipif(not vcam.IS_LINUX, reason="v4l2loopback")
def test_linux_watch_retries_while_the_device_is_missing(monkeypatch, tmp_path):
    monkeypatch.setattr(vcam, "V4L2_PHONE_DEV", str(tmp_path / "video99"))
    monkeypatch.setattr(vcam, "RETRY_OPEN", 0.01)
    watch = vcam.CameraWatch(lambda _w: None)
    watch.start()
    time.sleep(0.05)
    watch.stop()
    assert not watch.running
