"""What the window streams from: a paired phone, or a plugin's StreamSource (Browser camera).

Each source keeps the state of its own stream, so the window can run several and point its panels at any of them. A phone also
does what only a phone can: wake its camera, find another route when the stream drops, report its state, and turn its
camera off while its mic carries on.
"""

import logging
import threading
import time
from dataclasses import replace
from typing import TYPE_CHECKING, Optional, Protocol

from PyQt6.QtCore import QObject, QTimer, pyqtSignal

from telescope import vcam
from telescope.phone_client import PhoneControlClient
from telescope.phones import DESKTOP_OUTDATED, LOCAL_ONLY, NOT_PAIRED, PHONE_OUTDATED, READY
from telescope.widgets.banner import BannerAction, Issue
from telescope.i18n import N_, sentence, tr

if TYPE_CHECKING:
    from telescope.session import StreamSession

_RECOVER_RETRY_MS = 3000  # a dropped stream: how often to look for a route back to the phone
_ALIVE_POLL_MS = 5000  # camera off: how often to check the phone is still there, with no video to tell
_ALIVE_MISSES = 2  # checks in a row the phone didn't answer before looking for it like a dropped stream
_CAMERA_ON_CHECK_MS = 6000  # after turning the camera on: when to ask the phone whether it came on
_PARK_AFTER_MS = 10_000  # a stream gone this long: its virtual camera shows the wait screen, not a frozen frame
_UNREACHABLE_AFTER_MS = 30_000  # gone this long: a banner says so
# Frames arriving at 90% of what the phone's camera makes means the link keeps up with the camera; how long the camera's
# rate is trusted before asking again.
_CAMERA_LIMITED_SHARE = 0.9
_CAMERA_FPS_KEEP_S = 10.0
_HEAT_CHECK_MS = 15_000  # how often a streaming phone is asked whether its heat dropped the size


def camera_error_text(state: dict) -> str:
    """Why the phone's camera didn't turn back on, in the current language. A phone that sends no code, or one this
    app doesn't know, gets its English text as it is."""
    if state.get("camera_error_code") == "camera_in_use":
        return tr("The camera didn't turn back on. Another app may be using it.")
    return str(state["camera_error"])


