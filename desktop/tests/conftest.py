import atexit
import gc
import os
import shutil
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

# A save that fires after its test (a window's debounced save timer) must never reach the real config.
_CONFIG_HOME = tempfile.mkdtemp(prefix="telescope-tests-")
atexit.register(shutil.rmtree, _CONFIG_HOME, ignore_errors=True)
os.environ["XDG_CONFIG_HOME"] = os.path.join(_CONFIG_HOME, "xdg")
os.environ["APPDATA"] = os.path.join(_CONFIG_HOME, "appdata")

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import pytest


@pytest.fixture(autouse=True)
def _no_desktop_notifications(monkeypatch):
    """A test never pops a real notification up on the desktop it runs on (notify-send is a no-op here)."""
    class Popen(subprocess.Popen):  # a class, since modules imported during a test subclass it (asyncio on Windows)
        def __init__(self, args, *a, **kw):
            if isinstance(args, (list, tuple)) and args and Path(str(args[0])).name == "notify-send":
                args, a, kw = [sys.executable, "-c", ""], (), {"stdout": subprocess.DEVNULL, "stderr": subprocess.DEVNULL}
            super().__init__(args, *a, **kw)
    monkeypatch.setattr(subprocess, "Popen", Popen)


@pytest.fixture
def config_home(tmp_path, monkeypatch):
    """Reload module to pick up monkeypatched env vars instead of using real home."""
    monkeypatch.setenv("XDG_CONFIG_HOME", str(tmp_path / "xdg"))
    monkeypatch.setenv("APPDATA", str(tmp_path / "appdata"))
    monkeypatch.setattr(Path, "home", lambda: tmp_path / "home")
    import importlib
    import telescope.config as config
    importlib.reload(config)
    yield config
    importlib.reload(config)


@pytest.fixture(scope="session")
def qapp():
    from PyQt6.QtWidgets import QApplication
    app = QApplication.instance() or QApplication([])
    # Applied before any test makes a widget: doing it later restyles the ones still alive, which crashes Qt, and
    # whether that happens would depend on which tests share a process (pytest -n) instead of being the same every run
    from telescope.theme import apply_theme
    apply_theme(app)
    yield app


# How long a test's threads get to finish after its teardown asked them to stop.
THREAD_GRACE_S = 5.0
# Process-wide pools that idle between uses and live as long as the app does.
_PROCESS_THREADS = ("colour-lut", "browser-rtc", "asyncio_")

_callback_errors = []


def _record_callback_error(exc_type, exc, tb):
    """Without a hook, PyQt aborts the whole process on an exception in a slot or timer, and the run loses a worker."""
    _callback_errors.append(exc)
    sys.__excepthook__(exc_type, exc, tb)


def pytest_configure(config):
    sys.excepthook = _record_callback_error
    # Python's own collection runs in whichever thread allocates, and destroying a widget off the GUI thread crashes
    # Qt; the fixture below collects on the GUI thread instead, between tests
    gc.disable()


@pytest.fixture(autouse=True)
def _nothing_outlives_its_test(qapp):
    """Leaves Qt as each test found it: whatever a test leaves running fires into a later one and crashes Qt there."""
    from PyQt6 import sip
    from PyQt6.QtCore import QEvent, QTimer
    from PyQt6.QtWidgets import QApplication
    import telescope.theme as theme
    gc.collect()  # the last test's fixture values, let go of after its teardown
    gc.freeze()  # so the check below only looks at what this test makes
    threads = set(threading.enumerate())
    windows = set(QApplication.topLevelWidgets())
    app_state = qapp.styleSheet(), qapp.font(), qapp.palette(), theme._own_hues
    _callback_errors.clear()
    yield
    for window in QApplication.topLevelWidgets():
        if window not in windows:
            window.deleteLater()
    for _ in range(3):  # a destructor can schedule more deletions
        qapp.sendPostedEvents(None, QEvent.Type.DeferredDelete)
        qapp.processEvents()
        gc.collect()
    deadline = time.monotonic() + THREAD_GRACE_S
    for thread in set(threading.enumerate()) - threads:
        if not thread.name.startswith(_PROCESS_THREADS):
            thread.join(max(0.0, deadline - time.monotonic()))
    running = sorted(t.name for t in set(threading.enumerate()) - threads if not t.name.startswith(_PROCESS_THREADS))
    timers = [o for o in gc.get_objects() if isinstance(o, QTimer) and not sip.isdeleted(o) and o.isActive()]
    for timer in timers:
        timer.stop()
    if qapp.styleSheet() != app_state[0]:
        qapp.setStyleSheet(app_state[0])
    if qapp.font() != app_state[1]:
        qapp.setFont(app_state[1])
    if qapp.palette() != app_state[2]:
        qapp.setPalette(app_state[2])
    theme._own_hues = app_state[3]
    problems = [f"threads still running: {', '.join(running)}"] if running else []
    if timers:
        problems.append("timers still running: " + ", ".join(
            f"{type(t.parent()).__name__ if t.parent() else 'unparented'} every {t.interval()} ms" for t in timers))
    problems += [f"{type(e).__name__} in a Qt callback: {e}" for e in _callback_errors]
    if problems:
        pytest.fail("Left behind by this test: " + "; ".join(problems), pytrace=False)


