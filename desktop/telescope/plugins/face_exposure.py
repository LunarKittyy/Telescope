"""Expose for my face: moves the phone's metering point (the one Tap a point uses) to your face, calmly."""
import math
import time
from typing import Optional

from PyQt6.QtCore import Qt
from PyQt6.QtWidgets import QSizePolicy, QWidget

from telescope import people
from telescope.plugin import TelescopePlugin
from telescope.shortcuts import button_action
from telescope.view import shown_to_view, view_to_shown
from telescope.widgets.common import (
    Switch, action_button, add_card_header, card_layout, create_card, dim_until_paired, wrapped_note,
)
from telescope.widgets.vision_status import VisionStatus
from telescope.i18n import N_, tr

DEAD_ZONE = 0.08  # the face moving less than this (of the picture) from the point leaves the point where it is
STILL = 0.04  # the face counts as settled while it stays within this of where it stopped
SETTLE_S = 0.8  # and has to stay settled this long before the point moves (not while you walk in or out)
SMOOTH_S = 0.5  # while settled, the face's position is smoothed with this time constant
EVERY_S = 1.5  # the point moves at most this often: each move has the phone meter and focus again


class FaceMeter:
    """Where the metering point goes as the face moves: only once the face has settled somewhere new, not for small
    moves, and nowhere while there's no face (the phone keeps metering where it last was)."""

    def __init__(self):
        self.point: Optional[tuple] = None  # where it was last moved to
        self._smooth: Optional[tuple] = None
        self._at: Optional[float] = None
        self._stopped: Optional[tuple] = None  # where the face last stopped
        self._since: Optional[float] = None  # and since when it's been there
        self._moved_at = -math.inf

    def update(self, face: Optional[tuple], now: float) -> Optional[tuple]:
        """face: (x, y) of your face, or None while you're not there. Returns where to move the point now, or None to
        leave it."""
        if face is None:
            self._stopped = self._since = self._smooth = None
            return None
        if self._smooth is None or self._at is None:
            self._smooth = face
        else:
            a = 1.0 - math.exp(-max(now - self._at, 0.0) / SMOOTH_S)
            self._smooth = (self._smooth[0] + (face[0] - self._smooth[0]) * a,
                            self._smooth[1] + (face[1] - self._smooth[1]) * a)
        self._at = now
        if self._stopped is None or math.dist(face, self._stopped) > STILL:
            self._stopped, self._since, self._smooth = face, now, face  # moving: smoothing starts over where it stops
        if now - self._since < SETTLE_S or now - self._moved_at < EVERY_S:
            return None
        if self.point is not None and math.dist(self._smooth, self.point) <= DEAD_ZONE:
            return None
        self.point, self._moved_at = self._smooth, now
        return self.point

    def reset(self):
        self.__init__()