class SourceHost(Protocol):
    """What a source needs from the window, so one can be built (and tested) without it."""

    def connection(self):
        """The Connection plugin, or None."""
        ...

    def stream_output(self):
        """The Stream output plugin, or None."""
        ...

    def show_issue(self, key: str, issue: Issue) -> None: ...

    def clear_issue(self, key: str) -> None: ...

    def is_streaming(self) -> bool:
        """Whether any stream runs."""
        ...

    def is_focused(self, source: "Source") -> bool:
        """Whether the panels show source's stream."""
        ...

    def focus_stream(self, source_id: Optional[str]) -> None:
        """Point the panels at source_id's stream."""
        ...

    def is_waking(self) -> bool:
        """Whether a start is waking a phone now."""
        ...

    def waking_source(self) -> Optional["Source"]:
        """The source that start is waking."""
        ...

    def wake_progress(self, source: "Source", msg: str) -> None:
        """A step of source's wake, for its status line."""
        ...

    def wake_done(self, source: "Source", ok: bool, reason: str) -> None:
        """source's wake is through: the stream begins, or why it couldn't."""
        ...

    def apply_state(self, source: "Source", session_id: int, state: dict) -> None:
        """The phone's state for source's session session_id, to the plugins."""
        ...

    def set_session(self, source: "Source", session: "StreamSession") -> None:
        """source streams with session now (the same stream, moved to another URL)."""
        ...

    def stop_source(self, source: "Source") -> None:
        """Stop source's stream, leaving the phone alone (it's gone, or someone else's now)."""
        ...

    def start_source(self, source_id: Optional[str]) -> None:
        """Start source_id again, by itself or next to the others (a banner's Start)."""
        ...

    def set_camera_on(self, on: bool) -> None:
        """Turn the shown stream's camera on or off."""
        ...

    def camera_stayed_off(self, session: "StreamSession") -> None:
        """The camera didn't come back on: session carries on with the mic alone."""
        ...

    def mic_reconnected(self, source: "Source") -> None:
        """source's stream, with the camera off, is back (or moved to another route)."""
        ...

    def stream_moved(self, source: "Source") -> None:
        """source's stream moved to another route without a gap (hand_over): the plugins hear about the new one."""
        ...

    def lost_contact(self, source: "Source") -> None:
        """source's camera is off and its phone stopped answering: look for it like a dropped stream."""
        ...

    def park(self, source: "Source") -> None:
        """source's stream has been gone a while: its virtual camera gets the wait screen."""
        ...

    def show_slow(self, camera_limited: bool = False) -> None:
        """The shown stream's frames arrive slowly; camera_limited: because the camera makes no more."""
        ...


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
    lost_hint = N_("Check that it's still on.")  # what to do when it can't be reached for a while

    def __init__(self, host: SourceHost):
        super().__init__()
        self._host = host
        # A dropped stream that stays gone: the wait screen takes its virtual camera, then a banner says so.
        self._park_timer = QTimer(self)
        self._park_timer.setSingleShot(True)
        self._park_timer.timeout.connect(lambda: host.park(self))
        self._gone_timer = QTimer(self)
        self._gone_timer.setSingleShot(True)
        self._gone_timer.timeout.connect(self._show_unreachable)
        self.unreachable_shown = False
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
        self.keep_slot: Optional[int] = None  # the slot a restart comes back to
        self.forget_stream_status()

    def forget_stream_status(self):
        """What the footer shows for this stream while the panels show another, back to before it started."""
        self.status = (tr("Not streaming"), "dim")
        self.fps_text = self.net_text = "—"
        self.connected = False

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

    def waiting(self) -> bool:
        """prepare() said no, but something is on its way to it (a browser to scan the code): added next to other
        streams, it keeps a tile until then."""
        return False

    def cancel_wait(self):
        """Its waiting tile was closed."""

    def wake(self):
        """Only for a source that wakes: bring it up off the GUI thread, then call the window's _on_wake_done."""

    def control_client(self):
        raise NotImplementedError

    def stream_params(self) -> tuple:
        """(width, height, fps) the worker asks for; None width and height pass the frame size through."""
        return None, None, 30

    def started(self, session_id: int):
        """The stream began."""

    def refresh_state(self, session_id: int):
        """The panels came back to this stream: its state again, as it is now."""

    def stopped(self, remote_stop: bool, active: bool):
        """The stream ended, or a start was given up; active: there was a stream or a wake to stop."""

    def lost(self):
        """The stream dropped. With no other route, the reader waits for the source to come back."""

    def start_lost_clock(self):
        """The stream just dropped: after a while the wait screen takes over, and later a banner says so."""
        self._park_timer.start(_PARK_AFTER_MS)
        self._gone_timer.start(_UNREACHABLE_AFTER_MS)

    def _show_unreachable(self):
        if not self.recovering or self.session is None:
            return
        self.unreachable_shown = True
        self._host.show_issue(f"stopped:{self.id}", Issue(tr("Can't reach {name}", name=self.name),
                                                                   tr(self.lost_hint), kind="warn"))

    def end_recovery(self):
        self.recovering = False
        self._park_timer.stop()
        self._gone_timer.stop()
        if self.unreachable_shown:
            self.unreachable_shown = False
            self._host.clear_issue(f"stopped:{self.id}")

    def check_camera_rate(self, session: "StreamSession", arrival: float):
        """Frames arrive under the rate asked for; with no camera rate to ask about, that's the link."""
        self._host.show_slow()

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
    _AUTO_CANVAS = (1920, 1080) if vcam.IS_LINUX else None

    def __init__(self, host: SourceHost, source):
        super().__init__(host)
        self._source = source
        self.id = source.id

    @property
    def auto_canvas(self):
        return getattr(self._source, "auto_canvas", self._AUTO_CANVAS)

    def open_reader(self):
        return self._source.open_reader()

    @property
    def name(self) -> str:
        return self._source.name

    @property
    def lost_hint(self) -> str:
        return getattr(self._source, "lost_hint", N_("Open the Telescope page in it again."))

    @property
    def url(self) -> str:
        return self._source.url

    def prepare(self, interactive: bool) -> bool:
        conn = self._host.connection()
        return conn.ensure_virtual_camera(interactive) and self._source.prepare(interactive)

    def control_client(self):
        return self._source.control_client()

    @property
    def remember_changes_only(self) -> bool:
        return getattr(self._source, "remember_changes_only", False)

    def redirect(self) -> Optional[str]:
        redirect = getattr(self._source, "redirect", None)
        return redirect() if redirect is not None else None

    def waiting(self) -> bool:
        waiting = getattr(self._source, "waiting", None)
        return bool(waiting()) if waiting is not None else False

    def cancel_wait(self):
        cancel = getattr(self._source, "cancel_wait", None)
        if cancel is not None:
            cancel()


    def stream_params(self) -> tuple:
        return None, None, self._source.fps()


