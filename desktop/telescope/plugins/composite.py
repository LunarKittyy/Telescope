"""Several cameras in one picture: puts the running streams on one canvas, which goes out on the Telescope camera.

The picture is drawn in the frame steps of the stream going to the Telescope camera (slot 0, what call apps watch),
from that stream's frame and the newest frame of each other stream (host.stream_frames). While that stream stalls, a
clock of its own keeps drawing from its last frame (host.replace_frame), so the other streams don't freeze with it.
The other streams carry on to their own cameras as before.
"""

import logging
import threading
import time
from dataclasses import replace
from typing import Optional

from PyQt6.QtWidgets import QComboBox, QSizePolicy, QWidget

from telescope import vcam
from telescope.composite import (
    MAX_LAYOUTS, MAX_NAME, STALE_S, Compositor, Layout, clean_layout, clean_layouts, layout_dict, same_arrangement,
)
from telescope.plugin import TelescopePlugin
from telescope.shortcuts import button_action, choice_action
from telescope.widgets.common import (
    NoScrollComboBox, Switch, add_card_header, card_action, card_layout, control_row as _row, create_card,
    host_window, wrapped_note,
)
from telescope.i18n import N_, tr

FALLBACK_FPS = 30  # how often the picture is drawn while the stream it goes out on stalls


