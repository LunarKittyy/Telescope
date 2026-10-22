"""Auto framing: zooms and pans the view to keep the people in the picture framed, through the window's view."""
import math
import time
from dataclasses import dataclass
from typing import Optional

from PyQt6.QtCore import QTimer
from PyQt6.QtWidgets import QButtonGroup, QSizePolicy, QWidget

from telescope.plugin import TelescopePlugin
from telescope.shortcuts import button_action, choice_action
from telescope.view import pan_for, shown_to_view, view_to_shown, window_centre
from telescope.widgets.common import (
    SegmentButton, Switch, add_card_header, card_layout, control_row as _row, create_card, dim_until_paired,
    segmented_row,
)
from telescope.widgets.vision_status import VisionStatus

FRAMINGS = (("head", "Head and shoulders"), ("body", "Upper body"))
SPEEDS = (("calm", "Calm"), ("quick", "Quick"))
_OMEGA = {"calm": 1.6, "quick": 4.0}  # how fast the framing catches up (spring stiffness, 1/s)
_TICK_MS = 33
_MAX_ZOOM = 4.0  # closer than this gets too soft to be worth it
_LOST_S = 2.0  # nobody seen for this long: back to the whole picture
_DEAD_CENTRE = 0.08  # the framing only moves once the target's centre is this far off (of the window's size)
_DEAD_ZOOM = 1.15  # or its zoom this much
_HAND_OUT = 1.3  # a wrist this many shoulder widths out from its shoulder is holding something out
_HAND_ON_S = 0.3
_HAND_OFF_S = 0.8
_SEEN = 0.5  # landmark visibility the model has to be this sure of
_HANDS = ("wrist", "index", "pinky", "thumb")


@dataclass(frozen=True)
class Target:
    """Where the framing wants to be: the centre of the window in the picture as shown (0..1), and the zoom."""
    x: float
    y: float
    zoom: float


def _mid(a, b):
    return ((a[0] + b[0]) / 2, (a[1] + b[1]) / 2)


def _body(person, aspect: float) -> Optional[tuple]:
    """(shoulder centre, shoulder width in picture heights) or None without both shoulders."""
    left, right = person.point("left_shoulder", _SEEN), person.point("right_shoulder", _SEEN)
    if left is None or right is None:
        return None
    width = math.hypot((left[0] - right[0]) * aspect, left[1] - right[1])
    return (_mid(left, right), width) if width > 1e-3 else None


def hand_out(people, aspect: float) -> bool:
    """Someone holds a hand out to the side (showing something), above their hips."""
    for person in people:
        body = _body(person, aspect)
        if body is None:
            continue
        (_cx, sy), width = body
        for side in ("left", "right"):
            shoulder, wrist = person.point(f"{side}_shoulder", _SEEN), person.point(f"{side}_wrist", _SEEN)
            hip = person.point(f"{side}_hip", _SEEN)
            below = hip[1] if hip else sy + 1.6 * width
            if wrist and abs(wrist[0] - shoulder[0]) * aspect > _HAND_OUT * width and wrist[1] < below:
                return True
    return False


def target_box(people, aspect: float, framing: str = "head", hands: bool = False) -> Optional[tuple]:
    """(x0, y0, x1, y1) in the picture as shown around everyone, or None when nobody's clear enough to frame."""
    boxes = []
    for person in people:
        body = _body(person, aspect)
        if body is None:
            continue
        (cx, sy), width = body
        head = [p for p in (person.point(n, _SEEN) for n in ("nose", "left_eye", "right_eye")) if p]
        top = (min(p[1] for p in head) if head else sy - 0.6 * width) - 0.55 * width
        if framing == "body":
            hips = [p for p in (person.point(n, _SEEN) for n in ("left_hip", "right_hip")) if p]
            bottom = max(p[1] for p in hips) if hips else sy + 1.4 * width
            half = 1.05 * width / aspect
        else:
            bottom = sy + 0.6 * width
            half = 0.85 * width / aspect
        box = [cx - half, top, cx + half, bottom]
        if hands:
            margin = 0.35 * width
            for side in ("left", "right"):
                for p in (person.point(f"{side}_{n}", _SEEN) for n in _HANDS):
                    if p:
                        box = [min(box[0], p[0] - margin / aspect), min(box[1], p[1] - margin),
                               max(box[2], p[0] + margin / aspect), max(box[3], p[1] + margin)]
        boxes.append(box)
    if not boxes:
        return None
    return (min(b[0] for b in boxes), min(b[1] for b in boxes), max(b[2] for b in boxes), max(b[3] for b in boxes))


