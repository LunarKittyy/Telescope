"""What the window streams from: a paired phone, or a plugin's StreamSource (Browser camera).

Each source keeps the state of its own stream, so the window can run several and point its panels at any of them. A phone also
does what only a phone can: wake its camera, find another route when the stream drops, report its state, and turn its
camera off while its mic carries on.
"""

import logging
import threading
import time
from dataclasses import replace
from typing import TYPE_CHECKING, Optional

from PyQt6.QtCore import QObject, QTimer, pyqtSignal

from telescope import vcam
from telescope.phone_client import PhoneControlClient
from telescope.phones import DESKTOP_OUTDATED, LOCAL_ONLY, NOT_PAIRED, PHONE_OUTDATED, READY
from telescope.widgets.banner import BannerAction, Issue

if TYPE_CHECKING:
    from telescope.app import TelescopeWindow
    from telescope.session import StreamSession

_RECOVER_RETRY_MS = 3000  # a dropped stream: how often to look for a route back to the phone
_ALIVE_POLL_MS = 5000  # camera off: how often to check the phone is still there, with no video to tell
_ALIVE_MISSES = 2  # checks in a row the phone didn't answer before looking for it like a dropped stream
_CAMERA_ON_CHECK_MS = 6000  # after turning the camera on: when to ask the phone whether it came on
# Frames arriving at 90% of what the phone's camera makes means the link keeps up with the camera; how long the camera's
# rate is trusted before asking again.
_CAMERA_LIMITED_SHARE = 0.9
_CAMERA_FPS_KEEP_S = 10.0


class Source(QObject):
    """Somewhere to stream from, and the state of its stream. The defaults suit a source with nothing to wake, no
    other route to look for and no state to ask about."""

    id: Optional[str] = None
    name = ""
    url = ""
    auth = None
    wakes = False  # Start waits for wake() to report back before the stream begins
    mic_alone = False  # can stream its mic with the camera off
    open_reader = None  # a StreamWorker reader factory; None reads url
    auto_canvas = None  # the virtual camera's size when Setup's canvas is Auto; None takes the first frame's

    def __init__(self, win: "TelescopeWindow"):
        super().__init__()
        self._win = win
        # Camera off: the stream is the mic alone, with no video worker. Before a start it means Start mic only.
        self.camera_off = False
        self.camera_off_auto = False  # Automatic streaming turned it off, so an app opening the camera turns it on
        self.camera_toggle: Optional[bool] = None  # whether this stream's phone can turn its camera off; None: not known yet
        self.recovering = False  # a dropped stream looking for its way back
        # Whether the stream is falling behind (bus.stream_behind), and throughput reports to skip before judging it.
        self.behind = False
        self.settling_reports = 0
        # Frames coming in under the rate asked for: the link, or a camera making fewer (dim light slows it down)?
        self.arrival_slow = False
        # In-flight resolution change; cleared on confirm or timeout.
        self.pending_resolution: Optional[tuple[int, int]] = None
        self.pending_resolution_timer: Optional[QTimer] = None
        self.session: Optional["StreamSession"] = None  # while it streams
        self.slot = 0  # which virtual camera it streams to (vcam.MAX_SLOTS)
        self.last_state: Optional[dict] = None  # the phone's latest state, for the panels when they come back to it
        self.forget_stream_status()

    def forget_stream_status(self):
        """What the footer shows for this stream while the panels show another, back to before it started."""
        self.status = ("Not streaming", "dim")
        self.fps_text = self.net_text = "—"
        self.connected = False
        self.last_state = None

    def note_status(self, kind: str, msg: str):
        """A status report from this stream's worker, kept for the footer and its tile."""
        if kind == "fps":
            self.fps_text = msg
        elif kind in ("net", "net_warn"):
            self.net_text = msg
        elif kind == "ok":
            self.connected = True
            self.status = (msg, "ok")
        elif kind in ("waiting", "reconnecting"):
            self.connected = False
            self.status = (msg, "warn")
        elif kind == "idle":
            self.status = (msg, "dim")
        else:
            self.status = (msg, kind if kind in ("warn", "err") else "dim")

    remember_changes_only = False  # its settings are saved only where they differ from the defaults

    def prepare(self, interactive: bool) -> bool:
        """GUI thread, at Start: get ready, or show a banner and return False."""
        return True

    def redirect(self) -> Optional[str]:
        """At Start with nothing streaming: another source's id to stream instead, or None."""
        return None

    def wake(self):
        """Only for a source that wakes: bring it up off the GUI thread, then call the window's _on_wake_done."""

    def control_client(self):
        raise NotImplementedError

    def stream_params(self) -> tuple:
        """(width, height, fps) the worker asks for; None width and height pass the frame size through."""
        return None, None, 30

    def started(self, session_id: int):
        """The stream began."""

    def stopped(self, remote_stop: bool, active: bool):
        """The stream ended, or a start was given up; active: there was a stream or a wake to stop."""

    def lost(self):
        """The stream dropped. With no other route, the reader waits for the source to come back."""

    def end_recovery(self):
        self.recovering = False

    def check_camera_rate(self, session: "StreamSession", arrival: float):
        """Frames arrive under the rate asked for; with no camera rate to ask about, that's the link."""
        self._win._show_slow(True)

    def on_phone_state(self, state: dict):
        """The phone's state, after the plugins had it."""

    def watch_alive(self):
        """The camera went off: with no video coming in, keep checking the source is still there."""

    def stop_watching_alive(self):
        pass

    def camera_turned_on(self, session_id: int):
        """The camera was asked to come back on mid-stream."""

    def drain_stops(self, deadline: float):
        """Quitting: give stops still on their way until deadline (time.monotonic())."""


