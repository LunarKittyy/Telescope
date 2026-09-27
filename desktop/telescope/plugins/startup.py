"""Starting on its own: stream as soon as the phone is ready, open Telescope at sign-in, and stream only while an
app is using the camera.

All three are checkable entries in the settings menu, and the two streaming ones exclude each other. Streaming by
itself starts once each time the phone becomes ready; after Stop it waits until the phone goes away and comes back,
so Stop sticks. Streaming while watched starts when an app starts reading the camera and stops a while after the
last one lets go, but only a stream it started itself; after Stop it waits until the app lets go and something
opens the camera again.
"""

import logging
from typing import Optional

from PyQt6.QtCore import QTimer
from PyQt6.QtGui import QAction

from telescope.platform import autostart
from telescope.plugin import TelescopePlugin
from telescope.widgets.banner import Issue


logger = logging.getLogger(__name__)

WATCH_STOP_DELAY_MS = 15_000  # how long the camera sits unread before a stream started for an app stops


class StartupPlugin(TelescopePlugin):
    name = "startup"

    def __init__(self):
        self.auto_stream = False
        self.watch_stream = False
        self._held = False            # already started (or stopped) for this arrival of the phone
        self._phone: Optional[str] = None
        self._ready = False
        self._watched = False
        self._watch_held = False      # already started (or stopped) while this app reads the camera
        self._starting_for_watch = False  # asked the host to start; the next stream_started is ours
        self._started_for_watch = False   # the running stream is one it started, so it may stop it
        self._stream_action: Optional[QAction] = None
        self._watch_action: Optional[QAction] = None

    def setup(self, host, bus):
        self._host = host
        self._bus = bus
        self._watch_stop = QTimer()
        self._watch_stop.setSingleShot(True)
        self._watch_stop.setInterval(WATCH_STOP_DELAY_MS)
        self._watch_stop.timeout.connect(self._stop_unwatched)
        bus.phone_ready.connect(self._on_phone_ready)
        bus.stream_started.connect(self._on_stream_started)
        bus.stream_stopped.connect(self._on_stream_stopped)
        bus.device_changed.connect(self._on_device_changed)
        bus.camera_watched.connect(self._on_camera_watched)

    # ── Menu ──────────────────────────────────────────────────────────────

    def create_menu_actions(self) -> list:
        divider = QAction(None)
        divider.setSeparator(True)
        stream = QAction("Start streaming when the phone is ready", None)
        stream.setCheckable(True)
        stream.setChecked(self.auto_stream)
        stream.toggled.connect(self.set_auto_stream)
        sign_in = QAction("Open Telescope when I sign in", None)
        sign_in.setCheckable(True)
        sign_in.setChecked(autostart.is_enabled())
        sign_in.toggled.connect(self.set_open_at_sign_in)
        watch = QAction("Stream only while an app is using the camera", None)
        watch.setCheckable(True)
        watch.setChecked(self.watch_stream)
        watch.toggled.connect(self.set_watch_stream)
        self._stream_action, self._watch_action = stream, watch
        return [divider, stream, sign_in, watch]

    def set_auto_stream(self, on: bool):
        if on and self.watch_stream:
            self.set_watch_stream(False)  # a stream started for the phone would never stop for the app
        self.auto_stream = on
        self._check(self._stream_action, on)
        self._held = False
        self._keep_in_tray()
        self._host.schedule_save()

    def set_watch_stream(self, on: bool):
        if on and self.auto_stream:
            self.set_auto_stream(False)
        self.watch_stream = on
        self._check(self._watch_action, on)
        self._watch_held = False
        if not on:
            self._forget_own_stream()  # a stream it started is the user's now
        self._keep_in_tray()
        self._host.schedule_save()
        self._maybe_start_for_watch()

    @staticmethod
    def _check(action: Optional[QAction], on: bool):
        if action is not None and action.isChecked() != on:
            action.setChecked(on)  # re-enters its setter, which finds nothing left to change

    def _keep_in_tray(self):
        # Closing the window mustn't quit what's waiting for the phone or for an app.
        self._host.set_keep_in_tray(self.auto_stream or self.watch_stream)

    def set_open_at_sign_in(self, on: bool):
        ok, detail = autostart.enable() if on else autostart.disable()
        if ok:
            self._host.clear_issue("startup")
        else:
            self._host.show_issue("startup", Issue(detail))

    # ── Streaming by itself ───────────────────────────────────────────────

    def _on_phone_ready(self, phone_id: str, ready: bool):
        if phone_id != self._phone:
            if self._phone is not None:
                self._held = False  # another phone: a new arrival (the first report just names it)
            self._phone = phone_id
        self._ready = ready
        if not ready:
            self._held = False  # gone: the next time it's ready counts as a new arrival
            return
        if self.auto_stream and not self._held and not self._host.is_streaming():
            self._held = True
            self._host.start_stream(interactive=False)
        self._maybe_start_for_watch()

    def _on_stream_started(self, _url: str):
        self._started_for_watch = self._starting_for_watch
        self._starting_for_watch = False

    def _on_stream_stopped(self):
        self._held = True
        if self._host.is_restarting():
            # The same stream back in a moment (a reconnect or camera resize): still one it may stop.
            self._starting_for_watch = self._starting_for_watch or self._started_for_watch
            self._started_for_watch = False
            return
        self._forget_own_stream()
        if self._watched:
            self._watch_held = True  # stopped while an app reads the camera: wait for it to let go

    def _on_device_changed(self, _name: str):
        self._held = False
        self._ready = False  # the new phone's first status check says whether it's ready

    # ── Streaming while an app uses the camera ────────────────────────────

    def _on_camera_watched(self, watched: bool):
        self._watched = watched
        logger.info("An app %s reading the camera", "started" if watched else "stopped")
        if watched:
            self._watch_stop.stop()
            self._maybe_start_for_watch()
            return
        self._watch_held = False  # the next app to open the camera counts as new
        if self._started_for_watch or self._starting_for_watch:
            self._watch_stop.start()
        elif self.watch_stream and self._host.is_streaming():
            logger.info("The stream wasn't started for an app, so it keeps going")

    def _maybe_start_for_watch(self):
        if not (self.watch_stream and self._watched and self._ready) or self._watch_held:
            return
        if self._host.is_streaming() or self._host.is_starting():
            return  # someone else's start; it isn't this one's to stop
        # Held before starting, so a start that fails isn't retried on every status check.
        self._watch_held = True
        self._starting_for_watch = True
        logger.info("Starting the stream for the app reading the camera")
        self._host.start_stream(interactive=False)

    def _stop_unwatched(self):
        if self._watched:
            return
        ours = self._started_for_watch or self._starting_for_watch
        self._forget_own_stream()
        if ours:
            logger.info("No app has read the camera for a while; stopping the stream started for it")
            self._host.stop_stream()  # also stops a start still waking the phone; a no-op if that start failed

    def _forget_own_stream(self):
        self._starting_for_watch = self._started_for_watch = False
        self._watch_stop.stop()

    # ── Config ────────────────────────────────────────────────────────────

    def diagnostics(self) -> dict:
        on = lambda v: "on" if v else "off"  # noqa: E731
        return {
            "Start streaming when the phone is ready": on(self.auto_stream),
            "Stream only while an app is using the camera": on(self.watch_stream),
            "Stream started for an app": "yes" if self._started_for_watch or self._starting_for_watch else "no",
        }

    def get_config(self) -> dict:
        return {"auto_stream": self.auto_stream, "watch_stream": self.watch_stream}

    def set_config(self, cfg: dict):
        self.auto_stream = cfg.get("auto_stream") is True
        self.watch_stream = cfg.get("watch_stream") is True
        if self.watch_stream:
            self.auto_stream = False  # both on (from before they excluded each other): streaming only when watched wins
        self._keep_in_tray()
