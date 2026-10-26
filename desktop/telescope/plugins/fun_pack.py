import math
from typing import Optional

import numpy as np

from PyQt6.QtCore import QObject, Qt, QTimer, pyqtSignal
from PyQt6.QtWidgets import QButtonGroup, QComboBox, QPushButton, QWidget

from telescope import fun_effects, vision
from telescope.fun_effects import Clones, Look
from telescope.h264_mosh import Mosh, Mosher
from telescope.config import clamped_int
from telescope.i18n import N_, tr
from telescope.pictures import soften
from telescope.plugin import TelescopePlugin
from telescope.pose import BONES
from telescope.shortcuts import SHORTCUT_VALUE_ROLE, button_action, choice_action, slider_action
from telescope.view import upright_to_shown
from telescope.widgets.common import (
    NoScrollComboBox, NoScrollSlider, SegmentButton, add_card_header, button_row, card_layout, control_row_widget,
    create_card, dim_until_paired, segmented_row, slider_row, value_label, wrapped_note,
)
from telescope.widgets.vision_status import VisionStatus

EFFECTS = (
    ("off", N_("Off")),
    ("clones", N_("Clone party")),
    ("cloak", N_("Invisibility cloak")),
    ("light", N_("Light painting")),
    ("slit", N_("Slit-scan")),
    ("stick", N_("Stick figure")),
    ("game_boy", N_("Game Boy")),
    ("webcam", N_("1998 webcam")),
    ("vhs", N_("VHS")),
    ("crt", N_("CRT")),
    ("ascii", N_("ASCII")),
    ("mosh", N_("Datamosh")),
)
HINTS = {
    "clones": N_("Add a clone, move somewhere else and add another."),
    "cloak": N_("Take the empty room, then step back in."),
    "light": N_("Move a torch or a phone screen around in the dark."),
    "slit": N_("Each band of the picture runs a little behind the one before."),
    "stick": N_("A stick figure takes your place."),
    "mosh": N_("Move around and the picture smears instead of refreshing."),
}
_PLATE = ("clones", "cloak", "stick")  # effects that use the empty room
_EDGES = ("clones", "cloak")
_COUNTDOWN_S = 3
_FACE = ("eye", "ear", "nose")


def stick_figure(people: list, map_point, aspect: float) -> tuple:
    """(lines, heads) for Look from people_seen's people, through map_point onto the frame as shown."""
    lines, heads = [], []
    for person in people:
        pts = {}
        for name in person.points:
            p = person.point(name)
            if p is not None:
                pts[name] = map_point(*p)
        for a, b in BONES:
            if a in pts and b in pts and not any(f in a or f in b for f in _FACE):
                lines.append((pts[a], pts[b]))
        ears = [pts[n] for n in ("left_ear", "right_ear", "left_eye", "right_eye") if n in pts]
        head = pts.get("nose")
        if head is None and len(ears) >= 2:
            head = ((ears[0][0] + ears[1][0]) / 2, (ears[0][1] + ears[1][1]) / 2)
        if head is not None:
            r = 0.05
            if len(ears) >= 2:
                r = max(0.02, math.hypot((ears[0][0] - ears[1][0]) * aspect, ears[0][1] - ears[1][1]) * 0.8)
            heads.append((head, r))
            if "left_shoulder" in pts and "right_shoulder" in pts:
                (lx, ly), (rx, ry) = pts["left_shoulder"], pts["right_shoulder"]
                lines.append((((lx + rx) / 2, (ly + ry) / 2), (head[0], head[1] + r)))
    return tuple(lines), tuple(heads)


class _Relay(QObject):
    """Hands what process_frame took (the empty room, a clone) to the GUI thread."""

    taken = pyqtSignal(str, object)


