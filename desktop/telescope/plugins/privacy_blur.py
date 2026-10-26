"""Privacy blur: when someone else shows up, blurs everyone but you, the whole
background, or puts the wait screen up instead."""
import time
from dataclasses import dataclass
from typing import Optional

import cv2
import numpy as np

from PyQt6.QtCore import Qt
from PyQt6.QtWidgets import QComboBox, QPushButton, QSizePolicy, QWidget

from telescope import people
from telescope.plugin import TelescopePlugin
from telescope.shortcuts import SHORTCUT_VALUE_ROLE, button_action, choice_action
from telescope.view import shown_to_view, view_to_shown
from telescope.widgets.common import (
    NoScrollComboBox, Switch, add_card_header, card_layout, create_card, dim_until_paired,
)
from telescope.widgets.vision_status import VisionStatus
from telescope.i18n import N_, tr

# Translators: what happens while someone else is in the picture: the other people get blurred, everything but you
# gets blurred, or the wait screen goes up in place of the whole picture
MODES = (("others", N_("Blur the others")), ("background", N_("Blur all but me")),
         ("wait", N_("Show the wait screen")))
OFF_S = 5.0  # blurring stops once there's been one person (or nobody) for this long
_GROW = 0.4  # a person's box from their landmarks grows by this much of its size each way (arms, a chair)
_HEAD = 1.0  # and upwards by this much of its width more, since the landmarks stop at the eyes
_SMALL = 16  # the blur is worked out at a sixteenth of the size
_EDGE = 0.04  # the edge of a blurred box fades over this much of the picture


class Trigger:
    """When to blur, from the steady people count: on as soon as it says two or more (people.py only counts someone
    new after a couple of updates in a row), off once it has said one or none for OFF_S."""

    def __init__(self):
        self.on = False
        self._calm_since: Optional[float] = None

    def update(self, count: int, now: float) -> bool:
        if count >= 2:
            self.on, self._calm_since = True, None
        elif self.on:
            if self._calm_since is None:
                self._calm_since = now
            if now - self._calm_since >= OFF_S:
                self.on, self._calm_since = False, None
        return self.on

    def reset(self):
        self.on, self._calm_since = False, None


@dataclass(frozen=True)
class _Blur:
    """What a frame step needs, frozen: swapped whole by the Qt thread, read by the worker thread."""
    mode: str = "others"
    active: bool = False
    you: Optional[tuple] = None  # your box, in the picture as shown at zoom 1
    others: tuple = ()  # everyone else's
    covered: bool = False  # the wait screen is up in place of the picture


def grown(box: tuple, by: float = _GROW) -> tuple:
    w, h = box[2] - box[0], box[3] - box[1]
    return box[0] - w * by, box[1] - h * by - w * _HEAD, box[2] + w * by, box[3] + h * by


def shown_box(box: tuple, view) -> tuple:
    """box from the picture as shown at zoom 1 into the frame as shown now (0..1, not clipped)."""
    turn = (view.flip_h, view.flip_v, view.rotation)
    corners = [view_to_shown(*shown_to_view(x, y, 1.0, 0.0, 0.0, *turn), view.zoom, view.pan_x, view.pan_y, *turn)
               for x in (box[0], box[2]) for y in (box[1], box[3])]
    return (min(p[0] for p in corners), min(p[1] for p in corners),
            max(p[0] for p in corners), max(p[1] for p in corners))


