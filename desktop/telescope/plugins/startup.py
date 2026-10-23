"""Starting and stopping on its own, and opening Telescope at sign-in.

The Automatic streaming card in Settings picks when a stream starts by itself (never, as soon as the phone is
ready, or when an app opens the camera) and when one stops by itself (never, or once no app has read the camera for a
while: only a stream it started, or any stream). Starting when the phone is ready happens once each time the phone
becomes ready, and keeps the phone checked with the window hidden (otherwise it's checked only while the window is on
screen or an app reads the camera); after a Stop it waits until the phone goes away and comes back, so Stop sticks.
Starting for an app happens once while that app reads the camera; after a Stop it waits until the app lets go and
something opens the camera again. The idle stop never acts before the camera watch has reported at least once, so a
machine where it can't tell whether an app reads the camera never has its streams stopped under it. With the phone mic
on, the idle stop turns only the camera off and the mic keeps streaming (a call that switched its camera off still hears
you); the camera comes back on when an app opens it again, and Stop ends the stream.
"""

import logging
from typing import Optional

from PyQt6.QtCore import QSignalBlocker, QTimer
from PyQt6.QtWidgets import QHBoxLayout, QVBoxLayout, QWidget

from telescope import dev_profile
from telescope.platform import autostart
from telescope.plugin import SettingsSection, TelescopePlugin
from telescope.widgets.settings_search import keywords
from telescope.widgets.banner import Issue
from telescope.widgets.common import (
    NoScrollComboBox, NoScrollSpinBox, Switch, add_card_header, card_layout, create_card, ui_px, wrapped_note,
)
from telescope.widgets.controls import setting_row
from telescope.i18n import N_, tr


logger = logging.getLogger(__name__)

START_OFF, START_READY, START_WATCHED = "off", "ready", "watched"
STOP_OFF, STOP_OWN, STOP_ANY = "off", "own", "any"
START_CHOICES = (
    (START_OFF, N_("Only when I press Start")),
    (START_READY, N_("When the phone is ready")),
    (START_WATCHED, N_("When an app opens the camera")),
)
STOP_CHOICES = (
    (STOP_OFF, N_("Only when I press Stop")),
    (STOP_OWN, N_("When no app uses the camera, if it started the stream")),
    (STOP_ANY, N_("When no app uses the camera, even if I started it")),
)

# The shortest wait is longer than the slowest camera watch takes to notice an app (the /proc scan runs every 5 s,
# and Windows waits 3 s before it counts a reader as gone), so a stream isn't stopped under an app just opening it.
MIN_STOP_DELAY_S = 10
MAX_STOP_DELAY_S = 3600
DEFAULT_STOP_DELAY_S = 15


