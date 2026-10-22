"""The window's view of the shown stream: it crops, flips and rotates frames and tells the phone its share of the zoom."""
import time
from dataclasses import asdict, replace
from typing import Optional

import cv2
import numpy as np

from telescope.plugin import StreamsBehind, TelescopePlugin
from telescope.view import (
    PhoneZoom, PhoneZoomCaps, View, ViewStatus, apply_zoom, inverse_map, lens_boxes, pan_for, phone_window, rest_within,
    shown_to_view, split_zoom, transform_frame, view_to_shown, window_centre,
)

_MOVES = ("zoom", "pan_x", "pan_y")  # what a taken view moves; flip and rotation stay manual
_HOLD_SLACK = 1.4  # while steered, the phone is sent a share this much wider than the framing
_HOLD_DRIFT = 1.25  # and a new one once the share it would get is this far off its own
_HOLD_S = 2.0  # for this long


class ViewController(TelescopePlugin):
    """Hears the stream hooks like a plugin that follows the panels (through FlowHost.plugins()), but it's the
    window's: it isn't in the plugin list, has no card and can't be switched off."""
    name = "view"

    def setup(self, host, bus, vision=None):
        self._host = host
        self._bus = bus
        self._vision = vision  # the raw frames go to it while something wants people (Auto framing)
        self.view = View()  # Written by Qt thread; read by worker thread (GIL ensures atomicity).
        self._manual = View()  # what set_view asked for; differs from view in zoom and pan while it's taken
        self._owner = ""  # what took the view ("" = nobody)
        self._engaged = False  # something set a view: until then the phone is never sent a zoom
        self._frame_size = (0, 0)  # the phone's frame, for mapping preview clicks back onto it
        self._desktop_crop = (1.0, 0.0, 0.0)  # the zoom/pan share left for this computer (split_zoom)
        self._ctrl = None
        self._zoom_caps: Optional[PhoneZoomCaps] = None
        self._sent_zoom: Optional[PhoneZoom] = None
        self._sent_ratio_at = 0.0  # time.monotonic() when the phone was last sent a new ratio
        self._drift_since = None  # while steered: since when the phone's share has been clearly off (_held)
        self._behind = StreamsBehind()  # the phone side of each stream the panels moved away from
        bus.focus_point_picked.connect(self._on_point_picked)
        bus.camera_switched.connect(self._on_camera_caps)

    # ── What the window offers plugins (HostView) ─────────────────────────

    def set_view(self, **changes):
        self._manual = replace(self._manual, **changes)
        live = changes if not self._owner else {k: v for k, v in changes.items() if k not in _MOVES}
        if not self._engaged:
            self._engaged = True
            self._sync_zoom()
        self._move(replace(self.view, **live), moved=any(k in _MOVES for k in live))

    def take_view(self, owner: str) -> bool:
        if self._owner and self._owner != owner:
            return False
        if not self._owner:
            self._owner = owner
            self._drift_since = float("-inf")  # the phone's share is picked afresh on the first steer
            self._bus.view_taken.emit(owner)
        return True

    def steer_view(self, owner: str, zoom: float, pan_x: float, pan_y: float):
        if owner and owner == self._owner:
            engaging, before = not self._engaged, self.view
            self._engaged = True
            self._move(replace(self.view, zoom=zoom, pan_x=pan_x, pan_y=pan_y), moved=False)
            if engaging and self.view == before:
                self._sync_zoom()

    def release_view(self, owner: str):
        if not owner or owner != self._owner:
            return
        self._owner = ""
        self._bus.view_taken.emit("")
        self._move(replace(self.view, **{k: getattr(self._manual, k) for k in _MOVES}), moved=False)

    def _move(self, view: View, moved: bool):
        if view == self.view:
            return
        self.view = view
        self._sync_zoom()
        self._bus.view_changed.emit(view)
        self._bus.view_pannable.emit(view.zoom > 1.0)
        self._emit_lens_boxes(moved)

    # ── Frames (worker thread) ────────────────────────────────────────────

    def process_frame(self, frame: np.ndarray) -> np.ndarray:
        self._frame_size = (frame.shape[1], frame.shape[0])
        view = self.view
        if self._vision is not None and self._vision.wants_people():
            self._vision.feed_raw(frame, *self._upright(view))
        frame = apply_zoom(frame, *self._desktop_crop)
        return transform_frame(frame, view.flip_h, view.flip_v, view.rotation)

    def _upright(self, view: View) -> tuple:
        """(prepare, to_shown) for vision: the raw frame turned the way it's shown, and its points onto the
        picture as shown at zoom 1 (through the phone share it was sent with)."""
        sent = phone_window(self._sent_zoom if self._ctrl else None)
        flips = (view.flip_h, view.flip_v, view.rotation)

        def to_shown(u, v):
            return view_to_shown(*shown_to_view(u, v, *sent, *flips), 1.0, 0.0, 0.0, *flips)
        return (lambda frame: transform_frame(frame, *flips)), to_shown

    def aspect(self) -> float:
        """Width over height of the picture as shown, 0 before the first frame."""
        w, h = self._frame_size
        if not h:
            return 0.0
        return h / w if self.view.rotation in (cv2.ROTATE_90_CLOCKWISE, cv2.ROTATE_90_COUNTERCLOCKWISE) else w / h

    def frame_step(self):
        crop, view = self._desktop_crop, self.view
        return lambda frame: transform_frame(apply_zoom(frame, *crop), view.flip_h, view.flip_v, view.rotation)

    # ── The phone's share of the zoom ─────────────────────────────────────

    def on_stream_start(self, stream_url: str, ctrl):
        self._ctrl = ctrl
        self._sent_zoom = None  # a new connection (or a reconnect): tell the phone again
        self._sync_zoom()

    def on_focus_gained(self, stream_url: str, ctrl):
        kept = self._behind.take(self._host, ctrl)
        if kept is None:
            self.on_stream_start(stream_url, ctrl)
            return
        # Back on a stream: the phone already crops what it was sent, so the desktop only does the rest
        self._ctrl = ctrl
        self._zoom_caps, self._sent_zoom, self._sent_ratio_at = kept
        self._sync_zoom()

    def on_stream_stop(self):
        self._behind.drop(self._host)
        self._leave_stream()

    def on_focus_lost(self):
        self._behind.keep(self._host, self._ctrl, (self._zoom_caps, self._sent_zoom, self._sent_ratio_at))
        self._leave_stream()

    def _leave_stream(self):
        self._ctrl = None
        self._zoom_caps = None
        self._sync_zoom()
        self._emit_lens_boxes(False)

    def on_phone_state(self, state: dict):
        self._on_camera_caps(next((c for c in state.get("cameras", []) if c.get("current")), None))

    def _on_camera_caps(self, cam: Optional[dict]):
        caps = PhoneZoomCaps.from_camera(cam)
        if caps != self._zoom_caps:
            self._zoom_caps = caps
            self._sync_zoom()
            self._emit_lens_boxes(False)

    def _sync_zoom(self):
        """Split the framing between phone and desktop; the phone only hears about changes."""
        view = self.view
        current = self._sent_zoom.ratio if self._sent_zoom else 1.0
        caps = self._zoom_caps if self._ctrl and self._engaged else None
        split = split_zoom(view.zoom, view.pan_x, view.pan_y, caps, current)
        phone, desktop = split.phone, split.desktop
        if self._owner and phone is not None:
            phone, desktop = self._held(view, caps, current, split)
        self._desktop_crop = desktop
        if phone is not None and phone != self._sent_zoom:
            if self._sent_zoom is None or phone.ratio != self._sent_zoom.ratio:
                self._sent_ratio_at = time.monotonic()
            self._sent_zoom = phone
            self._ctrl.send(action="zoom", **asdict(phone))
        self._bus.view_status.emit(ViewStatus(self._ctrl is not None, self._zoom_caps if self._ctrl else None,
                                              self._sent_zoom, self._sent_ratio_at, desktop[0]))

    def _held(self, view: View, caps, current: float, split) -> tuple:
        """(phone share, desktop crop) while something steers the view: the phone keeps what it was sent (a moving
        crop would make it hop lenses and lag behind) until the framing leaves it, or wants a clearly different one
        for _HOLD_S."""
        held = self._sent_zoom
        rest = rest_within(view.zoom, view.pan_x, view.pan_y, held) if held else None
        wide = max(1.0, view.zoom / _HOLD_SLACK)  # room around the framing, so small moves stay on this computer
        centre = (window_centre(view.zoom, view.pan_x), window_centre(view.zoom, view.pan_y))
        want = split_zoom(wide, pan_for(centre[0], wide), pan_for(centre[1], wide), caps, current).phone
        off = (want.ratio * want.crop) / (held.ratio * held.crop) if held else 0.0
        drifted = not held or want.ratio != held.ratio or not 1 / _HOLD_DRIFT <= off <= _HOLD_DRIFT
        now = time.monotonic()
        if rest is not None:
            if not drifted:
                self._drift_since = None
                return held, rest
            if self._drift_since is None:
                self._drift_since = now
            if now - self._drift_since < _HOLD_S:
                return held, rest
        self._drift_since = None
        rest = rest_within(view.zoom, view.pan_x, view.pan_y, want)
        return (want, rest) if rest is not None else (split.phone, split.desktop)

    # ── The preview ───────────────────────────────────────────────────────

    def _on_point_picked(self, u: float, v: float):
        w, h = self._frame_size
        view = self.view
        self._bus.focus_point.emit(*inverse_map(u, v, w, h, *self._desktop_crop,
                                                view.flip_h, view.flip_v, view.rotation))

    def _emit_lens_boxes(self, moved: bool):
        caps, view = self._zoom_caps if self._ctrl else None, self.view
        self._bus.lens_boxes.emit(lens_boxes(caps, view.zoom, view.pan_x, view.pan_y, view.flip_h, view.flip_v,
                                             view.rotation), moved)