def blurred(frame: np.ndarray) -> np.ndarray:
    """frame blurred past recognising anyone, at a sixteenth of the size and scaled back up."""
    h, w = frame.shape[:2]
    small = cv2.resize(frame, (max(1, w // _SMALL), max(1, h // _SMALL)), interpolation=cv2.INTER_AREA)
    return cv2.resize(cv2.GaussianBlur(small, (0, 0), 2.0), (w, h), interpolation=cv2.INTER_LINEAR)


def boxes_mask(boxes, w: int, h: int) -> np.ndarray:
    """0..1 float32 at a sixteenth of w x h: 1 inside any of boxes (0..1 of the frame), fading out at the edges."""
    sw, sh = max(1, w // _SMALL), max(1, h // _SMALL)
    mask = np.zeros((sh, sw), np.float32)
    for x0, y0, x1, y1 in boxes:
        c0, c1 = max(int(x0 * sw), 0), min(int(np.ceil(x1 * sw)), sw)
        r0, r1 = max(int(y0 * sh), 0), min(int(np.ceil(y1 * sh)), sh)
        if c1 > c0 and r1 > r0:
            mask[r0:r1, c0:c1] = 1.0
    return cv2.GaussianBlur(mask, (0, 0), max(_EDGE * sw, 0.5))


def compose(frame: np.ndarray, weight: np.ndarray) -> np.ndarray:
    """frame with its blurred copy laid over it by weight (0..1 at any size): uint8 multiplies and an add."""
    if float(weight.max()) < 0.01:
        return frame
    h, w = frame.shape[:2]
    weight = cv2.resize(np.clip(weight * 255.0, 0, 255).astype(np.uint8), (w, h), interpolation=cv2.INTER_LINEAR)
    weight = cv2.merge((weight, weight, weight))
    over = cv2.multiply(blurred(frame), weight, scale=1 / 255)
    return cv2.add(cv2.multiply(frame, cv2.bitwise_not(weight), scale=1 / 255), over)


class PrivacyBlurPlugin(TelescopePlugin):
    name = "privacy_blur"
    title = N_("Privacy blur")
    about = N_("Blurs the other people, everything but you, or puts the wait screen up when someone else walks in.")
    when_off = N_("Without it there's no Privacy blur card and nothing gets hidden when someone walks in.")
    optional = True
    in_presets = True
    panel_region = "right"

    def setup(self, host, bus):
        self._host, self._bus = host, bus
        self._trigger = Trigger()
        self._blur = _Blur()
        self._auto = False
        self._tracks: list = []
        self._you_id: Optional[int] = None  # who "you" were when blurring started
        self._others: tuple = ()  # where the others were last seen, kept while blurring (they may still be there)
        self._covered = False
        self._asked_cover = False
        bus.people_tracked.connect(self._on_tracked)
        bus.stream_covered.connect(self._on_covered)

    # ── Card ──────────────────────────────────────────────────────────────

    def create_panel(self) -> QWidget:
        card = create_card()
        lay = card_layout(card)
        self._switch = Switch("")
        self._switch.setSizePolicy(QSizePolicy.Policy.Fixed, QSizePolicy.Policy.Fixed)
        self._switch.setToolTip(tr("Blur automatically when a second person shows up"))
        self._switch.toggled.connect(self._on_switch)
        add_card_header(lay, tr("Privacy blur"), "eye_off", action=self._switch, hue="vision")
        dim_until_paired(card, self._bus)
        self._mode_combo = NoScrollComboBox()
        self._mode_combo.setSizeAdjustPolicy(QComboBox.SizeAdjustPolicy.AdjustToMinimumContentsLengthWithIcon)
        self._mode_combo.setMinimumContentsLength(12)  # the card stays narrow; the list opens as wide as it needs
        for key, text in MODES:
            self._mode_combo.addItem(tr(text), key)
            self._mode_combo.setItemData(self._mode_combo.count() - 1, text, SHORTCUT_VALUE_ROLE)
        self._mode_combo.setToolTip(tr("What happens while someone else is in the picture"))
        self._mode_combo.currentIndexChanged.connect(lambda _i: self._on_mode())
        lay.addWidget(self._mode_combo)
        self._now_btn = QPushButton(tr("Blur now"))
        self._now_btn.setCheckable(True)
        self._now_btn.setToolTip(tr("Blur by hand until you click again"))
        self._now_btn.toggled.connect(lambda _on: self._update())
        lay.addWidget(self._now_btn, 0, Qt.AlignmentFlag.AlignLeft)
        self._status = VisionStatus(self._host, self._bus)
        lay.addWidget(self._status)
        self._update()
        return card

    def create_actions(self) -> list:
        g = tr("Privacy blur")
        return [
            button_action("privacy_blur.now", tr("Blur now"), g, self._now_btn),
            button_action("privacy_blur.auto", tr("Privacy blur when someone walks in"), g, self._switch),
            choice_action("privacy_blur.mode", tr("Privacy blur: what happens"), g, combo=self._mode_combo),
        ]

    def _mode(self) -> str:
        return self._mode_combo.currentData() or "others"

    def _set_mode(self, mode: str):
        self._mode_combo.setCurrentIndex(max(self._mode_combo.findData(mode), 0))

    def _on_mode(self):
        self._host.schedule_save()
        self._update()

    # ── On and off (Qt thread) ────────────────────────────────────────────

    def _on_switch(self, on: bool):
        self._host.schedule_save()
        self._apply(on)

    def _apply(self, on: bool):
        if on != self._auto:
            self._auto = on
            self._trigger.reset()
            self._host.want_people(self.title, on)
        self._update()

    def _on_tracked(self, source_id: str, tracks: list, count: int):
        if source_id != (self._host.focused_source_id() or ""):
            return
        self._tracks = tracks
        if self._auto:
            self._trigger.update(count, time.monotonic())
        self._update()

    def _on_covered(self, covered: bool):
        self._covered = covered
        self._update()

    def on_focus_gained(self, stream_url: str, ctrl):
        self._tracks, self._you_id, self._others = [], None, ()
        self._trigger.reset()
        self._update()

    def on_stream_stop(self):
        self.on_focus_gained("", None)

    def _you(self) -> Optional[people.Track]:
        """You: whoever was there longest when blurring started, while they're still followed; else whoever is now."""
        held = next((t for t in self._tracks if t.id == self._you_id), None)
        return held or people.main(self._tracks)

    def _update(self):
        """Work out what frames get now, from the switches and the latest people."""
        if not hasattr(self, "_now_btn"):
            return
        active = self._now_btn.isChecked() or (self._auto and self._trigger.on)
        if not active:
            self._you_id, self._others = None, ()
        you = self._you() if active else None
        if active and you is not None and self._you_id is None:
            self._you_id = you.id
        mode = self._mode()
        want_cover = active and mode == "wait"
        if want_cover != self._asked_cover:
            self._asked_cover = want_cover
            self._bus.cover_requested.emit(self.title, want_cover)  # Wait screen answers through stream_covered
        others = tuple(t.box for t in self._tracks if you is None or t.id != you.id)
        if active and others:
            self._others = others
        self._blur = _Blur(mode, active, you.box if you is not None else None, self._others,
                           self._covered and want_cover)
        problem = ""
        if want_cover and not self._covered:
            problem = tr("With Wait screen switched off in Plugins the whole picture gets blurred instead.")
        self._status.refresh(self._auto or bool(problem), problem)

    def shutdown(self):
        if self._asked_cover:
            self._bus.cover_requested.emit(self.title, False)
        if self._auto:
            self._host.want_people(self.title, False)

    # ── Frames (worker thread) ────────────────────────────────────────────

    def process_frame(self, frame: np.ndarray) -> np.ndarray:
        return self._step(frame, self._blur)

    def frame_step(self):
        blur = self._blur
        if not blur.active:
            return None
        return lambda frame: self._step(frame, blur)  # a stream left blurred stays blurred behind the panels

    def _step(self, frame: np.ndarray, blur: _Blur) -> np.ndarray:
        if not blur.active or blur.covered:
            return frame
        h, w = frame.shape[:2]
        view = self._host.view()
        if blur.you is None or blur.mode == "wait":
            return blurred(frame)  # nobody to go by (by hand without the vision download), or no wait screen to show
        you = boxes_mask([shown_box(grown(blur.you), view)], w, h)
        mask = self._host.person_mask(frame)
        people = None
        if mask is not None:
            people = cv2.resize(mask, (you.shape[1], you.shape[0]), interpolation=cv2.INTER_AREA).astype(np.float32)
            people /= 255.0
            you = people * np.minimum(you * 2.0, 1.0)  # you: the person mask inside your box
        if blur.mode == "others" and blur.others:
            weight = boxes_mask([shown_box(grown(b), view) for b in blur.others], w, h)
            if people is not None:
                weight = np.maximum(weight, people)  # anyone the mask finds who isn't you, the whole of them
            return compose(frame, weight * (1.0 - you))
        return compose(frame, 1.0 - you)  # all but you (also others with nobody else found yet, from Blur now)

    # ── Config ────────────────────────────────────────────────────────────

    def get_config(self) -> dict:
        return {"on": self._switch.isChecked(), "mode": self._mode()}

    def set_config(self, cfg: dict):
        mode = cfg.get("mode") if cfg.get("mode") in dict(MODES) else "others"
        self._mode_combo.blockSignals(True)
        self._set_mode(mode)
        self._mode_combo.blockSignals(False)
        self._switch.blockSignals(True)
        self._switch.setChecked(cfg.get("on") is True)
        self._switch.blockSignals(False)
        self._apply(self._switch.isChecked())

    def diagnostics(self) -> dict:
        state = "blurring" if self._blur.active else "on" if self._auto else "off"
        return {"Privacy blur": f"{state}, {self._mode()}"}