def delay_text(seconds: int) -> str:
    """"90 s", "2 min": whole minutes read as minutes."""
    return tr("{n} min", n=seconds // 60) if seconds % 60 == 0 else tr("{n} s", n=seconds)


def clamp_delay(value) -> int:
    if not isinstance(value, int) or isinstance(value, bool):
        return DEFAULT_STOP_DELAY_S
    return max(MIN_STOP_DELAY_S, min(MAX_STOP_DELAY_S, value))


class StartupPlugin(TelescopePlugin):
    name = "startup"
    title = N_("Start and stop by itself")
    about = N_("Opening Telescope when you sign in, and Automatic streaming.")
    when_off = N_("Without it Automatic streaming stops and both it and the sign-in option leave Settings. "
                  "Telescope still opens at sign-in if it did before.")

    def __init__(self):
        self.start_on = START_OFF
        self.stop_idle = STOP_OFF
        self.stop_delay_s = DEFAULT_STOP_DELAY_S
        self.notify_stop = True
        self._held = False            # already started (or stopped) for this arrival of the phone
        self._wanted_sent: Optional[tuple] = None  # what phone_wanted last said
        self._phone: Optional[str] = None
        self._ready = False
        self._watched = False
        self._shown = True            # the window on screen (EventBus.window_shown)
        self._watch_known = False     # the camera watch has reported at least once
        self._watch_held = False      # already started (or stopped) while this app reads the camera
        self._starting_own = False    # asked the host to start; the next stream_started is ours
        self._started_own = False     # the running stream is one it started
        # Several cameras streamed since the last full stop: the one left may go to an extra camera, which the watch
        # doesn't see, so nothing stops by itself until everything has
        self._several = False

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
        bus.streams_changed.connect(self._on_streams_changed)
        bus.camera_watched.connect(self._on_camera_watched)
        bus.window_shown.connect(self._on_window_shown)

    # ── Settings window ───────────────────────────────────────────────────

    def create_settings_sections(self) -> list:
        self._sign_in = SignInSection(self)
        self._auto = AutomaticStreamingSection(self)
        return [SettingsSection("general", self._sign_in, on_show=self._sign_in.refresh),
                SettingsSection("streaming", self._auto, on_show=self._auto.refresh)]

    # ── Settings ──────────────────────────────────────────────────────────

    def set_start_on(self, mode: str):
        if mode not in dict(START_CHOICES) or mode == self.start_on:
            return
        self.start_on = mode
        self._set_held(False)
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
        self._sync_wanted()

    def _set_held(self, held: bool):
        self._held = held
        self._sync_wanted()

    def _sync_wanted(self):
        # Waiting to start when ready: the phone is checked from the tray too. Once held, only the phone going away
        # (which lifts the hold) matters, so a phone left waiting all day is checked slowly.
        wanted = (self.start_on == START_READY, self._held)
        if wanted != self._wanted_sent:
            self._wanted_sent = wanted
            self._bus.phone_wanted.emit(*wanted)

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
                self._set_held(False)  # another phone: a new arrival (the first report just names it)
            self._phone = phone_id
        self._ready = ready
        if not ready:
            self._set_held(False)  # gone: the next time it's ready counts as a new arrival
            return
        if (self.start_on == START_READY and not self._held
                and not self._host.is_streaming() and not self._host.is_starting()):
            # Held before starting, so a start that fails isn't retried on every status check.
            self._set_held(True)
            self._start_own("Starting the stream now the phone is ready")
        self._maybe_start_for_watch()

    def _on_device_changed(self, _name: str):
        self._set_held(False)
        self._ready = False  # the new phone's first status check says whether it's ready

    # ── Starting when an app opens the camera ─────────────────────────────

    def _on_window_shown(self, shown: bool):
        self._shown = shown
        self._forget_ready_if_unchecked()

    def _forget_ready_if_unchecked(self):
        # The phone isn't checked while the window is hidden and no app reads the camera (unless starting when ready,
        # which never reads _ready), so what it said last can go stale. Once checks resume, the first one answers with phone_ready and anything waiting on it starts then.
        if not self._shown and not self._watched:
            self._ready = False

    def _on_camera_watched(self, watched: bool):
        self._watch_known = True
        self._watched = watched
        self._forget_ready_if_unchecked()
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

    def _on_streams_changed(self, count: int):
        if count > 1:
            self._several = True
        self._sync_idle_timer()

    def _on_stream_start_failed(self):
        self._starting_own = False
        self._sync_idle_timer()

    def _on_stream_stopped(self):
        self._set_held(True)
        if self._host.is_restarting():
            # The same stream back in a moment (a reconnect or camera resize): still one it started.
            self._starting_own = self._starting_own or self._started_own
            self._started_own = False
            return
        self._forget_own_stream()
        self._several = False
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
                and self._host.stream_count() <= 1 and not self._several)  # each camera is somebody's

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
                tr("Telescope stopped streaming"),
                tr("No app used the camera for {time}.", time=delay_text(self.stop_delay_s)),
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


class SignInSection(QWidget):
    """Opening Telescope when the person signs in to the computer."""

    def __init__(self, plugin: StartupPlugin):
        super().__init__()
        self._plugin = plugin
        outer = QVBoxLayout(self)
        outer.setContentsMargins(0, 0, 0, 0)
        card = create_card()
        lay = card_layout(card)
        add_card_header(lay, tr("Startup"), "monitor")
        keywords(card, tr("login boot autostart run on startup tray minimized", ctx="search"))
        self._box = Switch()
        self._box.toggled.connect(plugin.set_open_at_sign_in)
        strip, _ = setting_row(tr("Open Telescope when I sign in"), "", self._box,
                               keywords=tr("autostart login log in boot startup run on startup launch computer starts "
                                           "tray", ctx="search"))
        if dev_profile.active():  # the sign-in entry is the real app's
            self._box.setEnabled(False)
            strip.setToolTip(tr("Not in a dev profile"))
        lay.addWidget(strip)
        outer.addWidget(card)

    def refresh(self):
        with QSignalBlocker(self._box):
            self._box.setChecked(autostart.is_enabled())