class PhoneSource(Source):
    """A paired phone, by id. Connection resolves and wakes it."""

    wakes = True
    mic_alone = True
    lost_hint = N_("Open Telescope on it.")

    _sig_state = pyqtSignal(int, dict)
    _sig_wake_done = pyqtSignal(int, bool, str, str, object)  # wake_id, ok, reason, url, PhoneAuth
    _sig_wake_progress = pyqtSignal(int, str)  # wake_id, status text
    _sig_recovery_probed = pyqtSignal(int, int, object)  # session id, recovery generation, Resolution
    # session id, frames arriving per second, the phone's camera rate, the rate Dynamic stepped down to (0: none)
    _sig_camera_rate = pyqtSignal(int, float, float, int, int)
    _sig_alive = pyqtSignal(int, bool)  # session id, whether the phone answered (camera off)
    _sig_heat = pyqtSignal(int, object)  # session id, the phone's state for the heat note (or None)
    _sig_camera_check = pyqtSignal(int, object)  # session id, the phone's state after turning the camera on (or None)

    def __init__(self, host: SourceHost, phone_id: Optional[str]):
        super().__init__(host)
        self.id = phone_id
        # Generation counter for phone-wake; guards against stale async results.
        self._wake_id = 0
        self._wake_target = None  # where the latest wake went, to stop it if it finishes after a stop
        self._stop_late_wake = False
        # The generation drops probes from an earlier drop.
        self._recovery_gen = 0
        self._recovery_route = None  # the route recovery last moved the stream to
        self._handover = None  # (session id, route, url) of a hand_over() under way
        self._heat_shown = False
        self._heat_check_busy = False  # one heat check at a time: an unreachable phone takes seconds to time out
        self._state_poll_busy = False  # one recovery state poll at a time: an unreachable phone takes 4 s to time out
        # The phone can get hot mid-stream, and the state is otherwise only asked for at the start
        self._heat_timer = QTimer(self)
        self._heat_timer.setInterval(_HEAT_CHECK_MS)
        self._heat_timer.timeout.connect(self._check_heat)
        self._sig_heat.connect(self._on_heat_state)
        self._recovery_timer = QTimer(self)
        self._recovery_timer.setSingleShot(True)
        self._recovery_timer.timeout.connect(self._probe_recovery)
        # The phone's answer holds for a while, so it isn't asked every report.
        self._camera_check_busy = False
        self._camera_fps: Optional[float] = None  # 0: not known
        self._camera_fps_until = 0.0
        self.dynamic_fps = 0  # the rate the phone's Dynamic stepped down to for a slow link, as last asked; 0 = none
        self.heat_fps = 0  # the rate the phone capped itself at for its heat, as last asked; 0 = none
        self._alive_timer = QTimer(self)
        self._alive_timer.setInterval(_ALIVE_POLL_MS)
        self._alive_timer.timeout.connect(self._check_alive)
        self._camera_on_timer = QTimer(self)
        self._camera_on_timer.setSingleShot(True)
        self._camera_on_timer.setInterval(_CAMERA_ON_CHECK_MS)
        self._camera_on_timer.timeout.connect(self._spawn_camera_on_check)
        self._camera_on_session = 0
        self._alive_misses = 0
        self._alive_busy = False
        # Remote-stop requests; quit path waits for these to complete.
        self._stop_threads: list[threading.Thread] = []

        self._sig_state.connect(lambda session_id, state: host.apply_state(self, session_id, state))
        self._sig_wake_done.connect(self._on_wake_done)
        self._sig_wake_progress.connect(self._on_wake_progress)
        self._sig_recovery_probed.connect(self._on_recovery_probed)
        self._sig_camera_rate.connect(self._on_camera_rate)
        self._sig_alive.connect(self._on_alive)
        self._sig_camera_check.connect(self._on_camera_check)

    @property
    def name(self) -> str:
        conn = self._host.connection()
        phone = conn.phone(self.id) if conn is not None and hasattr(conn, "phone") else None
        return phone.name if phone is not None else tr("the phone")

    # ── Start and stop ────────────────────────────────────────────────────

    def prepare(self, interactive: bool) -> bool:
        conn = self._host.connection()
        self.url, self.auth, ok = conn.get_stream_info(interactive=interactive, pid=self.id)
        return ok

    def control_client(self):
        return PhoneControlClient(self.url, self.auth)

    def stream_params(self) -> tuple:
        so = self._host.stream_output()
        return so.get_stream_params() if so else (None, None, 30)

    def wake(self):
        conn = self._host.connection()
        self._wake_id += 1
        self._wake_target = conn.session_target(self.id)
        self._stop_late_wake = True
        output = self._host.stream_output()
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
            ok, reason = False, tr("Couldn't reach the phone.")
        try:
            self._sig_wake_done.emit(wake_id, ok, reason, url, auth)
        except RuntimeError:
            pass

    def _on_wake_progress(self, wake_id: int, msg: str):
        if wake_id != self._wake_id or not self._host.is_waking():
            return
        self._host.wake_progress(self, msg)

    def _on_wake_done(self, wake_id: int, ok: bool, reason: str, url: str, auth):
        host = self._host
        if wake_id != self._wake_id or not host.is_waking():
            if ok and self._stop_late_wake and host.waking_source() is not self and self.session is None:
                # Stopped while it woke: the stop may have reached the phone first, leaving its camera on
                self._stop_phone_async(self._wake_target)
            return
        self.url, self.auth = url, auth
        host.wake_done(self, ok, reason)

    def started(self, session_id: int):
        threading.Thread(target=self._fetch_state_async, args=(session_id,), daemon=True).start()
        self._heat_timer.start()

    def refresh_state(self, session_id: int):
        # After the panels' settings went out, so it holds them rather than the ones the stream started with
        threading.Thread(target=self._fetch_state_async, args=(session_id, False), daemon=True).start()

    def stopped(self, remote_stop: bool, active: bool):
        self._heat_timer.stop()
        self._wake_id += 1
        if not remote_stop:
            self._stop_late_wake = False  # the phone is meant to keep going, so a wake landing late isn't stopped either
        if remote_stop and active:
            self._stop_phone_async()
        conn = self._host.connection()
        if conn is not None and hasattr(conn, "release_stream"):
            conn.release_stream(self.id)
        self._handover = None  # a stopped worker says nothing more about it
        self._alive_timer.stop()
        self._camera_on_timer.stop()
        self._camera_fps = None
        self.dynamic_fps = 0
        self.heat_fps = 0

    def _stop_phone_async(self, target=None):
        """Tell phone to shut camera down; tracked thread lets quit path wait for it."""
        conn = self._host.connection()
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
        if state.get("camera_taken"):
            # Android gives the camera to the app on screen; the phone opens it again once that app lets go
            self._host.show_issue(f"camera_taken:{self.id}", Issue(
                tr("Another app on the phone is using the camera"),
                tr("The stream comes back by itself once that app closes or lets go of it."), kind="warn"))
        else:
            self._host.clear_issue(f"camera_taken:{self.id}")
        self._show_heat(state)
        self.camera_toggle = bool(state.get("camera_toggle"))
        if not self.camera_toggle:
            if self.camera_off:
                self._host.set_camera_on(True)
                self._host.show_issue("camera", Issue(tr("This phone can't turn its camera off yet"),
                                                      tr("Update Telescope on the phone to stream the mic with the "
                                                         "camera off."), kind="warn"))
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

    def _show_heat(self, state: dict):
        """Says so while the phone streams a smaller picture than asked because it's critically hot."""
        w, h, fps = state.get("heat_width"), state.get("heat_height"), state.get("heat_fps")
        if not (isinstance(w, int) and isinstance(h, int) and w > 0 and h > 0):
            self._clear_heat()
            return
        size = f"{w} × {h}"
        if isinstance(fps, int) and not isinstance(fps, bool) and fps > 0:
            text = tr("It's streaming at {size} and {fps} fps until it cools down.", size=size, fps=fps)
        else:
            text = tr("It's streaming at {size} until it cools down.", size=size)
        self._host.show_issue(f"heat:{self.id}", Issue(tr("The phone is very hot"), text, kind="warn"))
        self._heat_shown = True

    def _check_heat(self):
        session = self.session
        if session is None or self._heat_check_busy:
            return
        self._heat_check_busy = True

        def work():
            state = session.client.get_state()
            try:
                self._sig_heat.emit(session.id, state)
            except RuntimeError:
                pass  # the window is gone
        threading.Thread(target=work, daemon=True).start()

    def _on_heat_state(self, session_id: int, state):
        self._heat_check_busy = False
        session = self.session
        if state and session is not None and session.id == session_id:
            self._show_heat(state)

    def _clear_heat(self):
        if getattr(self, "_heat_shown", False):
            self._heat_shown = False
            self._host.clear_issue(f"heat:{self.id}")

    def forget_stream_status(self):
        super().forget_stream_status()
        self._clear_heat()  # a stopped stream isn't running small any more

    def end_recovery(self):
        super().end_recovery()
        self._recovery_gen += 1
        self._recovery_timer.stop()
        self._host.clear_issue(f"camera_taken:{self.id}")  # frames are back, or the stream is over

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
            if state and (state.get("codec_error") or state.get("camera_taken")):
                self._sig_state.emit(session_id, state)
        except RuntimeError:
            pass  # the window is gone
        finally:
            self._state_poll_busy = False

    def _probe_recovery(self):
        session, conn = self.session, self._host.connection()
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
        host = self._host
        session = self.session
        if not self.recovering or gen != self._recovery_gen or session is None or session.id != session_id:
            return
        if res is not None and res.status in (NOT_PAIRED, LOCAL_ONLY, PHONE_OUTDATED, DESKTOP_OUTDATED):
            # The phone answers but won't take the stream back: say why instead of retrying forever.
            host.stop_source(self)
            conn = host.connection()
            if conn:
                conn.show_problem(res, self.id)
            return
        if res is not None and res.status == READY:
            other = getattr(res, "streaming_for", "")
            if other:
                # Another computer started the phone while this one couldn't reach it: that stream is theirs.
                host.stop_source(self)
                host.show_issue(f"stopped:{self.id}", Issue(
                    tr("The phone is streaming to {computer}", computer=other) if not host.is_streaming()
                    else sentence(tr("{phone} is streaming to {computer}", phone=self.name, computer=other)),
                    tr("Stop it there or on the phone, then start again."),
                    [BannerAction(tr("Start"), lambda: host.start_source(self.id))], kind="warn"))
                return
            if not res.streaming and not res.busy:
                # Stopped on the phone, or by its idle watchdog while we couldn't reach it.
                host.stop_source(self)
                host.show_issue(f"stopped:{self.id}", Issue(
                    tr("The phone stopped streaming") if not host.is_streaming()
                    else sentence(tr("{phone} stopped streaming", phone=self.name)),
                    tr("Start again when you're ready."),
                    [BannerAction(tr("Start"), lambda: host.start_source(self.id))], kind="warn"))
                return
            if res.streaming and res.route != self._recovery_route:
                self._move_stream(session, res.route)
        self._recovery_timer.start(_RECOVER_RETRY_MS)

    def _move_stream(self, session: "StreamSession", route):
        host = self._host
        conn = host.connection()
        url = conn.adopt_stream_route(route, self.id) if conn else None
        if url is None or self.session is not session:
            return
        self._recovery_route = route
        if url != session.url:
            session.client.retarget(url)  # the same client, so its settings and what the panels keep for it carry on
            session = replace(session, url=url)
            host.set_session(self, session)
        if session.worker is not None:
            session.worker.retarget(url)
        else:
            host.mic_reconnected(self)  # camera off: the mic follows the new route, and nothing else waits for it

    # ── Moving to another route without a gap ─────────────────────────────

    def hand_over(self, route) -> bool:
        """Open the stream on route next to the running one and move over once it's healthy (plugging in USB while
        streaming over Wi-Fi). False if it can't try now; the Connection card hears how it went either way."""
        session = self.session
        conn = self._host.connection()
        if session is None or self.recovering or self._handover is not None or conn is None:
            return False
        url = conn.prepare_stream_route(route, self.id)
        if url is None:
            return False
        if session.worker is None:
            # Camera off: no video reader to wait for, the mic opens the already checked route next to the old one
            self._handover = (session.id, route, url)
            self.on_moved(url)
            return True
        if not session.worker.hand_over(url):
            conn.drop_stream_route(self.id)
            return False
        self._handover = (session.id, route, url)
        return True

    def on_moved(self, url: str):
        pending, self._handover = self._handover, None
        session = self.session
        conn = self._host.connection()
        if pending is None or session is None or session.id != pending[0] or pending[2] != url or conn is None:
            return
        conn.commit_stream_route(self.id)
        session.client.retarget(url)  # the same client, so its settings carry on
        self._host.set_session(self, replace(session, url=url))
        if session.worker is None:
            self._host.mic_reconnected(self)
        else:
            self._host.stream_moved(self)

    def on_move_failed(self, url: str):
        pending, self._handover = self._handover, None
        conn = self._host.connection()
        if pending is None or pending[2] != url or conn is None:
            return
        conn.drop_stream_route(self.id, failed=True)

    # ── Frames arriving slowly ────────────────────────────────────────────

    def check_camera_rate(self, session: "StreamSession", arrival: float):
        """If the phone's camera makes about as many frames as arrive, it's not the link."""
        if self._camera_fps is not None and time.monotonic() < self._camera_fps_until:
            self._host.show_slow(camera_limited=self._camera_limits(arrival))
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
        stepped = state.get("dynamic_fps") if state else None
        heat = state.get("heat_fps") if state else None
        try:
            self._sig_camera_rate.emit(session_id, arrival, float(rate) if isinstance(rate, (int, float)) else 0.0,
                                       stepped if isinstance(stepped, int) and stepped > 0 else 0,
                                       heat if isinstance(heat, int) and not isinstance(heat, bool) and heat > 0 else 0)
        except RuntimeError:
            pass  # the window is gone

    def _on_camera_rate(self, session_id: int, arrival: float, camera_fps: float, dynamic_fps: int = 0,
                        heat_fps: int = 0):
        self._camera_check_busy = False
        session = self.session
        if session is None or session.id != session_id or not self._host.is_focused(self):
            return
        self._camera_fps = camera_fps
        self.dynamic_fps = dynamic_fps
        self.heat_fps = heat_fps
        self._camera_fps_until = time.monotonic() + _CAMERA_FPS_KEEP_S
        if self.arrival_slow:
            self._host.show_slow(camera_limited=self._camera_limits(arrival))

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
        session = self.session
        if session is None or session.id != session_id or session.worker is not None:
            return
        if ok:
            self._alive_misses = 0
            if self.recovering:
                self._host.mic_reconnected(self)
            return
        self._alive_misses += 1
        if self._alive_misses >= _ALIVE_MISSES and not self.recovering:
            self._host.lost_contact(self)

    def camera_turned_on(self, session_id: int):
        self._camera_on_session = session_id
        self._camera_on_timer.start()

    def _spawn_camera_on_check(self):
        session_id = self._camera_on_session
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

    def _camera_on_again(self):
        self._host.focus_stream(self.id)  # the switch is the panels' stream's
        if self._host.is_focused(self):
            self._host.set_camera_on(True)

    def _on_camera_check(self, session_id: int, state):
        """The phone says whether the camera came back on; one that couldn't (in use elsewhere) stays off."""
        host = self._host
        session = self.session
        if session is None or session.id != session_id or self.camera_off or not state:
            return
        if state.get("camera_off") and state.get("camera_error"):
            self.camera_off = True
            host.camera_stayed_off(session)
            host.show_issue("camera", Issue(tr("The camera didn't turn back on"), camera_error_text(state),
                                            [BannerAction(tr("Try again"), self._camera_on_again)], kind="warn"))