class FaceExposurePlugin(TelescopePlugin):
    name = "face_exposure"
    title = N_("Expose for my face")
    about = N_("Moves the phone's metering point to your face to keep you well lit whatever's behind you.")
    when_off = N_("Without it there's no Expose for my face card and the phone meters the point you tap or the "
                  "whole picture.")
    optional = True
    needs_vision = True
    in_presets = True
    panel_region = "right"

    def setup(self, host, bus):
        self._host, self._bus = host, bus
        self._meter = FaceMeter()
        self._on = False
        self._manual = False  # a point was picked by hand, which wins until the plugin is switched off and on again
        self._camera_manual = False  # the Camera card is on Manual focus, which a point would undo
        self._forced = False  # Follow my face was clicked, so a point goes out even then
        self._available = False  # the lens can meter on a point, and a stream is there to send it to
        self._moving = False  # the point being picked is ours
        bus.people_tracked.connect(self._on_tracked)
        bus.focus_point_picked.connect(self._on_point_picked)
        bus.focus_mode_picked.connect(self._on_mode_picked)
        bus.focus_mode_changed.connect(self._on_focus_mode)
        bus.focus_point_available.connect(self._on_available)

    # ── Card ──────────────────────────────────────────────────────────────

    def create_panel(self) -> QWidget:
        card = create_card()
        lay = card_layout(card)
        self._switch = Switch("")
        self._switch.setSizePolicy(QSizePolicy.Policy.Fixed, QSizePolicy.Policy.Fixed)
        self._switch.setToolTip(tr("Meter the exposure on your face"))
        self._switch.toggled.connect(self._on_switch)
        add_card_header(lay, tr("Expose for my face"), "sun", action=self._switch, hue="vision")
        dim_until_paired(card, self._bus, phone_only=True)
        self._manual_note = wrapped_note("")
        lay.addWidget(self._manual_note)
        self._follow_btn = action_button(tr("Follow my face"))
        self._follow_btn.setToolTip(tr("Go back to metering on your face"))
        self._follow_btn.clicked.connect(self.follow_again)
        lay.addWidget(self._follow_btn, 0, Qt.AlignmentFlag.AlignLeft)
        self._status = VisionStatus(self._host, self._bus)
        lay.addWidget(self._status)
        self._show()
        return card

    def create_actions(self) -> list:
        g = tr("Expose for my face")
        return [button_action("face_exposure.on", tr("Expose for my face"), g, self._switch)]

    # ── On and off (Qt thread) ────────────────────────────────────────────

    def _on_switch(self, on: bool):
        self._host.schedule_save()
        self._apply(on)

    def _apply(self, on: bool):
        if on != self._on:
            self._on = on
            self._manual = self._forced = False
            self._meter.reset()
            self._host.want_people(self.title, on)
        self._show()

    def follow_again(self):
        """Back to the face after a point was picked by hand."""
        self._manual, self._forced = False, True
        self._meter.reset()
        self._show()

    def _held(self) -> bool:
        """Something the user chose keeps the point where it is."""
        return self._manual or (self._camera_manual and not self._forced)

    def _show(self):
        if not hasattr(self, "_status"):
            return
        held = self._on and self._held()
        self._manual_note.setText(tr("The phone meters on the point you picked by hand.") if self._manual else
                                  tr("Focus is on Manual in the Camera card."))
        self._manual_note.setVisible(held)
        self._follow_btn.setVisible(held)
        problem = ""
        if self._on and not self._available and self._host.is_streaming():
            problem = tr("This lens can't meter on a point.")
        self._status.refresh(self._on, problem)

    def _on_available(self, available: bool):
        self._available = available
        if not available:
            self._meter.reset()  # the phone forgot the point (another lens, or the stream ended)
        self._show()

    def _on_point_picked(self, *_point):
        if self._on and not self._moving:
            self._manual, self._forced = True, False
            self._show()

    def _on_mode_picked(self, mode: str):
        if mode != "manual":
            self._on_point_picked()
        elif self._on and not self._moving:
            self._manual, self._camera_manual, self._forced = False, True, False  # held by Manual, not by a point
            self._show()

    def _on_focus_mode(self, mode: str):
        self._camera_manual = mode == "manual"
        self._show()

    def on_focus_gained(self, stream_url: str, ctrl):
        self._meter.reset()

    def on_stream_stop(self):
        self._meter.reset()

    # ── Following (Qt thread) ─────────────────────────────────────────────

    def _on_tracked(self, source_id: str, tracks: list, _count: int):
        if not self._on or self._held() or not self._available:
            return
        if source_id != (self._host.focused_source_id() or ""):
            return
        you = people.main(tracks)
        face = you.face if you is not None and you.missing_s == 0 else None
        point = self._meter.update(face, time.monotonic())
        shown = self._shown(point) if point is not None else None
        if point is not None and shown is None:
            self._meter.point = None  # outside the picture as shown: tried again once you're back in it
        elif shown is not None:
            self._moving = True
            try:
                self._bus.focus_point_picked.emit(*shown)
            finally:
                self._moving = False

    def _shown(self, face: tuple) -> Optional[tuple]:
        """face, from the picture at zoom 1, in the picture as shown now; None when it's outside it."""
        view = self._host.view()
        turn = (view.flip_h, view.flip_v, view.rotation)
        u, v = view_to_shown(*shown_to_view(*face, 1.0, 0.0, 0.0, *turn), view.zoom, view.pan_x, view.pan_y, *turn)
        return (u, v) if 0.0 <= u <= 1.0 and 0.0 <= v <= 1.0 else None

    def shutdown(self):
        if self._on:
            self._host.want_people(self.title, False)

    # ── Config ────────────────────────────────────────────────────────────

    def get_config(self) -> dict:
        return {"on": self._switch.isChecked()}

    def set_config(self, cfg: dict):
        self._switch.blockSignals(True)
        self._switch.setChecked(cfg.get("on") is True)
        self._switch.blockSignals(False)
        self._apply(self._switch.isChecked())

    def diagnostics(self) -> dict:
        if not self._on:
            return {"Expose for my face": "off"}
        return {"Expose for my face": "point picked by hand" if self._manual else
                "waiting (manual focus)" if self._held() else "following"}