def fit(box: tuple, max_zoom: float = _MAX_ZOOM) -> Target:
    """The window (same shape as the picture) that holds box, as close as max_zoom allows and inside the picture."""
    x0, y0, x1, y1 = box
    zoom = min(max(1.0 / max(x1 - x0, y1 - y0, 1e-6), 1.0), max_zoom)
    half = 0.5 / zoom
    x = min(max((x0 + x1) / 2, half), 1 - half)
    y = min(max((y0 + y1) / 2, half), 1 - half)
    return Target(x, y, zoom)


def settled(goal: Target, target: Target) -> bool:
    """target is close enough to goal that moving to it would only make the framing wander."""
    size = 1.0 / goal.zoom
    near = abs(goal.x - target.x) < _DEAD_CENTRE * size and abs(goal.y - target.y) < _DEAD_CENTRE * size
    return near and 1 / _DEAD_ZOOM < target.zoom / goal.zoom < _DEAD_ZOOM


class Spring:
    """Critically damped: heads for its target without overshooting; zoom moves in log space so it feels even."""

    def __init__(self, at: Target):
        self.at = at
        self._v = (0.0, 0.0, 0.0)

    def step(self, goal: Target, dt: float, omega: float) -> Target:
        now = (self.at.x, self.at.y, math.log(self.at.zoom))
        want = (goal.x, goal.y, math.log(goal.zoom))
        f, k = 1 + 2 * dt * omega, dt * omega * omega
        det = 1 / (f + dt * k)
        moved = [((f * x + dt * v + dt * k * w) * det, (v + k * (w - x)) * det) for x, v, w in zip(now, self._v, want)]
        self._v = tuple(v for _x, v in moved)
        self.at = Target(moved[0][0], moved[1][0], math.exp(moved[2][0]))
        return self.at


class _Latch:
    """On after its input has been on for on_s, off after it's been off for off_s."""

    def __init__(self, on_s: float, off_s: float):
        self._on_s, self._off_s = on_s, off_s
        self.on = False
        self._since = None

    def update(self, value: bool, now: float) -> bool:
        if value == self.on:
            self._since = None
        elif self._since is None:
            self._since = now
        elif now - self._since >= (self._on_s if value else self._off_s):
            self.on, self._since = value, None
        return self.on


