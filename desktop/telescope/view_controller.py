"""The window's view of the shown stream: it crops, flips and rotates frames and tells the phone its share of the zoom."""
import time
from dataclasses import asdict, replace
from typing import Optional

import numpy as np

from telescope.plugin import StreamsBehind, TelescopePlugin
from telescope.view import (
    PhoneZoom, PhoneZoomCaps, View, ViewStatus, apply_zoom, inverse_map, lens_boxes, split_zoom, transform_frame,
)

_MOVES = ("zoom", "pan_x", "pan_y")  # what a taken view moves; flip and rotation stay manual


class ViewController(TelescopePlugin):
    """Hears the stream hooks like a plugin that follows the panels (through FlowHost.plugins()), but it's the
    window's: it isn't in the plugin list, has no card and can't be switched off."""
    name = "view"

    def setup(self, host, bus):
        self._host = host
        self._bus = bus
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
            self._bus.view_taken.emit(owner)
        return True

    def steer_view(self, owner: str, zoom: float, pan_x: float, pan_y: float):
        if owner and owner == self._owner:
            if not self._engaged:
                self._engaged = True
                self._sync_zoom()
            self._move(replace(self.view, zoom=zoom, pan_x=pan_x, pan_y=pan_y), moved=False)

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
        frame = apply_zoom(frame, *self._desktop_crop)
        return transform_frame(frame, view.flip_h, view.flip_v, view.rotation)

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
        self._desktop_crop = split.desktop
        if split.phone is not None and split.phone != self._sent_zoom:
            if self._sent_zoom is None or split.phone.ratio != self._sent_zoom.ratio:
                self._sent_ratio_at = time.monotonic()
            self._sent_zoom = split.phone
            self._ctrl.send(action="zoom", **asdict(split.phone))
        self._bus.view_status.emit(ViewStatus(self._ctrl is not None, self._zoom_caps if self._ctrl else None,
                                              self._sent_zoom, self._sent_ratio_at, split.desktop[0]))

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
