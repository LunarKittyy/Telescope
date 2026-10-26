"""Notes next to the lens: your notes on the phone's screen right under its front camera, to read while looking at
the people on the call. The phone side is NotesActivity.kt; docs/protocol.md has the notes controls."""
import json
import threading
import time
from typing import Optional

from PyQt6.QtCore import QObject, Qt, QTimer, pyqtSignal
from PyQt6.QtWidgets import QButtonGroup, QHBoxLayout, QPlainTextEdit, QPushButton, QWidget

from telescope.plugin import TelescopePlugin
from telescope.shortcuts import button_action, slider_action
from telescope.widgets.common import (
    NoScrollComboBox, NoScrollSlider, SegmentButton, add_card_header, button_row, card_layout,
    control_row as _row, control_row_widget, create_card, dim_until_paired, icon_button, segmented_row, set_status_kind,
    WrapLabel, set_ui_role, slider_row, value_label,
)
from telescope.i18n import N_, tr

SIZE_MIN, SIZE_MAX, SIZE_DEFAULT = 16, 120, 44  # sp on the phone; 44 is about 4 words a line in portrait
SPEED_MAX, SPEED_DEFAULT = 10, 3
MAX_SETS = 9
NOTES_MAX_BYTES = 60_000  # the phone reads control requests up to 64 KiB
NAME_CHARS = 32
_POLL_MS = 3000  # how often to ask the phone whether the notes are on its screen, while they're sent
_RESEND_AFTER_S = 8.0  # the phone lost notes sent longer ago than this (its stream started again): send them again

SCROLLS = (("auto", N_("Auto")), ("hand", N_("By hand")))


def body_size(text: str) -> int:
    """How big the notes control is on the wire, as the phone client sends it."""
    return len(json.dumps({"action": "notes", "text": text}, ensure_ascii=False).encode("utf-8"))


def set_name(text: str) -> str:
    """What the set picker calls a set of notes: its first line with something on it, or "" for an empty one."""
    line = next((ln.strip() for ln in text.splitlines() if ln.strip()), "")
    return line if len(line) <= NAME_CHARS else line[:NAME_CHARS - 1].rstrip() + "…"


def front_facing(state: dict) -> Optional[bool]:
    """Whether the phone streams from its front camera; None when the state doesn't say (an older phone)."""
    current = next((c for c in state.get("cameras") or () if isinstance(c, dict) and c.get("current")), None)
    if current is None or not current.get("facing"):
        return None
    return current.get("facing") == "front"


def notes_status(*, streaming: bool, phone: bool, takes_notes: Optional[bool], too_long: bool, sent: bool,
                 shown: bool, front: Optional[bool], edited: bool, closed: bool = False,
                 preview: bool = False) -> tuple:
    """(what the card says about the notes, its status kind)."""
    if not phone:
        return tr("Notes only work with a phone as the camera."), "dim"
    if takes_notes is False:
        return tr("Update Telescope on the phone to show notes there."), "warn"
    if too_long:
        return tr("These notes are too long to send to the phone."), "err"
    if not streaming:
        return tr("Start streaming to send the notes to the phone."), "dim"
    if not sent:
        if front is False:
            return tr("Switch to the front camera to have the phone's screen facing you."), "warn"
        return tr("Send to phone puts them on its screen right under the camera."), "dim"
    if not shown:
        if closed:
            return tr("The notes were closed on the phone. Send again to show them."), "warn"
        if preview:
            return tr("The notes show up on the phone once you close its preview."), "warn"
        return tr("Open Telescope on the phone to see the notes."), "warn"
    if front is False:
        return tr("The notes are up but the phone streams from its back camera."), "warn"
    if edited:
        return tr("Send again to show your changes on the phone."), "ok"
    return tr("The notes are on the phone's screen."), "ok"


class _Signals(QObject):
    state_ready = pyqtSignal(object, dict)  # (control client, its phone's state)