class AutomaticStreamingSection(QWidget):
    """When a stream starts by itself and when it stops by itself, with the wait before an idle stop."""

    _UNITS = ((N_("seconds"), 1), (N_("minutes"), 60))

    def __init__(self, plugin: StartupPlugin):
        super().__init__()
        self._plugin = plugin
        outer = QVBoxLayout(self)
        outer.setContentsMargins(0, 0, 0, 0)
        card = create_card()
        lay = card_layout(card)
        add_card_header(lay, tr("Automatic streaming"), "bolt")
        keywords(card, tr("auto start stop camera app opens discord obs zoom meet teams phone ready idle hands free",
                          ctx="search"))

        self._start = self._choice(START_CHOICES, plugin.set_start_on)
        strip, _ = setting_row(tr("Start", ctx="setting"), "", None, self._start,
                               keywords=tr("automatically begin streaming when phone ready app opens the camera auto "
                                           "start", ctx="search"))
        lay.addWidget(strip)
        self._stop = self._choice(STOP_CHOICES, plugin.set_stop_idle)
        strip, _ = setting_row(tr("Stop", ctx="setting"), "", None, self._stop,
                               keywords=tr("automatically end streaming idle no app uses the camera auto stop", ctx="search"))
        lay.addWidget(strip)
        self._mic_note = wrapped_note(tr("With the phone mic on, only the camera turns off."))
        lay.addWidget(self._mic_note)

        self._value = NoScrollSpinBox()
        self._value.setKeyboardTracking(False)  # saved once it's typed, not at every digit
        self._unit = NoScrollComboBox()
        for text, _scale in self._UNITS:
            self._unit.addItem(tr(text))
        self._value.valueChanged.connect(self._save_delay)
        self._unit.activated.connect(self._on_unit_changed)
        fields = QHBoxLayout()
        fields.setSpacing(8)
        for field in (self._value, self._unit):
            field.setMinimumWidth(ui_px(90))
            fields.addWidget(field)
        self._delay_row, _ = setting_row(tr("Wait before stopping"), tr("How long no app can use the camera before "
                                                                        "the stream stops."), fields,
                                         keywords=tr("delay timeout idle seconds minutes how long grace period auto "
                                                     "stop", ctx="search"))
        lay.addWidget(self._delay_row)

        self._notify = Switch()
        self._notify.toggled.connect(plugin.set_notify_stop)
        self._notify_row, _ = setting_row(tr("Tell me when it stops a stream"), "", self._notify,
                                          keywords=tr("notification notify alert popup message", ctx="search"))
        lay.addWidget(self._notify_row)
        outer.addWidget(card)

    @staticmethod
    def _choice(choices, setter) -> NoScrollComboBox:
        combo = NoScrollComboBox()
        for mode, text in choices:
            combo.addItem(tr(text), mode)
        combo.activated.connect(lambda i, c=combo: setter(c.itemData(i)))
        return combo

    def _scale(self) -> int:
        return self._UNITS[self._unit.currentIndex()][1]

    def _set_range(self):
        scale = self._scale()
        self._value.setRange(-(-MIN_STOP_DELAY_S // scale), MAX_STOP_DELAY_S // scale)

    def refresh(self):
        p = self._plugin
        for combo, current in ((self._start, p.start_on), (self._stop, p.stop_idle)):
            with QSignalBlocker(combo):
                combo.setCurrentIndex(max(0, combo.findData(current)))
        stops = p.stop_idle != STOP_OFF  # the wait and the notice do nothing without an idle stop
        self._mic_note.setVisible(stops)
        self._delay_row.setEnabled(stops)
        self._notify_row.setEnabled(stops)
        with QSignalBlocker(self._unit), QSignalBlocker(self._value):
            self._unit.setCurrentIndex(1 if p.stop_delay_s % 60 == 0 else 0)
            self._set_range()
            self._value.setValue(p.stop_delay_s // self._scale())
        with QSignalBlocker(self._notify):
            self._notify.setChecked(p.notify_stop)

    def _on_unit_changed(self, _index: int):
        # Keep roughly the same wait: 90 s becomes 2 min, 2 min becomes 120 s.
        old_scale = 60 if self._scale() == 1 else 1
        seconds = self._value.value() * old_scale
        with QSignalBlocker(self._value):
            self._set_range()
            self._value.setValue(round(seconds / self._scale()))
        self._save_delay()

    def seconds(self) -> int:
        return clamp_delay(self._value.value() * self._scale())

    def _save_delay(self, _value: int = 0):
        self._plugin.set_stop_delay(self.seconds())
