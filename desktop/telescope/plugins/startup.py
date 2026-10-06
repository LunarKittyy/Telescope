"""Starting and stopping on its own, and opening Telescope at sign-in.

The settings menu's Automatic streaming submenu picks when a stream starts by itself (never, as soon as the phone is
ready, or when an app opens the camera) and when one stops by itself (never, or once no app has read the camera for
a while: only a stream it started, or any stream). Starting when the phone is ready happens once each time the phone
becomes ready; after a Stop it waits until the phone goes away and comes back, so Stop sticks. Starting for an app
happens once while that app reads the camera; after a Stop it waits until the app lets go and something opens the
camera again. The idle stop never acts before the camera watch has reported at least once, so a machine where it
can't tell whether an app reads the camera never has its streams stopped under it. With the phone mic on, the idle stop
turns only the camera off and the mic keeps streaming (a call that switched its camera off still hears you); the camera
comes back on when an app opens it again, and Stop ends the stream.
"""

import logging
from typing import Optional

from PyQt6.QtCore import QTimer
from PyQt6.QtGui import QAction, QActionGroup
from PyQt6.QtWidgets import QDialog, QHBoxLayout, QLabel, QMenu, QPushButton, QSizePolicy, QWidgetAction

from telescope import dev_profile
from telescope.platform import autostart
from telescope.plugin import TelescopePlugin
from telescope.widgets.banner import Issue
from telescope.widgets.common import (
    NoScrollComboBox, NoScrollSpinBox, card_layout, control_row, create_card, dialog_buttons, dialog_header,
    dialog_layout, set_ui_role, ui_px,
)


logger = logging.getLogger(__name__)

START_OFF, START_READY, START_WATCHED = "off", "ready", "watched"
STOP_OFF, STOP_OWN, STOP_ANY = "off", "own", "any"
START_CHOICES = (
    (START_OFF, "Only when I press Start"),
    (START_READY, "When the phone is ready"),
    (START_WATCHED, "When an app opens the camera"),
)
STOP_CHOICES = (
    (STOP_OFF, "Only when I press Stop"),
    (STOP_OWN, "When no app uses the camera, if it started the stream"),
    (STOP_ANY, "When no app uses the camera, even if I started it"),
)

# The shortest wait is longer than the slowest camera watch takes to notice an app (the /proc scan runs every 5 s,
# and Windows waits 3 s before it counts a reader as gone), so a stream isn't stopped under an app just opening it.
MIN_STOP_DELAY_S = 10
MAX_STOP_DELAY_S = 3600
DEFAULT_STOP_DELAY_S = 15

_DELAY_DIALOG_WIDTH = 440  # room for the number and the unit side by side in the control column


def delay_text(seconds: int) -> str:
    """"90 s", "2 min": whole minutes read as minutes."""
    return f"{seconds // 60} min" if seconds % 60 == 0 else f"{seconds} s"


def clamp_delay(value) -> int:
    if not isinstance(value, int) or isinstance(value, bool):
        return DEFAULT_STOP_DELAY_S
    return max(MIN_STOP_DELAY_S, min(MAX_STOP_DELAY_S, value))


