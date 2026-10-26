"""Starting, stopping and moving the panels between streams.

StreamFlow drives Sessions: it starts a source (waking a phone first), stops one, moves the panels' focus between
streams that keep running and keeps the plugins that stay put (the mic) on one stream. It reaches the window only
through FlowHost (widgets, banners, saved settings, plugins' hooks), so tests/test_stream_flow.py can drive it with a
stand-in and no window.
"""

import logging
from typing import Callable, Optional, Protocol

from telescope import vcam
from telescope.plugin import TelescopePlugin
from telescope.session import Sessions, StreamSession
from telescope.sources import Source
from telescope.stream import guarded_step
from telescope.i18n import tr


class FlowHost(Protocol):
    """What StreamFlow asks of the window."""

    bus: object  # the EventBus

    def plugins(self) -> list: ...
    def plugin(self, name: str): ...
    def call(self, plugins: list, hook: str, *args): ...
    def source_by_id(self, source_id: Optional[str]) -> Source: ...
    def plugin_source(self, source_id: str) -> Optional[Source]: ...
    def selected_source(self) -> Source: ...
    def later(self, fn: Callable[[], None]): ...
    def start_problem(self, title: str, text: str): ...
    def clear_issue(self, key: str): ...
    def set_start_button(self, streaming: bool): ...
    def enable_start(self, enabled: bool): ...
    def source_status(self, source: Source, msg: str, kind: str): ...
    def show_stream(self, source: Source): ...
    def show_stopped(self): ...
    def show_camera_off(self): ...
    def refresh_tiles(self): ...
    def clear_pending_resolution(self, source: Source): ...
    def set_behind(self, behind: bool, source: Source): ...
    def save_profile(self, source_id: Optional[str], plugins: list): ...
    def apply_device_profile(self, source_id: Optional[str], plugins: list): ...
    def schedule_save(self): ...
    def canvas_size(self) -> tuple: ...
    def set_up_slots(self, then: Callable[[], None]): ...
    def set_camera_on(self, on: bool): ...
    def apply_state(self, session_id: int, state: dict, source: Source): ...
    def new_worker(self, source: Source, url: str, auth, params: tuple, pipeline: list, canvas: tuple): ...
    def end_worker(self, worker): ...
    def packet_steps(self) -> dict: ...
    def ask_keyframe(self, source: Source): ...


