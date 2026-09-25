"""Starting on its own: stream as soon as the phone is ready, and open Telescope at sign-in.

Both are checkable entries in the settings menu. Streaming by itself starts once each time the phone
becomes ready; after Stop it waits until the phone goes away and comes back, so Stop sticks.
"""

from typing import Optional

from PyQt6.QtGui import QAction

from telescope.platform import autostart
from telescope.plugin import TelescopePlugin
from telescope.widgets.banner import Issue


class StartupPlugin(TelescopePlugin):
    name = "startup"

    def __init__(self):
        self.auto_stream = False
        self._held = False            # already started (or stopped) for this arrival of the phone
        self._phone: Optional[str] = None

    def setup(self, host, bus):
        self._host = host
        self._bus = bus
        bus.phone_ready.connect(self._on_phone_ready)
        bus.stream_stopped.connect(self._on_stream_stopped)
        bus.device_changed.connect(self._on_device_changed)

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
        return [divider, stream, sign_in]

    def set_auto_stream(self, on: bool):
        self.auto_stream = on
        self._held = False
        self._host.set_keep_in_tray(on)  # closing the window mustn't quit what's waiting for the phone
        self._host.schedule_save()

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
        if not ready:
            self._held = False  # gone: the next time it's ready counts as a new arrival
            return
        if self.auto_stream and not self._held and not self._host.is_streaming():
            self._held = True
            self._host.start_stream(interactive=False)

    def _on_stream_stopped(self):
        self._held = True

    def _on_device_changed(self, _name: str):
        self._held = False

    # ── Config ────────────────────────────────────────────────────────────

    def get_config(self) -> dict:
        return {"auto_stream": self.auto_stream}

    def set_config(self, cfg: dict):
        self.auto_stream = cfg.get("auto_stream") is True
        self._host.set_keep_in_tray(self.auto_stream)