class NotesPlugin(TelescopePlugin):
    name = "notes"
    title = N_("Notes")
    about = N_("Shows your notes on the phone's screen right under the camera, to read while looking people in the eye.")
    when_off = N_("Without it there's no Notes card and the phone never shows notes.")
    optional = True
    panel_region = "right"

    def setup(self, host, bus):
        self._host, self._bus = host, bus
        self._ctrl = None  # the shown stream's control client
        self._sent_ctrl = None  # where the notes went, while the phone has them
        self._sent_text = ""
        self._sent_at = 0.0
        self._takes_notes: Optional[bool] = None  # None: not known yet
        self._shown = False
        self._closed = False  # closed on the phone (Back or its close button) since they were sent
        self._preview = False  # the phone holds them back while its Preview is open
        self._front: Optional[bool] = None
        self._source = ""  # "" for a phone, else the picked StreamSource
        self._sets = [""]
        self._current = 0
        self._sig = _Signals()
        self._sig.state_ready.connect(self._on_polled)
        self._timer = QTimer()
        self._timer.setInterval(_POLL_MS)
        self._timer.timeout.connect(self._poll)
        bus.source_selected.connect(self._on_source)
        bus.camera_switched.connect(self._on_lens)

    # ── Card ──────────────────────────────────────────────────────────────

    def create_panel(self) -> QWidget:
        card = create_card()
        lay = card_layout(card)
        add_card_header(lay, tr("Notes"), "notes", hue="phone")
        dim_until_paired(card, self._bus)

        self._set_combo = NoScrollComboBox()
        self._set_combo.setToolTip(tr("Your saved notes"))
        self._set_combo.currentIndexChanged.connect(self._pick_set)
        self._new_btn = icon_button("plus", tr("New notes"))
        self._new_btn.clicked.connect(self._new_set)
        self._delete_btn = icon_button("trash", tr("Delete these notes"))
        self._delete_btn.clicked.connect(self._delete_set)
        sets = QHBoxLayout()
        sets.setContentsMargins(0, 0, 0, 0)
        sets.setSpacing(6)
        sets.addWidget(self._set_combo, 1)
        sets.addWidget(self._new_btn)
        sets.addWidget(self._delete_btn)
        lay.addLayout(sets)

        self._editor = QPlainTextEdit()
        self._editor.setPlaceholderText(tr("Type or paste your notes. Next and Previous move one line at a time."))
        self._editor.setAccessibleName(tr("Notes"))
        self._editor.setMinimumHeight(140)
        self._editor.textChanged.connect(self._on_edit)
        lay.addWidget(self._editor)

        self._size_slider = NoScrollSlider(Qt.Orientation.Horizontal)
        self._size_slider.setRange(SIZE_MIN, SIZE_MAX)
        self._size_slider.setValue(SIZE_DEFAULT)
        self._size_slider.set_default(SIZE_DEFAULT)
        self._size_slider.setToolTip(tr("How big the notes are on the phone"))
        self._size_lbl = value_label(str(SIZE_DEFAULT))
        self._size_slider.valueChanged.connect(self._on_size)
        lay.addLayout(_row(tr("Text size"), slider_row(self._size_slider, self._size_lbl), stretch=True))

        self._scroll_btns = {}
        group = QButtonGroup(card)
        for key, text in SCROLLS:
            btn = SegmentButton(tr(text), value=key)
            btn.clicked.connect(lambda _c=False, k=key: self._set_scroll(k))
            group.addButton(btn)
            self._scroll_btns[key] = btn
        self._scroll_btns["auto"].setChecked(True)
        lay.addLayout(_row(tr("Scroll"), segmented_row(*self._scroll_btns.values()), stretch=True))

        self._speed_slider = NoScrollSlider(Qt.Orientation.Horizontal)
        self._speed_slider.setRange(1, SPEED_MAX)
        self._speed_slider.setValue(SPEED_DEFAULT)
        self._speed_slider.set_default(SPEED_DEFAULT)
        self._speed_lbl = value_label(str(SPEED_DEFAULT))
        self._speed_slider.valueChanged.connect(self._on_speed)
        self._speed_row = control_row_widget(tr("Speed"), slider_row(self._speed_slider, self._speed_lbl), stretch=True)
        lay.addWidget(self._speed_row)

        self._back_btn = QPushButton(tr("Previous", ctx="notes"))
        self._back_btn.setToolTip(tr("Back to the line before"))
        self._back_btn.clicked.connect(lambda: self._move(-1))
        self._play_btn = QPushButton(tr("Play"))
        self._play_btn.setCheckable(True)
        self._play_btn.setToolTip(tr("Start or pause scrolling on the phone"))
        self._play_btn.toggled.connect(self._on_play)
        self._next_btn = QPushButton(tr("Next", ctx="notes"))
        self._next_btn.setToolTip(tr("On to the next line"))
        self._next_btn.clicked.connect(lambda: self._move(1))
        lay.addLayout(button_row(self._back_btn, self._play_btn, self._next_btn))

        self._send_btn = QPushButton(tr("Send to phone"))
        set_ui_role(self._send_btn, "primary")
        self._send_btn.clicked.connect(self._send)
        self._clear_btn = QPushButton(tr("Clear from phone"))
        self._clear_btn.clicked.connect(self._clear)
        lay.addLayout(button_row(self._send_btn, self._clear_btn))

        self._status = WrapLabel()
        self._status.setObjectName("setting_sub")
        lay.addWidget(self._status)

        self._fill_sets()
        self._show()
        return card

    def create_actions(self) -> list:
        g = tr("Notes")
        return [
            button_action("notes.send", tr("Send notes"), g, self._send_btn),
            button_action("notes.clear", tr("Clear notes"), g, self._clear_btn),
            button_action("notes.next", tr("Next line"), g, self._next_btn),
            button_action("notes.previous", tr("Previous line"), g, self._back_btn),
            button_action("notes.play", tr("Scroll notes"), g, self._play_btn,
                          states=(tr("playing"), tr("paused"))),
            slider_action("notes.speed", tr("Notes scroll speed"), g, self._speed_slider, step=1.0,
                          readout=self._speed_lbl),
        ]

    # ── Saved sets ────────────────────────────────────────────────────────

    def _fill_sets(self):
        self._set_combo.blockSignals(True)
        self._set_combo.clear()
        for i, text in enumerate(self._sets):
            self._set_combo.addItem(self._set_label(i, text))
        self._set_combo.setCurrentIndex(self._current)
        self._set_combo.blockSignals(False)
        self._editor.blockSignals(True)
        self._editor.setPlainText(self._sets[self._current])
        self._editor.blockSignals(False)
        self._new_btn.setEnabled(len(self._sets) < MAX_SETS)
        self._delete_btn.setEnabled(len(self._sets) > 1 or bool(self._sets[0]))

    @staticmethod
    def _set_label(index: int, text: str) -> str:
        return set_name(text) or tr("Notes {n}", n=index + 1)

    def _pick_set(self, index: int):
        if 0 <= index < len(self._sets) and index != self._current:
            self._current = index
            self._fill_sets()
            self._show()
            self._host.schedule_save()

    def _new_set(self):
        if len(self._sets) >= MAX_SETS:
            return
        self._sets.append("")
        self._current = len(self._sets) - 1
        self._fill_sets()
        self._editor.setFocus()
        self._show()
        self._host.schedule_save()

    def _delete_set(self):
        del self._sets[self._current]
        if not self._sets:
            self._sets = [""]
        self._current = min(self._current, len(self._sets) - 1)
        self._fill_sets()
        self._show()
        self._host.schedule_save()

    def _on_edit(self):
        text = self._editor.toPlainText()
        self._sets[self._current] = text
        self._set_combo.setItemText(self._current, self._set_label(self._current, text))
        self._delete_btn.setEnabled(len(self._sets) > 1 or bool(text))
        self._show()
        self._host.schedule_save()

    # ── Handlers (Qt thread) ──────────────────────────────────────────────

    def _scroll(self) -> str:
        return next((k for k, btn in self._scroll_btns.items() if btn.isChecked()), "auto")

    def _set_scroll(self, key: str):
        self._scroll_btns[key].setChecked(True)
        if key == "hand":
            self._play_btn.setChecked(False)
        self._send_style()
        self._show()
        self._host.schedule_save()

    def _on_size(self, value: int):
        self._size_lbl.setText(str(value))
        self._send_style()
        self._host.schedule_save()

    def _on_speed(self, value: int):
        self._speed_lbl.setText(str(value))
        self._send_style()
        self._host.schedule_save()

    def _on_play(self, playing: bool):
        self._play_btn.setText(tr("Pause") if playing else tr("Play"))
        self._send_style()

    def _style(self) -> dict:
        auto = self._scroll() == "auto"
        return {"action": "notes_style", "size": self._size_slider.value(),
                "speed": self._speed_slider.value() if auto else 0,
                "playing": 1 if auto and self._play_btn.isChecked() else 0}

    def _sendable(self) -> bool:
        return self._ctrl is not None and self._source == "" and self._takes_notes is not False

    def _send(self):
        text = self._editor.toPlainText()
        if not self._sendable() or body_size(text) > NOTES_MAX_BYTES:
            return
        if not text.strip():
            self._clear()
            return
        self._play_btn.setChecked(False)  # starts at the top, waiting for Play
        self._ctrl.send(**self._style())
        self._ctrl.send(action="notes", text=text)
        self._sent_ctrl, self._sent_text, self._sent_at = self._ctrl, text, time.monotonic()
        self._shown = True  # until the phone says otherwise; it opens them right away when Telescope is on screen
        self._closed = self._preview = False
        self._timer.start()
        QTimer.singleShot(1200, self._poll)
        self._show()

    def _clear(self):
        if self._ctrl is not None and self._sendable():
            self._ctrl.send(action="notes", text="")
        self._play_btn.setChecked(False)
        self._sent_ctrl, self._sent_text, self._shown = None, "", False
        self._closed = self._preview = False
        self._show()

    def _move(self, by: int):
        if self._is_sent():
            self._ctrl.send(action="notes_scroll", by=by)

    def _send_style(self):
        if self._is_sent():
            self._ctrl.send(**self._style())

    def _is_sent(self) -> bool:
        return self._ctrl is not None and self._sent_ctrl is self._ctrl

    def _on_lens(self, cam: dict):
        if isinstance(cam, dict) and cam.get("facing"):
            self._front = cam.get("facing") == "front"
            self._show()

    def _on_source(self, sid: str):
        self._source = sid
        self._show()

    def _show(self):
        if not hasattr(self, "_status"):
            return
        auto = self._scroll() == "auto"
        self._speed_row.setVisible(auto)
        self._play_btn.setVisible(auto)
        text = self._editor.toPlainText()
        too_long = body_size(text) > NOTES_MAX_BYTES
        sent = self._is_sent()
        status, kind = notes_status(
            streaming=self._ctrl is not None, phone=self._source == "", takes_notes=self._takes_notes,
            too_long=too_long, sent=sent, shown=self._shown, front=self._front, edited=sent and text != self._sent_text,
            closed=self._closed, preview=self._preview)
        self._status.setText(status)
        set_status_kind(self._status, kind)
        ready = self._sendable()
        self._send_btn.setEnabled(ready and not too_long and bool(text.strip()))
        self._clear_btn.setEnabled(sent)
        for btn in (self._back_btn, self._next_btn, self._play_btn):
            btn.setEnabled(sent)

    # ── The stream ────────────────────────────────────────────────────────

    def on_stream_start(self, stream_url: str, ctrl):
        self._ctrl = ctrl
        if self._sent_ctrl is not None and self._sent_ctrl is not ctrl:
            self._sent_ctrl = None  # another phone, or a new stream: what was sent went with the old one
        self._timer.start()
        self._show()

    def on_stream_stop(self):
        if self._sent_ctrl is self._ctrl:
            self._sent_ctrl, self._sent_text = None, ""  # the phone takes the notes down when its stream ends
            self._play_btn.setChecked(False)
        self._ctrl = None
        self._takes_notes, self._front, self._shown = None, None, False
        self._timer.stop()
        self._show()

    def on_focus_gained(self, stream_url: str, ctrl):
        self._ctrl = ctrl  # the phone still shows whatever it had: nothing to send again
        self._takes_notes, self._front = None, None
        self._timer.start()
        self._show()

    def on_focus_lost(self):
        self._ctrl = None
        self._takes_notes, self._front = None, None
        self._timer.stop()
        self._show()

    def on_phone_state(self, state: dict):
        self._read_state(state)

    def _poll(self):
        ctrl = self._ctrl
        if ctrl is None:
            self._timer.stop()
            return
        if self._takes_notes is not None and self._sent_ctrl is not ctrl:
            return  # nothing to follow until notes are sent

        def fetch():
            state = ctrl.get_state()
            if state:
                self._sig.state_ready.emit(ctrl, state)
        threading.Thread(target=fetch, daemon=True).start()

    def _on_polled(self, ctrl, state: dict):
        if ctrl is self._ctrl:  # a reply from a stream that has stopped since is dropped
            self._read_state(state)

    def _read_state(self, state: dict):
        if "cameras" not in state:
            return  # not a whole state
        self._takes_notes = bool(state.get("takes_notes"))
        self._front = front_facing(state)
        if self._is_sent():
            self._shown = bool(state.get("notes_shown"))
            self._closed = bool(state.get("notes_closed"))  # older phones leave these out
            self._preview = bool(state.get("notes_preview"))
            lost = self._takes_notes and not state.get("notes")
            if lost and time.monotonic() - self._sent_at > _RESEND_AFTER_S:
                self._ctrl.send(**self._style())
                self._ctrl.send(action="notes", text=self._sent_text)
                self._sent_at = time.monotonic()
        self._show()

    def shutdown(self):
        self._timer.stop()

    # ── Config ────────────────────────────────────────────────────────────

    def get_config(self) -> dict:
        return {"sets": list(self._sets), "current": self._current, "size": self._size_slider.value(),
                "speed": self._speed_slider.value(), "scroll": self._scroll()}

    def set_config(self, cfg: dict):
        sets = cfg.get("sets")
        sets = [s for s in sets if isinstance(s, str)][:MAX_SETS] if isinstance(sets, list) else []
        self._sets = sets or [""]
        current = cfg.get("current")
        self._current = current if isinstance(current, int) and 0 <= current < len(self._sets) else 0
        self._size_slider.setValue(_clamp(cfg.get("size"), SIZE_DEFAULT, SIZE_MIN, SIZE_MAX))
        self._speed_slider.setValue(_clamp(cfg.get("speed"), SPEED_DEFAULT, 1, SPEED_MAX))
        scroll = cfg.get("scroll") if cfg.get("scroll") in dict(SCROLLS) else "auto"
        self._scroll_btns[scroll].setChecked(True)
        self._fill_sets()
        self._show()

    def diagnostics(self) -> dict:
        return {"Notes": "on the phone" if self._is_sent() else "not sent",
                "Notes on phone": {None: "not known", True: "yes", False: "phone too old"}[self._takes_notes]}


def _clamp(value, default: int, lo: int, hi: int) -> int:
    try:
        return min(hi, max(lo, int(value)))
    except (TypeError, ValueError):
        return default