class StartupPlugin(TelescopePlugin):
    name = "startup"

    def __init__(self):
        self.start_on = START_OFF
        self.stop_idle = STOP_OFF
        self.stop_delay_s = DEFAULT_STOP_DELAY_S
        self.notify_stop = True
        self._held = False            # already started (or stopped) for this arrival of the phone
        self._phone: Optional[str] = None
        self._ready = False
        self._watched = False
        self._watch_known = False     # the camera watch has reported at least once
        self._watch_held = False      # already started (or stopped) while this app reads the camera
        self._starting_own = False    # asked the host to start; the next stream_started is ours
        self._started_own = False     # the running stream is one it started
        self._delay_dlg: Optional["StopDelayDialog"] = None

    def setup(self, host, bus):
        self._host = host
        self._bus = bus
        self._idle_stop = QTimer()
        self._idle_stop.setSingleShot(True)
        self._idle_stop.timeout.connect(self._stop_idle)
        bus.phone_ready.connect(self._on_phone_ready)
        bus.stream_started.connect(self._on_stream_started)
        bus.stream_stopped.connect(self._on_stream_stopped)
        bus.stream_start_failed.connect(self._on_stream_start_failed)
        bus.device_changed.connect(self._on_device_changed)
        bus.streams_changed.connect(lambda _count: self._sync_idle_timer())
        bus.camera_watched.connect(self._on_camera_watched)

    # ── Menu ──────────────────────────────────────────────────────────────

    def create_menu_actions(self) -> list:
        divider = QAction(None)
        divider.setSeparator(True)
        sign_in = QAction("Open Telescope when I sign in", None)
        sign_in.setCheckable(True)
        sign_in.setChecked(autostart.is_enabled())
        sign_in.toggled.connect(self.set_open_at_sign_in)
        if dev_profile.active():  # the sign-in entry is the real app's
            sign_in.setEnabled(False)
            sign_in.setToolTip("Not in a dev profile")
        return [divider, self._build_submenu(), sign_in]

    def _build_submenu(self) -> QMenu:
        menu = QMenu("Automatic streaming")
        self._add_heading(menu, "Start")
        self._add_choices(menu, START_CHOICES, self.start_on, self.set_start_on)
        self._add_heading(menu, "Stop")
        self._add_choices(menu, STOP_CHOICES, self.stop_idle, self.set_stop_idle)
        stops = self.stop_idle != STOP_OFF  # the wait and the notice do nothing without an idle stop
        note = self._add_action(menu, "With the phone mic on, only the camera turns off")
        note.setEnabled(False)
        note.setVisible(stops)
        menu.addSeparator()
        delay = self._add_action(menu, f"Wait before stopping: {delay_text(self.stop_delay_s)}…")
        delay.setEnabled(stops)
        delay.triggered.connect(self._open_delay_dialog)
        notify = self._add_action(menu, "Tell me when it stops a stream")
        notify.setCheckable(True)
        notify.setChecked(self.notify_stop)
        notify.setEnabled(stops)
        notify.toggled.connect(self.set_notify_stop)
        return menu

    @staticmethod
    def _add_action(menu: QMenu, text: str) -> QAction:
        # Made here rather than by menu.addAction(text), so PyQt hears when the menu deletes it.
        action = QAction(text, menu)
        menu.addAction(action)
        return action

    @staticmethod
    def _add_heading(menu: QMenu, text: str):
        # A label, not addSection(): the app's menu style draws sections as a bare gap.
        label = QLabel(text.upper())
        label.setObjectName("menu_section")
        heading = QWidgetAction(menu)
        heading.setDefaultWidget(label)
        heading.setEnabled(False)
        menu.addAction(heading)

    @staticmethod
    def _add_choices(menu: QMenu, choices, current: str, setter):
        group = QActionGroup(menu)
        group.setExclusive(True)
        for mode, text in choices:
            action = StartupPlugin._add_action(menu, text)
            action.setCheckable(True)
            action.setChecked(mode == current)
            group.addAction(action)
            action.triggered.connect(lambda _checked=False, m=mode: setter(m))

    def _open_delay_dialog(self):
        if self._delay_dlg is None:
            self._delay_dlg = StopDelayDialog(self)
        self._delay_dlg.refresh()
        self._delay_dlg.show()
        self._delay_dlg.raise_()
        self._delay_dlg.activateWindow()

    # ── Settings ──────────────────────────────────────────────────────────

    def set_start_on(self, mode: str):
        if mode not in dict(START_CHOICES) or mode == self.start_on:
            return
        self.start_on = mode
        self._held = False
        self._watch_held = False
        self._forget_own_stream()  # a stream it started is the user's now
        self._keep_in_tray()
        self._host.schedule_save()
        self._maybe_start_for_watch()
        self._sync_idle_timer()

    def set_stop_idle(self, mode: str):
        if mode not in dict(STOP_CHOICES) or mode == self.stop_idle:
            return
        self.stop_idle = mode
        self._host.schedule_save()
        self._sync_idle_timer()

    def set_stop_delay(self, seconds: int):
        seconds = clamp_delay(seconds)
        if seconds == self.stop_delay_s:
            return
        self.stop_delay_s = seconds
        self._host.schedule_save()
        if self._idle_stop.isActive():
            self._idle_stop.start(seconds * 1000)  # counts the new wait from now

    def set_notify_stop(self, on: bool):
        if on == self.notify_stop:
            return
        self.notify_stop = on
        self._host.schedule_save()

    def _keep_in_tray(self):
        # Closing the window mustn't quit what's waiting for the phone or for an app.
        self._host.set_keep_in_tray(self.start_on != START_OFF)

    def set_open_at_sign_in(self, on: bool):
        ok, detail = autostart.enable() if on else autostart.disable()
        if ok:
            self._host.clear_issue("startup")
        else:
            self._host.show_issue("startup", Issue(detail))

    # ── Starting when the phone is ready ──────────────────────────────────

    def _on_phone_ready(self, phone_id: str, ready: bool):
        if phone_id != self._phone:
            if self._phone is not None:
                self._held = False  # another phone: a new arrival (the first report just names it)
            self._phone = phone_id
        self._ready = ready
        if not ready:
            self._held = False  # gone: the next time it's ready counts as a new arrival
            return
        if (self.start_on == START_READY and not self._held
                and not self._host.is_streaming() and not self._host.is_starting()):
            # Held before starting, so a start that fails isn't retried on every status check.
            self._held = True
            self._start_own("Starting the stream now the phone is ready")
        self._maybe_start_for_watch()

    def _on_device_changed(self, _name: str):
        self._held = False
        self._ready = False  # the new phone's first status check says whether it's ready

    # ── Starting when an app opens the camera ─────────────────────────────

    def _on_camera_watched(self, watched: bool):
        self._watch_known = True
        self._watched = watched
        logger.info("An app %s reading the camera", "started" if watched else "stopped")
        if watched:
            if self._host.is_streaming() and self._host.is_camera_off_auto():
                logger.info("An app opened the camera again; turning the phone's camera back on")
                self._host.set_camera_on(True, auto=True)
            self._maybe_start_for_watch()
        else:
            self._watch_held = False  # the next app to open the camera counts as new
            if self.stop_idle == STOP_OWN and self._host.is_streaming() and not self._owns_stream():
                logger.info("The stream wasn't started automatically, so it keeps going")
        self._sync_idle_timer()

    def _maybe_start_for_watch(self):
        if not (self.start_on == START_WATCHED and self._watched and self._ready) or self._watch_held:
            return
        if self._host.is_streaming() or self._host.is_starting():
            return  # someone else's start; it isn't this one's to claim
        self._watch_held = True
        self._start_own("Starting the stream for the app reading the camera")

    def _start_own(self, why: str):
        # Marked before asking: a start that fails straight away reports stream_start_failed from inside the call.
        self._starting_own = True
        logger.info(why)
        self._host.start_stream(interactive=False)

    # ── The stream coming and going ───────────────────────────────────────

    def _on_stream_started(self, _url: str):
        self._started_own = self._starting_own
        self._starting_own = False
        if self._idle_stop_applies() and not self._watch_known:
            logger.info("Can't tell yet whether an app reads the camera, so the stream won't stop by itself")
        self._sync_idle_timer()

    def _on_stream_start_failed(self):
        self._starting_own = False
        self._sync_idle_timer()

    def _on_stream_stopped(self):
        self._held = True
        if self._host.is_restarting():
            # The same stream back in a moment (a reconnect or camera resize): still one it started.
            self._starting_own = self._starting_own or self._started_own
            self._started_own = False
            return
        self._forget_own_stream()
        if self._watched:
            self._watch_held = True  # stopped while an app reads the camera: wait for it to let go
        self._sync_idle_timer()

    # ── Stopping once nothing reads the camera ────────────────────────────

    def _owns_stream(self) -> bool:
        return self._started_own or self._starting_own

    def _idle_stop_applies(self) -> bool:
        return self.stop_idle == STOP_ANY or (self.stop_idle == STOP_OWN and self._owns_stream())

    def _counting(self) -> bool:
        """Whether the wait before an idle stop should be running right now. Every doubt means no."""
        return (self._idle_stop_applies() and self._watch_known and not self._watched
                and (self._host.is_streaming() or self._starting_own) and self._host.is_camera_on()
                and self._host.stream_count() <= 1)  # several cameras: each is somebody's, so none stops by itself

    def _sync_idle_timer(self):
        if not self._counting():
            self._idle_stop.stop()
        elif not self._idle_stop.isActive():
            self._idle_stop.start(self.stop_delay_s * 1000)

    def _stop_idle(self):
        if not self._counting():  # something changed since the wait started
            return
        running = self._host.is_streaming() or self._host.is_starting()
        if self._host.is_streaming() and self._host.can_turn_camera_off()[0]:
            # The mic is on: a call that switched its camera off still hears it, so only the camera goes
            logger.info("No app has read the camera for %s; turning the phone's camera off", delay_text(self.stop_delay_s))
            self._idle_stop.stop()
            self._host.set_camera_on(False, auto=True)
            return
        self._forget_own_stream()
        if not running:
            return  # its start already failed; nothing to stop
        logger.info("No app has read the camera for %s; stopping the stream", delay_text(self.stop_delay_s))
        self._host.stop_stream()  # also stops a start still waking the phone
        if self.notify_stop:
            self._host.send_notification(
                "Telescope stopped streaming",
                f"No app used the camera for {delay_text(self.stop_delay_s)}.",
                urgent=False,
            )

    def _forget_own_stream(self):
        self._starting_own = self._started_own = False
        self._idle_stop.stop()

    # ── Config ────────────────────────────────────────────────────────────

    def diagnostics(self) -> dict:
        return {
            "Start streaming": dict(START_CHOICES)[self.start_on],
            "Stop streaming": dict(STOP_CHOICES)[self.stop_idle],
            "Wait before stopping": delay_text(self.stop_delay_s),
            "Tell me when it stops a stream": "on" if self.notify_stop else "off",
            "Stream started automatically": "yes" if self._owns_stream() else "no",
        }

    def get_config(self) -> dict:
        return {"start_on": self.start_on, "stop_idle": self.stop_idle,
                "stop_delay_s": self.stop_delay_s, "notify_stop": self.notify_stop}

    def set_config(self, cfg: dict):
        if "start_on" in cfg or "stop_idle" in cfg:
            start, stop = cfg.get("start_on"), cfg.get("stop_idle")
            self.start_on = start if start in dict(START_CHOICES) else START_OFF
            self.stop_idle = stop if stop in dict(STOP_CHOICES) else STOP_OFF
        elif cfg.get("watch_stream") is True:  # the two checkboxes from before; watching won when both were on
            self.start_on, self.stop_idle = START_WATCHED, STOP_OWN
        elif cfg.get("auto_stream") is True:
            self.start_on, self.stop_idle = START_READY, STOP_OFF
        else:
            self.start_on, self.stop_idle = START_OFF, STOP_OFF
        self.stop_delay_s = clamp_delay(cfg.get("stop_delay_s", DEFAULT_STOP_DELAY_S))
        self.notify_stop = cfg.get("notify_stop") is not False
        self._keep_in_tray()
        self._sync_idle_timer()