class PluginSource(Source):
    """A plugin's StreamSource (see plugin.py): ready once it and the virtual camera are, with nothing to wake."""

    # A browser's first frame can be any shape (even square), and on Linux apps then keep the camera at that size
    auto_canvas = (1920, 1080) if vcam.IS_LINUX else None

    def __init__(self, win: "TelescopeWindow", source):
        super().__init__(win)
        self._source = source
        self.id = source.id

    def open_reader(self):
        return self._source.open_reader()

    @property
    def name(self) -> str:
        return self._source.name

    @property
    def url(self) -> str:
        return self._source.url

    def prepare(self, interactive: bool) -> bool:
        conn = self._win._plugin("connection")
        return conn.ensure_virtual_camera(interactive) and self._source.prepare(interactive)

    def control_client(self):
        return self._source.control_client()

    @property
    def remember_changes_only(self) -> bool:
        return getattr(self._source, "remember_changes_only", False)

    def redirect(self) -> Optional[str]:
        redirect = getattr(self._source, "redirect", None)
        return redirect() if redirect is not None else None


    def stream_params(self) -> tuple:
        return None, None, self._source.fps()


class PhoneSource(Source):
    """A paired phone, by id. Connection resolves and wakes it."""

    wakes = True
    mic_alone = True

    _sig_state = pyqtSignal(int, dict)
    _sig_wake_done = pyqtSignal(int, bool, str, str, object)  # wake_id, ok, reason, url, PhoneAuth
    _sig_wake_progress = pyqtSignal(int, str)  # wake_id, status text
    _sig_recovery_probed = pyqtSignal(int, int, object)  # session id, recovery generation, Resolution
    _sig_camera_rate = pyqtSignal(int, float, float)  # session id, frames arriving per second, the phone's camera rate
    _sig_alive = pyqtSignal(int, bool)  # session id, whether the phone answered (camera off)
    _sig_camera_check = pyqtSignal(int, object)  # session id, the phone's state after turning the camera on (or None)

    def __init__(self, win: "TelescopeWindow", phone_id: Optional[str]):
        super().__init__(win)
        self.id = phone_id
        # Generation counter for phone-wake; guards against stale async results.
        self._wake_id = 0
        self._wake_target = None  # where the latest wake went, to stop it if it finishes after a stop
        self._stop_late_wake = False
        # The generation drops probes from an earlier drop.
        self._recovery_gen = 0
        self._recovery_route = None  # the route recovery last moved the stream to
        self._state_poll_busy = False  # one recovery state poll at a time: an unreachable phone takes 4 s to time out
        self._recovery_timer = QTimer(self)
        self._recovery_timer.setSingleShot(True)
        self._recovery_timer.timeout.connect(self._probe_recovery)
        # The phone's answer holds for a while, so it isn't asked every report.
        self._camera_check_busy = False
        self._camera_fps: Optional[float] = None  # 0: not known
        self._camera_fps_until = 0.0
        self._alive_timer = QTimer(self)
        self._alive_timer.setInterval(_ALIVE_POLL_MS)
        self._alive_timer.timeout.connect(self._check_alive)
        self._alive_misses = 0
        self._alive_busy = False
        # Remote-stop requests; quit path waits for these to complete.
        self._stop_threads: list[threading.Thread] = []

        self._sig_state.connect(lambda session_id, state: win._apply_state(session_id, state, self))
        self._sig_wake_done.connect(self._on_wake_done)
        self._sig_wake_progress.connect(self._on_wake_progress)
        self._sig_recovery_probed.connect(self._on_recovery_probed)
        self._sig_camera_rate.connect(self._on_camera_rate)
        self._sig_alive.connect(self._on_alive)
        self._sig_camera_check.connect(self._on_camera_check)

    @property
    def name(self) -> str:
        conn = self._win._plugin("connection")
        phone = conn.phone(self.id) if conn is not None and hasattr(conn, "phone") else None
        return phone.name if phone is not None else "the phone"

    # ── Start and stop ────────────────────────────────────────────────────

    def prepare(self, interactive: bool) -> bool:
        conn = self._win._plugin("connection")
        self.url, self.auth, ok = conn.get_stream_info(interactive=interactive, pid=self.id)
        return ok

    def control_client(self):
        return PhoneControlClient(self.url, self.auth)

    def stream_params(self) -> tuple:
        so = self._win._plugin("stream_output")
        return so.get_stream_params() if so else (None, None, 30)

    def wake(self):
        conn = self._win._plugin("connection")
        self._wake_id += 1
        self._wake_target = conn.session_target(self.id)
        self._stop_late_wake = True
        output = self._win._plugin("stream_output")
        # Read here, on the GUI thread: what the phone should open at, not the size it last used (which may have failed)
        opening = output.opening() if output is not None and hasattr(output, "opening") else None
        if self.camera_off:
            opening = {**(opening or {}), "camera": "off"}  # an older phone leaves it out and opens the camera
        self._spawn_wake(self._wake_id, conn, self.url, self.auth, self._wake_target, opening)

    def _spawn_wake(self, wake_id: int, conn, url: str, auth, target=None, opening=None):
        """Split from wake() to allow test synchronization; thread mustn't outlive QObject."""
        threading.Thread(
            target=self._wake_phone, args=(wake_id, conn, url, auth, target, opening), daemon=True,
        ).start()

    def _wake_phone(self, wake_id: int, conn, url: str, auth, target=None, opening=None):
        def on_progress(msg: str):
            try:
                self._sig_wake_progress.emit(wake_id, msg)
            except RuntimeError:
                pass

        try:
            ok, reason = conn.ensure_phone_streaming(on_progress=on_progress, target=target, opening=opening)
        except Exception:
            logging.exception("Phone wake failed")
            ok, reason = False, "Couldn't reach the phone."
        try:
            self._sig_wake_done.emit(wake_id, ok, reason, url, auth)
        except RuntimeError:
            pass

    def _on_wake_progress(self, wake_id: int, msg: str):
        if wake_id != self._wake_id or not self._win._waking:
            return
        self._win._source_status(self, msg, "dim")

    def _on_wake_done(self, wake_id: int, ok: bool, reason: str, url: str, auth):
        win = self._win
        if wake_id != self._wake_id or not win._waking:
            if ok and self._stop_late_wake and not win._waking and self.session is None:
                # Stopped while it woke: the stop may have reached the phone first, leaving its camera on
                self._stop_phone_async(self._wake_target)
            return
        self.url, self.auth = url, auth
        win._on_wake_done(self, ok, reason)

    def started(self, session_id: int):
        threading.Thread(target=self._fetch_state_async, args=(session_id,), daemon=True).start()

    def stopped(self, remote_stop: bool, active: bool):
        self._wake_id += 1
        if not remote_stop:
            self._stop_late_wake = False  # the phone is meant to keep going, so a wake landing late isn't stopped either
        if remote_stop and active:
            self._stop_phone_async()
        conn = self._win._plugin("connection")
        if conn is not None and hasattr(conn, "release_stream"):
            conn.release_stream(self.id)
        self._alive_timer.stop()
        self._camera_fps = None

    def _stop_phone_async(self, target=None):
        """Tell phone to shut camera down; tracked thread lets quit path wait for it."""
        conn = self._win._plugin("connection")
        if not conn:
            return
        target = target or conn.session_target(self.id)

        def stop():
            try:
                conn.stop_phone_streaming(target=target)
            except Exception:
                logging.debug("Remote stop failed", exc_info=True)

        t = threading.Thread(target=stop, daemon=True)
        self._stop_threads = [x for x in self._stop_threads if x.is_alive()]
        self._stop_threads.append(t)
        t.start()

    def drain_stops(self, deadline: float):
        """Wait for remote stops to complete, but never block quit indefinitely."""
        for t in self._stop_threads:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                break
            t.join(remaining)
        self._stop_threads.clear()

    # ── The phone's state ─────────────────────────────────────────────────

    def _fetch_state_async(self, session_id: int, report_failure: bool = True):
        """The phone's state to the plugins; report_failure=False keeps a fetch that got nothing quiet (mid-stream)."""
        time.sleep(1.5)
        for _ in range(3):
            session = self.session  # one read: _stop() can clear it between checks on the GUI thread
            if session is None or session.id != session_id:
                return
            state = session.client.get_state()
            if state:
                self._sig_state.emit(session_id, state)
                return
            time.sleep(2)
        if report_failure and self.session is not None and self.session.id == session_id:
            self._sig_state.emit(session_id, {})

    def on_phone_state(self, state: dict):
        """The phone's camera on or off as this stream wants it: a restart keeps it off, and an older phone can't."""
        session = self.session
        if session is None:
            return
        self.camera_toggle = bool(state.get("camera_toggle"))
        if not self.camera_toggle:
            if self.camera_off:
                self._win.set_camera_on(True)
                self._win.show_issue("camera", Issue("This phone can't turn its camera off yet",
                                                     "Update Telescope on the phone to stream the mic with the camera off.",
                                                     kind="warn"))
            return
        if bool(state.get("camera_off")) != self.camera_off:
            session.client.send(action="camera_on", value=0 if self.camera_off else 1)

    # ── A dropped stream ──────────────────────────────────────────────────
    # The worker keeps retrying its URL, which is enough when Wi-Fi blips. But the phone may only be
    # reachable another way now (cable pulled: Wi-Fi; plugged back in: a fresh adb forward), so keep
    # asking Connection how to reach it and point the worker there.

    def lost(self):
        self._recovery_route = None
        self._probe_recovery()

    def end_recovery(self):
        super().end_recovery()
        self._recovery_gen += 1
        self._recovery_timer.stop()

    def _spawn_state_fetch(self, session_id: int):
        """Split out so tests can leave the thread out."""
        if self._state_poll_busy:
            return
        self._state_poll_busy = True
        threading.Thread(target=self._poll_codec_state, args=(session_id,), daemon=True).start()

    def _poll_codec_state(self, session_id: int):
        """The phone's state, if it says why the stream stopped; the rest can wait for the stream to be back."""
        try:
            session = self.session  # one read: _stop() can clear it between checks on the GUI thread
            if session is None or session.id != session_id:
                return
            state = session.client.get_state()
            if state and state.get("codec_error"):
                self._sig_state.emit(session_id, state)
        except RuntimeError:
            pass  # the window is gone
        finally:
            self._state_poll_busy = False

    def _probe_recovery(self):
        session, conn = self.session, self._win._plugin("connection")
        if session is None or conn is None or not self.recovering:
            return
        # The phone may have dropped the stream on purpose (H.264 it can't do at this size): its state says why. The
        # camera can take seconds to give up after the stream stops, so ask every round, not just once.
        self._spawn_state_fetch(session.id)
        gen, job = self._recovery_gen, conn.recovery_probe(self.id)
        self._spawn_recovery_probe(session.id, gen, job)

    def _spawn_recovery_probe(self, session_id: int, gen: int, job):
        """Split out so tests can run it synchronously."""
        def work():
            try:
                res = job()
            except Exception:
                logging.exception("Looking for the phone again failed")
                res = None
            try:
                self._sig_recovery_probed.emit(session_id, gen, res)
            except RuntimeError:
                pass
        threading.Thread(target=work, daemon=True).start()

    def _on_recovery_probed(self, session_id: int, gen: int, res):
        win = self._win
        session = self.session
        if not self.recovering or gen != self._recovery_gen or session is None or session.id != session_id:
            return
        if res is not None and res.status in (NOT_PAIRED, LOCAL_ONLY, PHONE_OUTDATED, DESKTOP_OUTDATED):
            # The phone answers but won't take the stream back: say why instead of retrying forever.
            win._stop(remote_stop=False, source=self)
            conn = win._plugin("connection")
            if conn:
                conn.show_problem(res, self.id)
            return
        if res is not None and res.status == READY:
            if not res.streaming and not res.busy:
                # Stopped on the phone, or by its idle watchdog while we couldn't reach it.
                win._stop(remote_stop=False, source=self)
                win.show_issue("start", Issue(
                    "The phone stopped streaming" if not win._streams else f"{self.name} stopped streaming",
                    "Start again when you're ready.",
                    [BannerAction("Start", win.start_stream)], kind="warn"))
                return
            if res.streaming and res.route != self._recovery_route:
                self._move_stream(session, res.route)
        self._recovery_timer.start(_RECOVER_RETRY_MS)

    def _move_stream(self, session: "StreamSession", route):
        win = self._win
        conn = win._plugin("connection")
        url = conn.adopt_stream_route(route, self.id) if conn else None
        if url is None or self.session is not session:
            return
        self._recovery_route = route
        if url != session.url:
            session.client.close()
            auth = session.worker.auth if session.worker is not None else session.client.auth
            session = replace(session, url=url, client=PhoneControlClient(url, auth))
            win._set_session(self, session)
        if session.worker is not None:
            session.worker.retarget(url)
        else:
            win._on_stream_reconnected(self)  # camera off: the mic follows the new route, and nothing else waits for it
            win._show_camera_off()

    # ── Frames arriving slowly ────────────────────────────────────────────

    def check_camera_rate(self, session: "StreamSession", arrival: float):
        """If the phone's camera makes about as many frames as arrive, it's not the link."""
        if self._camera_fps is not None and time.monotonic() < self._camera_fps_until:
            self._win._show_slow(True, camera_limited=self._camera_limits(arrival))
        elif not self._camera_check_busy:
            # The readout and any note wait for the answer, a moment on a working link.
            self._camera_check_busy = True
            self._spawn_camera_check(session.id, arrival)

    def _spawn_camera_check(self, session_id: int, arrival: float):
        """Split out so tests can run it synchronously."""
        threading.Thread(target=self._check_camera_rate, args=(session_id, arrival), daemon=True).start()

    def _check_camera_rate(self, session_id: int, arrival: float):
        session = self.session
        state = session.client.get_state() if session is not None and session.id == session_id else None
        rate = state.get("camera_fps") if state else None
        try:
            self._sig_camera_rate.emit(session_id, arrival, float(rate) if isinstance(rate, (int, float)) else 0.0)
        except RuntimeError:
            pass  # the window is gone

    def _on_camera_rate(self, session_id: int, arrival: float, camera_fps: float):
        self._camera_check_busy = False
        session = self.session
        if session is None or session.id != session_id or self is not self._win._focus:
            return
        self._camera_fps = camera_fps
        self._camera_fps_until = time.monotonic() + _CAMERA_FPS_KEEP_S
        if self.arrival_slow:
            self._win._show_slow(True, camera_limited=self._camera_limits(arrival))

    def _camera_limits(self, arrival: float) -> bool:
        # 0: a phone too old to say, or no answer: the link, as before
        return bool(self._camera_fps) and arrival >= self._camera_fps * _CAMERA_LIMITED_SHARE

    # ── Camera off: the mic alone ─────────────────────────────────────────

    def watch_alive(self):
        self._alive_misses = 0
        self._alive_timer.start()

    def stop_watching_alive(self):
        self._alive_timer.stop()

    def _check_alive(self):
        """With no video coming in, ask the phone now and then whether it's still there."""
        session = self.session
        if session is None or session.worker is not None or self._alive_busy:
            return
        self._alive_busy = True
        self._spawn_alive_check(session.id, session.client)

    def _spawn_alive_check(self, session_id: int, client):
        """Split out so tests can leave the thread out."""
        def work():
            try:
                ok = client.get_state() is not None
            finally:
                self._alive_busy = False
            try:
                self._sig_alive.emit(session_id, ok)
            except RuntimeError:
                pass  # the window is gone
        threading.Thread(target=work, daemon=True).start()

    def _on_alive(self, session_id: int, ok: bool):
        win = self._win
        session = self.session
        if session is None or session.id != session_id or session.worker is not None:
            return
        if ok:
            self._alive_misses = 0
            if self.recovering:
                win._on_stream_reconnected(self)
                win._show_camera_off()
            return
        self._alive_misses += 1
        if self._alive_misses >= _ALIVE_MISSES and not self.recovering:
            win._start_reconnecting_animation("Lost the phone - reconnecting")
            win._begin_recovery(self)

    def camera_turned_on(self, session_id: int):
        QTimer.singleShot(_CAMERA_ON_CHECK_MS, lambda: self._spawn_camera_on_check(session_id))

    def _spawn_camera_on_check(self, session_id: int):
        session = self.session
        if session is None or session.id != session_id or self.camera_off:
            return
        client = session.client

        def work():
            state = client.get_state()
            try:
                self._sig_camera_check.emit(session_id, state)
            except RuntimeError:
                pass
        threading.Thread(target=work, daemon=True).start()

    def _on_camera_check(self, session_id: int, state):
        """The phone says whether the camera came back on; one that couldn't (in use elsewhere) stays off."""
        win = self._win
        session = self.session
        if session is None or session.id != session_id or self.camera_off or not state:
            return
        if state.get("camera_off") and state.get("camera_error"):
            self.camera_off = True
            win._camera_off_now(session)
            win._bus.camera_on_changed.emit(False)
            win.show_issue("camera", Issue("The camera didn't turn back on", state["camera_error"],
                                           [BannerAction("Try again", lambda: win.set_camera_on(True))], kind="warn"))