class FunPackPlugin(TelescopePlugin):
    name = "fun_pack"
    title = N_("Fun pack")
    about = N_("Silly effects: clones, an invisibility cloak, light painting, retro looks and datamosh.")
    category = "look"
    icon = "sparkle"
    frame_stage = "effects"
    when_off = N_("Without it there's no Fun pack card and the picture has no effects.")
    optional = True
    in_presets = True
    device_local = True  # an effect on one camera stays on that camera
    panel_region = "right"

    def setup(self, host, bus):
        self._host, self._bus = host, bus
        self._look = Look()  # Written by Qt thread; read by worker thread (swapped whole, GIL-atomic).
        self._mosh = Mosh()  # The same, for the stream's packet step
        self._runner = fun_effects.Runner()
        self._grab: Optional[str] = None  # "plate" or "clone": process_frame takes the next frame for it
        self._plate: Optional[np.ndarray] = None
        self._clones: Optional[Clones] = None
        self._taken_with = None  # (source, view, frame shape) the empty room and clones were taken with
        self._stale = False  # they were dropped because one of those changed
        self._stale_sent = False  # process_frame told the GUI thread about a new frame shape already
        self._people: list = []
        self._lines, self._heads = (), ()
        self._reset = 0
        self._countdown, self._counting = 0, ""
        self._mosh_on = False
        self._wants_people = False
        self._problem = ""
        self._relay = _Relay()
        self._relay.taken.connect(self._on_taken)
        self._timer = QTimer()
        self._timer.setInterval(1000)
        self._timer.timeout.connect(self._tick)
        bus.people_seen.connect(self._on_people)
        bus.view_changed.connect(lambda _view: (self._check_taken(), self._map_people()))
        bus.source_selected.connect(lambda _id: self._new_source())
        bus.device_changed.connect(lambda _id: self._new_source())
        for signal in (bus.stream_transport_changed, bus.stream_stopped, bus.stream_started, bus.source_selected,
                       bus.device_changed):
            signal.connect(lambda *_a: self._refresh())

    # ── Card ──────────────────────────────────────────────────────────────

    def create_panel(self) -> QWidget:
        card = create_card()
        lay = card_layout(card)
        add_card_header(lay, tr("Fun pack"), self.icon, hue=self.category)
        dim_until_paired(card, self._bus)

        self._combo = NoScrollComboBox()
        self._combo.setSizeAdjustPolicy(QComboBox.SizeAdjustPolicy.AdjustToMinimumContentsLengthWithIcon)
        self._combo.setMinimumContentsLength(12)  # a long effect name can't widen the card; the list opens wider
        for key, text in EFFECTS:
            self._combo.addItem(tr(text), key)
            self._combo.setItemData(self._combo.count() - 1, text, SHORTCUT_VALUE_ROLE)
        self._combo.currentIndexChanged.connect(self._on_effect)
        lay.addWidget(control_row_widget(tr("Effect"), self._combo, stretch=True))
        self._hint = wrapped_note("")
        lay.addWidget(self._hint)

        self._plate_btn = QPushButton(tr("Take the empty room"))
        self._plate_btn.setToolTip(tr("Step out of the picture: it's taken after a short countdown"))
        self._plate_btn.clicked.connect(lambda: self._start_countdown("plate"))
        self._clone_btn = QPushButton(tr("Add a clone"))
        self._clone_btn.setToolTip(tr("Get in place: you're copied after a short countdown"))
        self._clone_btn.clicked.connect(lambda: self._start_countdown("clone"))
        self._clear_btn = QPushButton(tr("Clear"))
        self._clear_btn.clicked.connect(self._clear)
        self._clean_btn = QPushButton(tr("Clean up"))
        self._clean_btn.setToolTip(tr("Let the next keyframe through so the picture comes back"))
        self._clean_btn.clicked.connect(self._clean_up)
        self._plate_row = QWidget()
        self._plate_row.setLayout(button_row(self._plate_btn))
        lay.addWidget(self._plate_row)
        self._buttons = QWidget()
        self._buttons.setLayout(button_row(self._clone_btn, self._clean_btn, self._clear_btn))
        lay.addWidget(self._buttons)

        self._sliders = {}
        for key, label, lo, hi, default, tip in (
                ("edges", N_("Edges"), 0, 10, 3, N_("How soft the edge around you is")),
                ("fade", N_("Trails"), 1, 10, 5, N_("How long the light trails last")),
                ("brightness", N_("Brightness"), 1, 10, 6, N_("How bright a light has to be to paint")),
                ("spread", N_("Spread"), 5, 45, 20, N_("Frames between one end of the picture and the other")),
                ("smear", N_("Smear"), 0, 3, 0, N_("Plays each frame's motion this many extra times"))):
            slider = NoScrollSlider(Qt.Orientation.Horizontal)
            slider.setRange(lo, hi)
            slider.setValue(default)
            slider.set_default(default)
            slider.setToolTip(tr(tip))
            readout = value_label(str(default))
            slider.valueChanged.connect(lambda v, r=readout: (r.setText(str(v)), self._apply(), self._host.schedule_save()))
            row = control_row_widget(tr(label), slider_row(slider, readout), stretch=True)
            lay.addWidget(row)
            self._sliders[key] = (slider, readout, row)

        self._dir_btns = {}
        group = QButtonGroup(card)
        for key, text in (("down", N_("Down")), ("across", N_("Across"))):
            btn = SegmentButton(tr(text), value=key)
            btn.clicked.connect(lambda _c=False: (self._apply(), self._host.schedule_save()))
            group.addButton(btn)
            self._dir_btns[key] = btn
        self._dir_btns["down"].setChecked(True)
        self._dir_row = control_row_widget(tr("Direction"), segmented_row(*self._dir_btns.values()), stretch=True)
        lay.addWidget(self._dir_row)

        self._note = wrapped_note("", "status_warn")
        lay.addWidget(self._note)
        self._status = VisionStatus(self._host, self._bus)
        lay.addWidget(self._status)
        self._show()
        return card

    def create_actions(self) -> list:
        g = tr("Fun pack")
        return [
            choice_action("fun_pack.effect", tr("Fun pack effect"), g, combo=self._combo),
            button_action("fun_pack.plate", tr("Take the empty room"), g, self._plate_btn),
            button_action("fun_pack.clone", tr("Add a clone"), g, self._clone_btn),
            button_action("fun_pack.clean", tr("Clean up the datamosh"), g, self._clean_btn),
            slider_action("fun_pack.smear", tr("Datamosh smear"), g, self._sliders["smear"][0], step=1.0,
                          readout=self._sliders["smear"][1]),
        ]

    # ── Handlers (Qt thread) ──────────────────────────────────────────────

    def _effect(self) -> str:
        return self._combo.currentData() or "off"

    def _value(self, key: str) -> int:
        return self._sliders[key][0].value()

    def _on_effect(self, _index: int):
        self._timer.stop()
        self._counting, self._grab = "", None
        self._apply()
        self._host.schedule_save()

    def _start_countdown(self, what: str):
        self._counting, self._countdown = what, _COUNTDOWN_S
        self._timer.start()
        self._show()

    def _tick(self):
        self._countdown -= 1
        if self._countdown <= 0:
            self._timer.stop()
            self._grab, self._counting = self._counting, ""
        self._show()

    def _taken_key(self, shape) -> tuple:
        return self._host.focused_source_id() or "", self._host.view(), tuple(shape)

    def _on_taken(self, what: str, value):
        if what == "reshaped":
            self._drop_taken()
            return
        if value is not None:
            key = self._taken_key((value if what == "plate" else value.picture).shape)
            if key != self._taken_with:
                self._plate, self._clones = None, None
            self._taken_with, self._stale, self._stale_sent = key, False, False
        if what == "plate":
            self._plate = value
        elif what == "clone" and value is not None:
            self._clones = value
        self._problem = "" if value is not None else tr("Take the empty room first, so you can be cut out.")
        self._apply()

    def _drop_taken(self):
        """The empty room and clones no longer line up with the picture: the card asks for the room again."""
        if self._plate is None and self._clones is None:
            return
        self._plate, self._clones, self._taken_with = None, None, None
        self._stale, self._stale_sent = True, False
        self._apply()

    def _check_taken(self):
        if self._taken_with is not None and self._host.view() != self._taken_with[1]:
            self._drop_taken()

    def _new_source(self):
        self._runner = fun_effects.Runner()  # slit-scan history and light trails belong to the stream before
        self._drop_taken()

    def _clear(self):
        if self._effect() == "clones":
            self._clones = None
        self._reset += 1
        self._apply()

    def _clean_up(self):
        if self._mosh_on:  # off and on again: the phone is asked for a keyframe and a new Mosher lets it through
            self._host.change_packets(self.name, None)
            self._host.change_packets(self.name, lambda: Mosher(lambda: self._mosh, clean_up=True))

    def _make_mosher(self) -> Mosher:
        return Mosher(lambda: self._mosh)

    def _on_people(self, source_id: str, people: list):
        if self._effect() == "stick" and source_id == (self._host.focused_source_id() or ""):
            self._people = people
            self._map_people()

    def _map_people(self):
        if self._effect() != "stick":
            return
        view = self._host.view()

        def map_point(x, y):
            return upright_to_shown(view, x, y)
        self._lines, self._heads = stick_figure(self._people, map_point, self._host.view_aspect() or 16 / 9)
        self._apply(show=False)

    def _apply(self, show: bool = True):
        effect = self._effect()
        self._look = Look(
            effect=effect, plate=self._plate, clones=self._clones, edges=self._value("edges"),
            fade=self._value("fade"), brightness=self._value("brightness"), spread=self._value("spread"),
            across=self._dir_btns["across"].isChecked(), lines=self._lines, heads=self._heads,
            mask=self._person_mask if effect in _EDGES else None, reset=self._reset)
        self._mosh = Mosh(True, self._value("smear"))
        if show:
            self._refresh()

    def _refresh(self):
        """Packets, people and the card, after anything that changes what the effect needs."""
        if not hasattr(self, "_combo"):
            return
        effect = self._effect()
        mosh = effect == "mosh" and not self._host.packets_problem()
        if mosh != self._mosh_on:
            self._mosh_on = mosh
            self._host.change_packets(self.name, self._make_mosher if mosh else None)
        if (effect == "stick") != self._wants_people:  # only on a change: asking loads MediaPipe, which signals back here
            self._wants_people = effect == "stick"
            self._host.want_people(self.name, self._wants_people)
        if effect != "stick":
            self._people, self._lines, self._heads = [], (), ()
        self._show()

    def shutdown(self):
        if self._mosh_on:
            self._mosh_on = False
            self._host.change_packets(self.name, None)
        if self._wants_people:
            self._wants_people = False
            self._host.want_people(self.name, False)

    def _show(self):
        if not hasattr(self, "_combo"):
            return
        effect = self._effect()
        unavailable = effect == "mosh" and bool(self._host.packets_problem())
        hint = "" if unavailable else HINTS.get(effect, "")
        self._hint.setText(tr(hint) if hint else "")
        self._hint.setVisible(bool(hint))
        self._plate_row.setVisible(effect in _PLATE)
        counting = {"plate": self._plate_btn, "clone": self._clone_btn}.get(self._counting)
        self._plate_btn.setText(tr("Taking it in {n}…", n=self._countdown) if counting is self._plate_btn
                                else tr("Take the empty room again") if self._plate is not None
                                else tr("Take the empty room"))
        self._clone_btn.setText(tr("Copying you in {n}…", n=self._countdown) if counting is self._clone_btn
                                else tr("Add a clone"))
        full = self._clones is not None and self._clones.count >= Clones.MAX
        self._clone_btn.setEnabled(not self._counting and not full)
        self._plate_btn.setEnabled(not self._counting)
        self._clone_btn.setVisible(effect == "clones")
        self._clean_btn.setVisible(effect == "mosh" and not unavailable)
        self._clear_btn.setVisible(effect in ("clones", "light"))
        self._clear_btn.setEnabled(effect != "clones" or self._clones is not None)
        self._buttons.setVisible(effect in ("clones", "light") or effect == "mosh" and not unavailable)
        for key, effects in (("edges", _EDGES), ("fade", ("light",)), ("brightness", ("light",)),
                             ("spread", ("slit",)), ("smear", ("mosh",))):
            self._sliders[key][2].setVisible(effect in effects and not unavailable)
        self._dir_row.setVisible(effect == "slit")
        problem = ""
        if effect == "mosh":
            problem = self._host.packets_problem()
        elif effect in _PLATE and self._stale and self._plate is None:
            problem = tr("The picture changed, so take the empty room again.")
        elif effect == "cloak" and self._plate is None:
            problem = tr("Take the empty room first.")
        elif effect == "clones":
            problem = self._problem
        self._note.setText(problem)
        self._note.setVisible(bool(problem))
        self._status.refresh(effect in ("stick", *_EDGES), optional=effect in _EDGES)

    # ── Frames (worker thread) ────────────────────────────────────────────

    def process_frame(self, frame: np.ndarray) -> np.ndarray:
        grab, look = self._grab, self._look
        taken = look.plate if look.plate is not None else look.clones.picture if look.clones is not None else None
        if taken is not None and taken.shape != frame.shape and not self._stale_sent:
            self._stale_sent = True  # a new stream size: what was taken no longer lines up
            self._relay.taken.emit("reshaped", None)
        if grab is not None:
            self._grab = None
            self._take(grab, frame, look)
        return self._runner(frame, look)

    def _person_mask(self, frame: np.ndarray) -> Optional[np.ndarray]:
        # Without the download, asking would only make vision look for it again every few seconds
        return None if self._host.vision_state() == vision.MISSING else self._host.person_mask(frame)

    def _take(self, what: str, frame: np.ndarray, look: Look):
        if what == "plate":
            self._relay.taken.emit("plate", frame.copy())
            return
        mask = fun_effects.person(frame, look, fun_effects.PlateMask())
        clones = None if mask is None else Clones.add(look.clones, frame, soften(mask, look.edges))
        self._relay.taken.emit("clone", clones)

    def frame_step(self):
        look = self._look
        if look.effect not in fun_effects.EFFECTS or look.effect in _PLATE:  # the room and pose are the shown stream's
            return None
        runner = fun_effects.Runner()
        return lambda frame: runner(frame, look)

    # ── Config ────────────────────────────────────────────────────────────

    def get_config(self) -> dict:
        cfg = {"effect": self._effect(), "across": self._dir_btns["across"].isChecked()}
        cfg.update({key: self._value(key) for key in self._sliders})
        return cfg

    def set_config(self, cfg: dict):
        effect = cfg.get("effect") if cfg.get("effect") in dict(EFFECTS) else "off"
        for key, (slider, _readout, _row) in self._sliders.items():
            slider.setValue(clamped_int(cfg.get(key), slider.default(), slider.minimum(), slider.maximum()))
        self._dir_btns["across" if cfg.get("across") is True else "down"].setChecked(True)
        self._combo.blockSignals(True)
        self._combo.setCurrentIndex(self._combo.findData(effect))
        self._combo.blockSignals(False)
        self._apply()

    def diagnostics(self) -> dict:
        return {"Fun pack": self._effect()}