class StopDelayDialog(QDialog):
    """How long the camera can go unread before an idle stop: a number and a unit."""

    _UNITS = (("seconds", 1), ("minutes", 60))

    def __init__(self, plugin: StartupPlugin):
        super().__init__()
        self._plugin = plugin
        self.setWindowTitle("Wait before stopping")
        self.setMinimumWidth(ui_px(_DELAY_DIALOG_WIDTH))
        lay = dialog_layout(self)
        dialog_header(lay, "Wait before stopping",
                      "How long no app can use the camera before the stream stops.")

        card = create_card()
        card_lay = card_layout(card)
        self._value = NoScrollSpinBox()
        self._unit = NoScrollComboBox()
        for text, _scale in self._UNITS:
            self._unit.addItem(text)
        self._unit.currentIndexChanged.connect(self._on_unit_changed)
        fields = QHBoxLayout()
        fields.setContentsMargins(0, 0, 0, 0)
        fields.setSpacing(8)
        for field in (self._value, self._unit):  # the two share the control column equally
            field.setSizePolicy(QSizePolicy.Policy.Expanding, QSizePolicy.Policy.Fixed)
            field.setMinimumWidth(1)
            fields.addWidget(field, 1)
        card_lay.addLayout(control_row("Wait", fields, stretch=True))
        lay.addWidget(card)

        cancel = QPushButton("Cancel")
        cancel.clicked.connect(self.reject)
        save = QPushButton("Save")
        set_ui_role(save, "primary")
        save.setDefault(True)
        save.clicked.connect(self._save)
        dialog_buttons(lay, cancel, save)

    def _scale(self) -> int:
        return self._UNITS[self._unit.currentIndex()][1]

    def _set_range(self):
        scale = self._scale()
        self._value.setRange(-(-MIN_STOP_DELAY_S // scale), MAX_STOP_DELAY_S // scale)

    def refresh(self):
        seconds = self._plugin.stop_delay_s
        self._unit.blockSignals(True)
        self._unit.setCurrentIndex(1 if seconds % 60 == 0 else 0)
        self._unit.blockSignals(False)
        self._set_range()
        self._value.setValue(seconds // self._scale())

    def _on_unit_changed(self, _index: int):
        # Keep roughly the same wait: 90 s becomes 2 min, 2 min becomes 120 s.
        old_scale = 60 if self._scale() == 1 else 1
        seconds = self._value.value() * old_scale
        self._set_range()
        self._value.setValue(round(seconds / self._scale()))

    def seconds(self) -> int:
        return clamp_delay(self._value.value() * self._scale())

    def _save(self):
        self._plugin.set_stop_delay(self.seconds())
        self.accept()