class FramingPlugin(TelescopePlugin):
    name = "framing"
    title = "Auto framing"
    about = "Zooms and pans to keep you in the picture as you move."
    when_off = "Without it there's no Auto framing card and the zoom stays where you put it."
    optional = True
    needs_vision = True
    panel_region = "left"

    def setup(self, host, bus):
        self._host, self._bus = host, bus
        self._people, self._seen_at = [], None
        self._spring: Optional[Spring] = None
        self._goal: Optional[Target] = None
        self._hands = _Latch(_HAND_ON_S, _HAND_OFF_S)
        self._ticked_at = None
        self._max_zoom = _MAX_ZOOM
        self._steering = False
        self._timer = QTimer()
        self._timer.setInterval(_TICK_MS)
        self._timer.timeout.connect(self._tick)
        bus.people_seen.connect(self._on_people)
        bus.max_zoom_changed.connect(lambda most: setattr(self, "_max_zoom", min(_MAX_ZOOM, float(most))))

    # ── Card ──────────────────────────────────────────────────────────────

    def create_panel(self) -> QWidget:
        card = create_card()
        lay = card_layout(card)
        self._switch = Switch("")
        self._switch.setSizePolicy(QSizePolicy.Policy.Fixed, QSizePolicy.Policy.Fixed)
        self._switch.setToolTip("Follow you with the zoom and pan")
        self._switch.toggled.connect(self._on_switch)
        add_card_header(lay, "Auto framing", "framing", action=self._switch, hue="vision")
        dim_until_paired(card, self._bus)
        self._framing_btns = self._segments(lay, "Framing", FRAMINGS, "head")
        self._speed_btns = self._segments(lay, "Speed", SPEEDS, "calm")
        self._hand_switch = Switch("Widen for something in your hand")
        self._hand_switch.setChecked(True)
        self._hand_switch.toggled.connect(lambda _on: self._host.schedule_save())
        lay.addWidget(self._hand_switch)
        self._status = VisionStatus(self._host, self._bus)
        lay.addWidget(self._status)
        self._show()
        return card

    def _segments(self, lay, label: str, options, default: str) -> dict:
        btns, group = {}, QButtonGroup(lay.parentWidget())
        for key, text in options:
            btn = SegmentButton(text)
            btn.setProperty("shortcutValue", key)
            btn.clicked.connect(lambda _c=False: self._host.schedule_save())
            group.addButton(btn)
            btns[key] = btn
        btns[default].setChecked(True)
        lay.addLayout(_row(label, segmented_row(*btns.values()), stretch=True))
        return btns

    def create_actions(self) -> list:
        g = "Auto framing"
        return [
            button_action("framing.on", "Auto framing", g, self._switch),
            choice_action("framing.framing", "Framing", g, buttons=list(self._framing_btns.values())),
        ]

    @staticmethod
    def _picked(btns: dict) -> str:
        return next((k for k, btn in btns.items() if btn.isChecked()), next(iter(btns)))

    # ── On and off (Qt thread) ────────────────────────────────────────────

    def _on_switch(self, on: bool):
        self._host.schedule_save()
        self._apply(on)

    def _apply(self, on: bool):
        if on and not self._steering:
            if not self._host.take_view(self.title):
                self._status.refresh(True, "Something else is moving the zoom and pan.")
                return
            self._steering = True
            self._host.want_people(self.title, True)
            self._spring = self._goal = None
            self._ticked_at = None
            self._timer.start()
        elif not on and self._steering:
            self._steering = False
            self._timer.stop()
            self._host.want_people(self.title, False)
            self._host.release_view(self.title)
            self._people, self._seen_at = [], None
        self._show()

    def _show(self):
        if hasattr(self, "_status"):
            self._status.refresh(self._switch.isChecked())

    # ── Following (Qt thread) ─────────────────────────────────────────────

    def _on_people(self, source_id: str, people: list):
        if self._steering and source_id == (self._host.focused_source_id() or ""):
            self._people, self._seen_at = people, time.monotonic()

    def on_focus_gained(self, stream_url: str, ctrl):
        self._people, self._seen_at, self._spring, self._goal = [], None, None, None

    def _start_from_view(self) -> Spring:
        view = self._host.view()
        x, y = window_centre(view.zoom, view.pan_x), window_centre(view.zoom, view.pan_y)
        sx, sy = view_to_shown(x, y, 1.0, 0.0, 0.0, view.flip_h, view.flip_v, view.rotation)
        return Spring(Target(sx, sy, max(view.zoom, 1.0)))

    def _tick(self):
        now = time.monotonic()
        dt = min(now - self._ticked_at, 0.2) if self._ticked_at is not None else _TICK_MS / 1000
        self._ticked_at = now
        if self._spring is None:
            self._spring = self._start_from_view()
        target = self._target(now)
        if self._goal is None or not settled(self._goal, target):
            self._goal = target
        before = self._spring.at
        at = self._spring.step(self._goal, dt, _OMEGA[self._picked(self._speed_btns)])
        if abs(at.x - before.x) + abs(at.y - before.y) + abs(at.zoom - before.zoom) > 1e-4:
            self._steer(at)

    def _target(self, now: float) -> Target:
        aspect = self._host.view_aspect() or 16 / 9
        fresh = self._seen_at is not None and now - self._seen_at < _LOST_S
        people = self._people if fresh else []
        hands = self._hand_switch.isChecked() and self._hands.update(hand_out(people, aspect), now)
        box = target_box(people, aspect, self._picked(self._framing_btns), hands)
        return fit(box, self._max_zoom) if box else Target(0.5, 0.5, 1.0)

    def _steer(self, at: Target):
        view = self._host.view()
        x, y = shown_to_view(at.x, at.y, 1.0, 0.0, 0.0, view.flip_h, view.flip_v, view.rotation)
        self._host.steer_view(self.title, at.zoom, pan_for(x, at.zoom), pan_for(y, at.zoom))

    def shutdown(self):
        self._apply(False)

    # ── Config ────────────────────────────────────────────────────────────

    def get_config(self) -> dict:
        return {"on": self._switch.isChecked(), "framing": self._picked(self._framing_btns),
                "speed": self._picked(self._speed_btns), "hand": self._hand_switch.isChecked()}

    def set_config(self, cfg: dict):
        self._framing_btns.get(str(cfg.get("framing")), self._framing_btns["head"]).setChecked(True)
        self._speed_btns.get(str(cfg.get("speed")), self._speed_btns["calm"]).setChecked(True)
        self._hand_switch.blockSignals(True)
        self._hand_switch.setChecked(cfg.get("hand") is not False)
        self._hand_switch.blockSignals(False)
        self._switch.blockSignals(True)
        self._switch.setChecked(cfg.get("on") is True)
        self._switch.blockSignals(False)
        self._apply(self._switch.isChecked())

    def diagnostics(self) -> dict:
        return {"Auto framing": "on" if self._steering else "off"}