class StreamFlow:
    """Every stream's start, stop and focus; the state itself is in Sessions."""

    def __init__(self, host: FlowHost):
        self._host = host
        self.sessions = Sessions()
        self._next_session_id = 1
        self.start_id: Optional[str] = None  # what the last start was for, so its banner's Try again starts it again
        self.slot_memory: dict = {}  # source id: the extra virtual camera it went to last, so it goes there again

    # ── Who hears about what ──────────────────────────────────────────────

    def active_source(self) -> Source:
        """The panels' stream's source, the one a start is waking, or while idle the one Start would stream from."""
        return self.sessions.active(self._host.selected_source)

    def tell(self, source: Source, hook: str, *args):
        """Call a stream hook on the plugins that hear about source (Sessions.hearing)."""
        self._host.call(self.sessions.hearing(self._host.plugins(), source), hook, *args)

    def followers(self) -> list:
        return Sessions.followers(self._host.plugins())

    def stays(self) -> list:
        """The plugins that stay with one stream (the mic) whichever the panels show."""
        return Sessions.stays(self._host.plugins())

    def _each_plugin(self, hook: str, *args):
        self._host.call(self._host.plugins(), hook, *args)

    def _each_follower(self, hook: str, *args):
        self._host.call(self.followers(), hook, *args)

    # ── Starting ──────────────────────────────────────────────────────────

    def start(self, interactive: bool = True, source: Optional[Source] = None):
        """Ensure phone camera is running before reading frames; wake happens off-thread to avoid blocking UI."""
        host, sessions = self._host, self.sessions
        conn = host.plugin("connection")
        if not conn:
            return
        host.clear_issue("start")
        source = source or host.selected_source()
        target = source.redirect() if not sessions.streams else None
        redirected = host.plugin_source(target) if target else None
        if redirected is not None:  # e.g. Browser camera picked with a browser already there
            if conn.selected_device != target:
                conn.select(target)
            source = redirected
        self.start_id = source.id
        host.clear_issue(f"stopped:{source.id}")
        sessions.preparing = True
        try:
            ok = source.prepare(interactive)
        except Exception:
            logging.exception("Getting %s ready to stream failed", source.name)
            host.start_problem(tr("Couldn't start streaming"), tr("Something went wrong. Copy diagnostics has the details."))
            ok = False
        finally:
            sessions.preparing = False
            host.later(self.after_start)
        if not ok:
            self.start_failed(source)
            return
        if not source.wakes:  # nothing to wake: the stream starts right away
            self.begin_stream(source)
            return

        sessions.waking = True
        sessions.wake_source = source
        host.set_start_button(streaming=True)
        host.enable_start(False)
        host.source_status(source, tr("Starting the phone's mic…") if source.camera_off
                           else tr("Starting the phone's camera…"), "dim")
        if sessions.streams:
            host.refresh_tiles()  # its tile says it's starting
        source.wake()

    def start_failed(self, source: Source):
        sessions = self.sessions
        source.keep_slot = None
        if sessions.streams and source.session is None and source.waiting():
            sessions.pending = source  # a tile of its own, with the panels on it, until what it waits for comes
            self._host.bus.stream_start_failed.emit()
            self._host.refresh_tiles()
            return
        if sessions.streams and source is sessions.focus and source.session is None:
            self.move_focus(sessions.streams[0])  # an added camera that didn't start: back to one that streams
        if sessions.main is source and source.session is None:
            self.sync_main()  # the mic's stream didn't come back from a restart: it moves to one that streams
        self._host.bus.stream_start_failed.emit()

    def wake_done(self, source: Source, ok: bool, reason: str):
        host, sessions = self._host, self.sessions
        sessions.waking = False
        sessions.wake_source = None
        host.enable_start(True)
        host.later(self.after_start)
        if not ok:
            host.set_start_button(streaming=bool(sessions.streams))
            host.source_status(source, tr("Not streaming"), "dim")
            host.start_problem(tr("Couldn't start the phone's camera"), reason)
            self.start_failed(source)
            return
        self.begin_stream(source)

    def begin_stream(self, source: Source):
        host, sessions = self._host, self.sessions
        url = source.url
        ctrl = source.control_client()
        session_id = self._next_session_id
        self._next_session_id += 1
        source.camera_toggle = None
        first = not sessions.streams
        if first:
            sessions.focus = sessions.main = source
        source.slot = self.free_slot(source)
        source.keep_slot = None
        worker = None if source.camera_off else self.start_worker(url, source.auth, source)
        sessions.add(source, StreamSession(id=session_id, url=url, client=ctrl, worker=worker, source=source))

        if first:
            host.bus.stream_started.emit(url)
            self._each_plugin("on_stream_start", url, ctrl)
        else:
            self.tell(source, "on_stream_start", url, ctrl)  # the panels if they show it, the mic if it's back
        if source.camera_off:
            self.tell(source, "on_camera_off")

        source.started(session_id)

        host.set_start_button(streaming=True)
        if source.camera_off:
            host.show_camera_off()
        else:
            host.source_status(source, tr("Connecting..."), "dim")
        self.streams_changed()

    def free_slot(self, source: Source) -> int:
        """The virtual camera source streams to: the usual one alone, else the one it had before if it's free."""
        used = {s.slot for s in self.sessions.streams if s is not source}
        if source.keep_slot is not None and source.keep_slot not in used:
            return source.keep_slot
        if not used:
            return 0
        remembered = self.slot_memory.get(source.id)
        if isinstance(remembered, int) and 0 < remembered < vcam.MAX_SLOTS and remembered not in used:
            return remembered
        slot = min(set(range(vcam.MAX_SLOTS)) - used, default=0)
        if slot and source.id:
            self.slot_memory[source.id] = slot
            self._host.schedule_save()
        return slot

    def start_worker(self, url: str, auth, source: Source):
        """A video worker on url, after whatever holds the virtual camera while idle has let go of it."""
        params = source.stream_params()
        canvas_w, canvas_h = self._host.canvas_size()
        if canvas_w is None and source.auto_canvas:
            canvas_w, canvas_h = source.auto_canvas
        if source.slot and not (canvas_w and canvas_h):
            canvas_w, canvas_h = vcam.DEFAULT_SIZE  # an extra camera keeps one size, whatever streams to it
        self.free_vcam_for(source.slot)
        live = source is self.sessions.focus or not self.sessions.streams
        worker = self._host.new_worker(source, url, auth, params, self.pipeline(live=live), (canvas_w, canvas_h))
        if live:
            worker.packets.set(self._host.packet_steps())
        return worker

    def free_vcam_for(self, slot: int):
        """Whatever holds slot's virtual camera while idle (the wait screen) lets go, for a stream about to open it."""
        if slot == 0:
            self._each_plugin("on_stream_starting")
        self.announce_idle_outputs(opening=slot)

    def pipeline(self, live: bool = True) -> list:
        """The plugins' frame steps: live for the stream the panels show, or frozen at their current settings for one
        that carries on while they show another."""
        steps = []
        for p in sorted(self._host.plugins(), key=lambda p: p.sees_final_frame):  # stable: otherwise as registered
            if type(p).process_frame is TelescopePlugin.process_frame:
                continue
            try:
                fn = p.process_frame if live else p.frame_step()
            except Exception:
                logging.exception("Plugin %s couldn't freeze its frame step; leaving it out", p.name)
                continue
            if fn is not None:
                steps.append(guarded_step(p.name or type(p).__name__, fn))
        return steps

    def after_start(self):
        """What waited for the start under way to be through: phone states that need the panels, restarts, then
        starts that waited for the extra cameras."""
        sessions = self.sessions
        while not sessions.is_starting() and (sessions.held_states or sessions.restart_later or sessions.start_later):
            if sessions.held_states:
                source, session_id, state = sessions.held_states.pop(next(iter(sessions.held_states)))
                self._host.apply_state(session_id, state, source)
            elif sessions.restart_later:
                self.reconnect_stream(sessions.restart_later.pop())
            else:
                sessions.start_later.pop(0)()

    def reconnect_stream(self, source_id: Optional[str] = None):
        """Restart the panels' stream, or source_id's, to pick up changed connection settings."""
        sessions = self.sessions
        waking = sessions.is_waking(source_id)
        if source_id is None or waking:
            if sessions.shown is None and not sessions.waking:  # a start still waking would use the old settings
                return
            source = self.active_source()
        else:
            source = sessions.find(source_id)
            if source is None:
                return
            if sessions.is_starting():  # one start at a time: this one goes after it
                sessions.restart_later.add(source_id)
                return
            self.move_focus(source)  # it starts with the panels' settings, so they have to be its own
        self.stop_for_restart(source)
        self.start(source=source)

    def stop_for_restart(self, source: Optional[Source] = None):
        """Stop a stream that starts again right after; stream_stopped handlers can tell with is_restarting()."""
        source = source or self.active_source()
        if source.session is not None:
            source.keep_slot = source.slot  # back on the virtual camera it had, which the call app watches
        self.sessions.restarting = True
        try:
            self.stop(remote_stop=False, source=source, keep_focus=True)
        finally:
            self.sessions.restarting = False

    # ── Several streams: which one the panels show ────────────────────────

    def focus_stream(self, source_id: str):
        """Point the panels at source_id's stream (a tile was clicked)."""
        sessions = self.sessions
        source = sessions.find(source_id)
        if source is None and sessions.pending is not None and sessions.pending.id == source_id:
            source = sessions.pending
        if source is not None and not sessions.waking:
            self.move_focus(source)

    def pick_source(self, source_id: Optional[str]) -> bool:
        """The picker moved to source_id. True if that's handled here: its stream gets the panels, or with several
        running it takes the place of the one they show."""
        sessions = self.sessions
        source = sessions.find(source_id)
        if source is not None:
            if not sessions.waking:
                self.move_focus(source)
            return True
        if len(sessions.streams) > 1 and not sessions.waking and not sessions.preparing:
            old = sessions.focus
            self.stop(source=old, keep_focus=True)
            new = self._host.source_by_id(source_id)
            self.move_focus(new)
            self.start(source=new)
            return True
        return False

    def add_stream(self, source_id: str, before: Optional[Callable[[], None]] = None):
        """Start streaming source_id next to what streams already, to a virtual camera of its own (before: runs once
        the panels show it, so a setting it changes is that source's)."""
        sessions = self.sessions
        if (sessions.is_starting() or not sessions.streams or len(sessions.streams) >= vcam.MAX_SLOTS
                or sessions.is_streaming_from(source_id)):
            return
        source = self._host.source_by_id(source_id)
        if not vcam.slot_ready(self.free_slot(source)):
            self._host.set_up_slots(lambda: self.add_stream(source_id, before))
            return
        if sessions.focus is not None and sessions.focus.camera_off:
            self._host.set_camera_on(True)  # one camera off is for the mic alone; with two it would just be confusing
        self.move_focus(source)  # its own settings first, so it starts with them
        if before is not None:
            before()
        self.start(source=source)

    def move_focus(self, source: Source):
        host, sessions = self._host, self.sessions
        old = sessions.focus
        if source is old:
            return
        if old is not None:
            if old.session is not None:
                host.save_profile(old.id, self.followers())  # as it was, before the stop hooks
                if old.session.worker is not None:
                    old.session.worker.set_pipeline(self.pipeline(live=False))
                    if old.session.worker.packets.set({}):  # packet steps stay with the panels
                        host.ask_keyframe(old)
                self._each_follower("on_focus_lost")  # it streams on, behind
            if old.behind:
                old.behind = False
                host.bus.stream_behind.emit(old.id or "", False)
        sessions.focus = source
        conn = host.plugin("connection")
        if conn is not None:
            conn.show_selected(source.id)
        host.apply_device_profile(source.id, self.followers())
        host.bus.device_changed.emit(source.id or "")
        self.show_focus()

    def show_focus(self):
        """Bring the panels and footer up to date with the stream they show now."""
        host = self._host
        source = self.sessions.focus
        session = source.session
        host.show_stream(source)
        if session is not None:
            if session.worker is not None:
                session.worker.set_pipeline(self.pipeline())
                session.worker.packets.set(host.packet_steps())
            self._each_follower("on_focus_gained", session.url, session.client)
            source.refresh_state(session.id)
            if source.recovering:
                host.bus.stream_lost.emit(source.id or "")
            elif source.connected:
                host.bus.stream_connected.emit(source.id or "")
        host.refresh_tiles()

    # ── The main stream: where the plugins that stay put (the mic) are ────

    def sync_main(self):
        """The first stream left is the one plugins that don't follow the panels (the mic) hear about."""
        main = self.sessions.streams[0] if self.sessions.streams else None
        if main is self.sessions.main or self.sessions.stopping_all:
            return
        self.set_main(main)

    def make_main(self, source_id: str) -> bool:
        """Move the plugins that don't follow the panels (the mic) to source_id's stream. False if it doesn't stream."""
        sessions = self.sessions
        main = next((s for s in sessions.streams if s.id == source_id and s.session is not None), None)
        if main is None or sessions.stopping_all or sessions.restarting:
            return False
        if main is not sessions.main:
            self.set_main(main)
        return True

    def main_stream(self) -> Optional[tuple]:
        """(id, name) of the stream the mic is on, while one streams."""
        main = self.sessions.main
        return (main.id, main.name) if main is not None and main.session is not None else None

    def set_main(self, main: Optional[Source]):
        host = self._host
        stays = self.stays()
        old, self.sessions.main = self.sessions.main, None
        if old is not None:
            host.call(stays, "on_focus_lost" if old.session is not None else "on_stream_stop")
            host.save_profile(old.id, stays)
        self.sessions.main = main
        if main is not None:
            host.apply_device_profile(main.id, stays)
            # Only those staying with it: the panels' plugins already hear about it if they show it
            host.call(stays, "on_focus_gained", main.session.url, main.session.client)

    def streams_changed(self):
        self.announce_idle_outputs()
        self._host.bus.streams_changed.emit(len(self.sessions.streams))
        self._host.refresh_tiles()

    def announce_idle_outputs(self, opening: Optional[int] = None):
        """Tell the wait screen which cameras nothing streams to; opening is one a stream is about to take. The main
        one counts only while others stream: alone, its wait screen follows the stream."""
        # Camera off, or a stream gone a while (parked): free
        used = {s.slot for s in self.sessions.streams
                if s.session.worker is not None and not getattr(s.session.worker, "parked", False)} | {opening}
        first = 0 if self.sessions.streams else 1
        self._host.bus.idle_outputs.emit([n for n in range(first, vcam.MAX_SLOTS)
                                          if n not in used and (n == 0 or vcam.slot_ready(n))])

    # ── Stopping ──────────────────────────────────────────────────────────

    def stop_stream(self, source_id: Optional[str] = None):
        """Stop the panels' stream, or source_id's; safe no-op if idle. Waking counts as active, and so does a
        source waiting next to the streams."""
        sessions = self.sessions
        if sessions.pending is not None and source_id == sessions.pending.id:
            self.drop_pending()
            return
        if source_id is not None:
            source = sessions.find(source_id)
            if source is None and sessions.is_waking(source_id):
                source = sessions.wake_source  # its Starting tile's stop
            if source is not None:
                self.stop(source=source)
        elif sessions.streams or sessions.waking:
            self.stop()

    def drop_pending(self):
        sessions = self.sessions
        source, sessions.pending = sessions.pending, None
        if source is sessions.focus and sessions.streams:
            self.move_focus(sessions.streams[0])
        self._host.refresh_tiles()
        source.cancel_wait()

    def stop_all(self, remote_stop: bool = True):
        """Stop every stream, and a start still waking a phone (the Stop button)."""
        sessions = self.sessions
        if sessions.pending is not None:
            self.drop_pending()
        sessions.drop_queued()
        shown, main = self.active_source(), sessions.main
        others = [s for s in sessions.streams if s is not shown]
        if sessions.waking and sessions.wake_source not in (None, shown):
            others.append(sessions.wake_source)  # one restarting next to the shown stream
        sessions.stopping_all = True
        try:
            for source in others:
                self.stop(remote_stop, source)
        finally:
            sessions.stopping_all = False
        self.stop(remote_stop)
        if main is not None and main is not shown:
            # The mic has the first stream's settings; idle, they'd be saved as the shown one's
            stays = self.stays()
            self._host.save_profile(main.id, stays)
            self._host.apply_device_profile(shown.id, stays)

    def stop(self, remote_stop: bool = True, source: Optional[Source] = None, keep_focus: bool = False):
        """Tear down a stream, the panels' one unless source says; remote_stop=False for reconnects (changed
        address/vcam reload). keep_focus: it starts again right after, so the panels stay on it."""
        host, sessions = self._host, self.sessions
        source = source or self.active_source()
        source.end_recovery()
        was_waking = sessions.waking and sessions.wake_source in (None, source)
        if was_waking:
            sessions.waking = False
            sessions.wake_source = None
            host.enable_start(True)
            host.later(self.after_start)
        shown = source is sessions.focus or sessions.focus is None

        # Forget the session before the synchronous teardown below, so any in-flight async state fetch sees "no active session" immediately rather than racing the unwind.
        session = source.session
        sessions.forget(source)
        worker = session.worker if session else None
        ctrl = session.client if session else None

        source.stopped(remote_stop, active=bool(session or was_waking))

        if worker:
            host.end_worker(worker)
        if ctrl:
            ctrl.close()
        camera_was_off = source.camera_off
        if not sessions.restarting:
            sessions.canvas_restart = False  # stopped for real: a canvas resize under way doesn't start it again
            source.camera_off = source.camera_off_auto = False  # each stream starts with the camera on
            source.keep_slot = None
            sessions.restart_later.discard(source.id)
            sessions.held_states.pop(source.id, None)
        host.clear_pending_resolution(source)
        source.settling_reports = 0
        source.arrival_slow = False
        source.forget_stream_status()
        host.set_behind(False, source)

        if sessions.streams:  # others carry on
            if shown:
                host.save_profile(source.id, self.followers())
                self._each_follower("on_stream_stop")
                host.show_stopped()
                if not keep_focus:
                    self.move_focus(sessions.streams[0])
            if source is sessions.main:
                if sessions.restarting:  # back in a moment: the mic waits for it rather than moving to another
                    host.call(self.stays(), "on_stream_stop")
                else:
                    self.sync_main()
            self.streams_changed()
            return

        if shown:
            host.clear_issue("camera")
        sessions.focus = sessions.main = None
        # The last stream next to it stopped: it's picked
        if sessions.pending is not None and not sessions.restarting:
            pending, sessions.pending = sessions.pending, None
            conn = host.plugin("connection")
            if conn is not None:
                conn.select(pending.id)
        host.set_start_button(streaming=sessions.waking)
        host.show_stopped()
        host.bus.stream_stopped.emit()
        self._each_plugin("on_stream_stop")
        if camera_was_off and not source.camera_off:
            host.bus.camera_on_changed.emit(True)
        self.streams_changed()

    # ── A stream that dropped ─────────────────────────────────────────────

    def begin_recovery(self, source: Optional[Source] = None):
        """The stream dropped: the source looks for its way back (a phone may be reachable another way now)."""
        shown = self.sessions.shown
        source = source or (shown.source if shown is not None else None)
        if source is None or source.session is None or source.recovering:
            return
        source.recovering = True
        source.start_lost_clock()
        self._host.refresh_tiles()
        self._host.set_behind(False, source)
        self._host.bus.stream_lost.emit(source.id or "")
        source.lost()

    def stream_reconnected(self, source: Source):
        source.end_recovery()
        self._host.refresh_tiles()
        source.settling_reports = 1  # the next throughput report still counts the gap
        session = source.session
        if session is None:
            return
        if session.worker is not None and getattr(session.worker, "parked", False):
            self.free_vcam_for(source.slot)  # the wait screen lets go before the stream takes the camera back
            session.worker.set_parked(False)
        if source is not self.sessions.focus and hasattr(session.client, "resend_settings"):
            session.client.resend_settings()  # the panels' plugins send them for the stream they show
        self.tell(source, "on_stream_start", session.url, session.client)

    def park(self, source: Source):
        """A stream gone a while: its virtual camera gets the wait screen instead of the last frame, until it's back."""
        worker = source.session.worker if source.session is not None else None
        if not source.recovering or worker is None or getattr(worker, "parked", True):
            return
        logging.info("%s has been gone a while; the virtual camera shows the wait screen", source.name)
        worker.set_parked(True)
        self.announce_idle_outputs()