class CompositePlugin(TelescopePlugin):
    name = "composite"
    title = N_("Several cameras in one picture")
    about = N_("Puts the streams you run side by side or one inside another, out on one virtual camera.")
    category = "streaming"
    icon = "layout"
    when_off = N_("Without it there's no One picture card and each stream only goes to its own camera.")
    optional = True
    in_presets = True  # a preset can be a scene: "face big, desk small" for talking, side by side for a demo
    follows_focus = False  # it's about the Telescope camera's stream, whichever one the panels show
    panel_region = "left"

    def setup(self, host, bus):
        self._host, self._bus = host, bus
        self._on = False
        self._layout = Layout()
        self._layouts: list = []
        self._target: Optional[str] = None  # source id of the stream going to the Telescope camera, where it goes out
        self._camera_size: Optional[tuple] = None  # what that camera opened at, for Auto canvas size
        self._compositor = Compositor()
        self._main_frame: Optional[tuple] = None  # (frame, time) the target stream had before the picture
        self._editor = None
        self._last_picture_at = 0.0
        self._fallback_key = None  # the frames the clock last drew from, so it doesn't draw the same picture again
        self._clock: Optional[threading.Thread] = None
        self._clock_wake = threading.Event()
        self._shut = False
        bus.streams_changed.connect(lambda _n: self._follow_camera())
        bus.camera_opened.connect(self._on_camera_opened)
        self._prepare()

    # ── The picture (each stream's thread) ────────────────────────────────

    def process_frame(self, frame):
        return self._step(frame)

    def frame_step(self):
        return self._step  # the settings are the app's, not the stream's, so it stays live behind the panels too

    def _step(self, frame):
        target = self._target
        if not self._on or target is None or self._host.frame_source() != target:
            return frame
        now = time.monotonic()
        self._main_frame = (frame, now)
        frames = self.frames(frame, now)
        if self._compositor.live_tiles(frames) < 2:
            return frame
        out = self._compositor.render(frames)
        if out is None:
            return frame
        self._last_picture_at = now
        self._fallback_key = None
        return out

    # ── While the target stream stalls (the clock's thread) ───────────────

    def _run_clock(self):
        while not self._shut:
            if not self._on or self._target is None:  # rests until there's a stream on the camera to keep moving
                self._clock_wake.wait()
                self._clock_wake.clear()
                continue
            time.sleep(1 / FALLBACK_FPS)
            try:
                self.draw_stalled()
            except Exception:
                logging.exception("Drawing the picture while its stream stalls failed")

    def draw_stalled(self, now: Optional[float] = None) -> bool:
        """Draw the picture from the target stream's last frame when that's more than two frames old, if any other
        stream moved since; the target's frame steps draw it otherwise. True if a picture went out."""
        target, main = self._target, self._main_frame
        now = time.monotonic() if now is None else now
        if not self._on or target is None or main is None or now - main[1] < 2 / FALLBACK_FPS:
            return False
        current = self._host.stream_frame(target)
        if current is None:  # its camera is off, or it hasn't sent a frame yet
            return False
        frames = self.frames(None, now)
        key = frozenset((sid, number) for sid, (_frame, number) in frames.items())
        others = {sid: src for sid, src in frames.items() if sid != target}
        if key == self._fallback_key or not self._compositor.live_tiles(others):  # nothing else to keep moving
            return False
        out = self._compositor.render(frames)
        # Not if the stream sent a frame in the meantime: that one has the newer picture
        if out is None or not self._host.replace_frame(target, out, current.number):
            return False
        self._fallback_key = key
        self._last_picture_at = now
        return True

    def frames(self, main=None, now: Optional[float] = None) -> dict:
        """{source id: (frame, number)} of every stream with a fresh frame; main is the target stream's own."""
        now = time.monotonic() if now is None else now
        if main is None and self._main_frame is not None and now - self._main_frame[1] < STALE_S:
            main = self._main_frame[0]
        out = {}
        for sf in self._host.stream_frames():
            if sf.source_id == self._target:
                if main is not None:
                    out[sf.source_id] = (main, None)
                elif not self._on:  # with the picture on, what the main stream sends out is the picture itself
                    out[sf.source_id] = (sf.frame, sf.number)
            elif sf.age(now) < STALE_S:
                out[sf.source_id] = (sf.frame, sf.number)
        if main is not None and self._target is not None:
            out.setdefault(self._target, (main, None))
        return out

    def streams(self) -> list:
        """[(source id, name, aspect)] of the running streams that have a picture, in the order they started."""
        names = {sf.source_id: sf.name for sf in self._host.stream_frames()}
        return [(sid, names.get(sid, sid), frame.shape[1] / max(1, frame.shape[0]))
                for sid, (frame, _number) in self.frames().items()]

    def size(self) -> tuple:
        """The picture's size: the canvas size Setup asks for, else what the main camera opened at, else 1920x1080."""
        w, h = self._host.canvas_size()
        if w and h:
            return int(w), int(h)
        return self._camera_size or vcam.DEFAULT_SIZE

    def _prepare(self):
        self._compositor.prepare(self._layout, self.size())

    def _on_camera_opened(self, _source_id: str, slot: int, _w: int, _h: int):
        if slot == 0:
            self._follow_camera()

    # ── The stream going to the Telescope camera (GUI thread) ─────────────

    def _follow_camera(self):
        on_camera = self._host.stream_on_camera(0)
        target = on_camera[0] if on_camera is not None else None
        size = tuple(on_camera[1:]) if on_camera is not None and on_camera[1] and on_camera[2] else None
        if target != self._target:
            self._target = target
            self._main_frame = None  # another stream's frame isn't this one's
            self._fallback_key = None
            if target is not None:
                self._wake_clock()
        if size is not None and size != self._camera_size:
            self._camera_size = size
            self._prepare()
        self._show()

    def on_stream_start(self, stream_url: str, ctrl):
        self._follow_camera()

    def on_focus_gained(self, stream_url: str, ctrl):
        self._follow_camera()

    def on_stream_stop(self):
        self._follow_camera()

    def on_focus_lost(self):
        self._follow_camera()

    # ── Layouts (GUI thread) ──────────────────────────────────────────────

    def layout(self) -> Layout:
        return self._layout

    def set_layout(self, layout: Layout):
        """The picture as the editor has it now; the name says which saved layout it started from."""
        self._layout = layout
        self._prepare()
        self._show()
        self._host.schedule_save()

    def layouts(self) -> list:
        return list(self._layouts)

    def names(self) -> list:
        return [layout.name for layout in self._layouts]

    def pick(self, name: str) -> bool:
        layout = next((layout for layout in self._layouts if layout.name == name), None)
        if layout is None:
            return False
        self.set_layout(layout)
        return True

    def save_layout(self, name: str) -> str:
        """Save the picture as it is under name, replacing one of the same name; the name it got, "" for none."""
        name = "".join(c for c in name if c.isprintable() and c not in "<>")[:MAX_NAME].strip()
        if not name:
            return ""
        layout = replace(self._layout, name=name)
        at = next((i for i, saved in enumerate(self._layouts) if saved.name == name), None)
        if at is not None:
            self._layouts[at] = layout
        elif len(self._layouts) < MAX_LAYOUTS:
            self._layouts.append(layout)
        else:
            return ""
        self.set_layout(layout)
        return name

    def rename_layout(self, old: str, new: str):
        new = "".join(c for c in new if c.isprintable() and c not in "<>")[:MAX_NAME].strip()
        if not new or new == old:
            return
        self._layouts = [saved for saved in self._layouts if saved.name != new]
        self._layouts = [replace(saved, name=new) if saved.name == old else saved for saved in self._layouts]
        if self._layout.name == old:
            self._layout = replace(self._layout, name=new)
        self._show()
        self._host.schedule_save()

    def delete_layout(self, name: str):
        self._layouts = [saved for saved in self._layouts if saved.name != name]
        self._show()
        self._host.schedule_save()

    def saved_match(self) -> str:
        """The name of the saved layout the picture is now, or "" when it's been changed since."""
        return next((saved.name for saved in self._layouts if saved.name == self._layout.name
                     and same_arrangement(saved, self._layout)), "")

    # ── Card ──────────────────────────────────────────────────────────────

    def create_panel(self) -> QWidget:
        card = create_card()
        lay = card_layout(card)
        self._switch = Switch("")
        self._switch.setSizePolicy(QSizePolicy.Policy.Fixed, QSizePolicy.Policy.Fixed)
        self._switch.setToolTip(tr("Send the streams out as one picture"))
        self._switch.toggled.connect(self._on_switch)
        add_card_header(lay, tr("One picture"), "layout", action=self._switch, hue="stream")
        self._combo = NoScrollComboBox()
        self._combo.setSizeAdjustPolicy(QComboBox.SizeAdjustPolicy.AdjustToMinimumContentsLengthWithIcon)
        self._combo.setMinimumContentsLength(12)  # a long layout name doesn't widen the card; the list opens wider
        self._combo.setPlaceholderText(tr("Not saved"))
        self._combo.setToolTip(tr("A layout you saved in the editor"))
        self._combo.activated.connect(lambda i: self.pick(self._combo.itemText(i)))
        lay.addLayout(_row(tr("Layout"), self._combo, stretch=True))
        self._edit_btn = card_action(tr("Edit picture…"), "pencil")
        self._edit_btn.clicked.connect(self.open_editor)
        lay.addWidget(self._edit_btn)
        self._note = wrapped_note("")
        lay.addWidget(self._note)
        self._show()
        return card

    def create_actions(self) -> list:
        g = tr("One picture")
        return [
            button_action("composite.on", tr("One picture"), g, self._switch),
            choice_action("composite.layout", tr("Picture layout"), g, combo=self._combo),
        ]

    def _on_switch(self, on: bool):
        self._on = on
        self._wake_clock()
        self._show()
        self._host.schedule_save()

    def _wake_clock(self):
        if not self._on or self._shut:
            return
        if self._clock is None:
            self._clock = threading.Thread(target=self._run_clock, daemon=True, name="composite-clock")
            self._clock.start()
        self._clock_wake.set()

    def _show(self):
        if not hasattr(self, "_combo"):
            return
        names = self.names()
        if [self._combo.itemText(i) for i in range(self._combo.count())] != names:
            self._combo.blockSignals(True)
            self._combo.clear()
            self._combo.addItems(names)
            self._combo.blockSignals(False)
        match = self.saved_match()
        self._combo.setCurrentIndex(names.index(match) if match else -1)
        self._combo.setEnabled(bool(names))
        live = sum(1 for t in self._layout.tiles if self._host.is_streaming_from(t.source_id))
        if not self._on:
            self._note.hide()
            return
        camera = vcam.slot_label(0)
        if not self._layout.tiles:
            text = tr("Nothing's in the picture yet. Put streams in it with Edit picture.")
        elif live < 2:
            text = tr("It takes over once two of its streams are running.")
        elif self._target is None:
            text = tr("It goes out on {camera}, and no stream goes there now.", camera=camera)
        elif self._host.stream_frame(self._target) is None:
            text = tr("It goes out on {camera} once the stream there sends a picture.", camera=camera)
        else:
            text = tr("Goes out on {camera}.", camera=camera)
        self._note.setText(text)
        self._note.show()

    def open_editor(self):
        from telescope.widgets.composite_editor import CompositeEditor
        if self._editor is None:
            self._editor = CompositeEditor(self, host_window(self._host))
            self._editor.finished.connect(self._editor_closed)
        self._editor.show()
        self._editor.raise_()
        self._editor.activateWindow()

    def _editor_closed(self, _result: int):
        editor, self._editor = self._editor, None
        if editor is not None:
            editor.deleteLater()

    # ── Config ────────────────────────────────────────────────────────────

    def get_config(self) -> dict:
        return {"on": self._on, "layout": layout_dict(self._layout),
                "layouts": [layout_dict(saved) for saved in self._layouts]}

    def set_config(self, cfg: dict):
        self._layouts = clean_layouts(cfg.get("layouts"))
        self._apply(cfg)

    def apply_preset(self, cfg: dict):
        """A preset brings back whether the picture is on and how it looks, and leaves the saved layouts alone."""
        self._apply(cfg)

    def _apply(self, cfg: dict):
        self._layout = clean_layout(cfg.get("layout")) or Layout()
        self._on = cfg.get("on") is True
        self._wake_clock()
        self._prepare()
        if hasattr(self, "_switch"):
            self._switch.blockSignals(True)
            self._switch.setChecked(self._on)
            self._switch.blockSignals(False)
        self._show()
        if self._editor is not None:
            self._editor.reload()

    def diagnostics(self) -> dict:
        if not self._on:
            return {"One picture": "off"}
        live = time.monotonic() - self._last_picture_at < 1.0
        return {"One picture": f"on, {len(self._layout.tiles)} streams, {'drawing' if live else 'waiting'}"}

    def shutdown(self):
        self._shut = True
        self._clock_wake.set()
        if self._editor is not None:
            self._editor.close()