@pytest.fixture(autouse=True)
def _visions_shut_down(monkeypatch):
    """Every Vision a test makes is shut down after it, as the window does when it quits, so its threads end."""
    from telescope.vision import Vision
    made = []
    init = Vision.__init__

    def tracked(self, *args, **kwargs):
        init(self, *args, **kwargs)
        made.append(self)

    monkeypatch.setattr(Vision, "__init__", tracked)
    yield
    for vision in made:
        vision.shutdown()


class Timing:
    """Times code against a piece of reference frame work run alongside it, so a slow or busy runner slows both."""

    def __init__(self):
        import numpy as np
        self._frame = np.random.default_rng(0).integers(0, 255, (1080, 1920, 3), np.uint8)

    def _reference(self):
        import cv2
        cv2.resize(self._frame, (960, 540), interpolation=cv2.INTER_AREA).copy()

    def relative(self, fn, runs: int = 10) -> float:
        """How many reference pieces fn takes, best run against best run: a run the scheduler interrupted counts for
        neither."""
        import cv2
        threads = cv2.getNumThreads()
        cv2.setNumThreads(1)  # so the ratio doesn't depend on how many cores the runner has
        try:
            fn()  # warm caches and lazily made buffers
            best_fn = best_ref = float("inf")
            for _ in range(runs):
                start = time.perf_counter()
                self._reference()
                best_ref = min(best_ref, time.perf_counter() - start)
                start = time.perf_counter()
                fn()
                best_fn = min(best_fn, time.perf_counter() - start)
        finally:
            cv2.setNumThreads(threads)
        return best_fn / best_ref


@pytest.fixture
def timing():
    return Timing()


@pytest.fixture
def qt_widgets(qapp):
    """Own registered widget roots until their deferred deletion finishes."""
    from PyQt6 import sip
    from PyQt6.QtCore import QEvent
    widgets = []

    def keep(widget):
        widgets.append(widget)
        return widget

    yield keep
    for widget in reversed(widgets):
        if not sip.isdeleted(widget):
            widget.close()
            widget.deleteLater()
    qapp.sendPostedEvents(None, QEvent.Type.DeferredDelete)
    qapp.processEvents()


@pytest.fixture(autouse=True)
def _no_locked_camera(monkeypatch):
    """Whatever the real virtual camera is doing on this machine stays out of the tests."""
    import telescope.vcam as vcam
    monkeypatch.setattr(vcam, "locked_size", lambda *_: None)


@pytest.fixture(autouse=True)
def _on_air_sign_stays_local(monkeypatch):
    """A window that runs the On Air sign listens on a free localhost port, not on the network at 8768."""
    from telescope.on_air import OnAirServer
    monkeypatch.setattr(OnAirServer.__init__, "__defaults__", (0, "127.0.0.1"))


@pytest.fixture(autouse=True)
def _no_real_webcams(monkeypatch):
    """Webcams plugged into this machine stay out of the tests: none are found or touched unless a test gives some."""
    import telescope.plugins.webcam as webcam
    monkeypatch.setattr(webcam, "default_scanner", lambda: lambda: [])
    monkeypatch.setattr(webcam, "default_controls", lambda: lambda device: None)
