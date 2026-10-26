"""Name tag: a lower third with your name, pronouns and an optional second line (see telescope/name_tag.py).

The window's view (zoom, pan, flip and rotation) runs before every plugin's process_frame, so the tag is drawn onto the
picture as apps get it: Mirror in Transforms never mirrors the text, and left means left in the call.
"""

from typing import Optional

import numpy as np
from PyQt6.QtCore import QSignalBlocker
from PyQt6.QtWidgets import QLineEdit, QSizePolicy, QWidget

from telescope import name_tag
from telescope.plugin import TelescopePlugin
from telescope.shortcuts import button_action
from telescope.widgets.common import (
    NoScrollComboBox, Switch, add_card_header, card_layout, control_row, create_card,
)
from telescope.widgets.settings_search import keywords
from telescope.i18n import N_, tr

STYLE_TEXTS = {"bar": N_("Bar"), "pill": N_("Pill"), "light": N_("Light"), "shadow": N_("Shadow")}
POSITION_TEXTS = {"bottom_left": N_("Bottom left"), "bottom_centre": N_("Bottom centre"),
                  "bottom_right": N_("Bottom right"), "top_left": N_("Top left"), "top_right": N_("Top right")}
SIZE_TEXTS = {"small": N_("Small"), "medium": N_("Medium"), "large": N_("Large")}


class _Settings:
    """What a frame step needs, frozen: the spec and its own cache (a stream behind the panels keeps the ones it had)."""

    def __init__(self, on: bool = False, spec: Optional[name_tag.Spec] = None):
        self.on, self.spec = on, spec or name_tag.Spec()
        self.cache = name_tag.TagCache()

    @property
    def shows(self) -> bool:
        return self.on and not self.spec.empty


class NameTagPlugin(TelescopePlugin):
    name = "name_tag"
    title = N_("Name tag")
    about = N_("Puts your name and pronouns in the corner of the picture.")
    when_off = N_("Without it there's no Name tag card and nothing is drawn on the picture.")
    optional = True
    in_presets = True  # a preset can be a scene: the tag for a talk, none for a chat
    panel_region = "right"

    def setup(self, host, bus):
        self._host, self._bus = host, bus
        self._settings = _Settings()  # Written by Qt thread; read by worker thread (swapped whole, GIL-atomic).

    # ── Card ──────────────────────────────────────────────────────────────

    def create_panel(self) -> QWidget:
        card = create_card()
        lay = card_layout(card)
        self._switch = Switch("")
        self._switch.setSizePolicy(QSizePolicy.Policy.Fixed, QSizePolicy.Policy.Fixed)
        self._switch.setToolTip(tr("Show the name tag on the picture"))
        self._switch.toggled.connect(self._changed)
        add_card_header(lay, tr("Name tag"), "bookmark", action=self._switch)
        keywords(card, tr("lower third caption title label overlay text name pronouns role", ctx="search"))

        self._name_edit = self._edit()
        lay.addLayout(control_row(tr("Name"), self._name_edit, stretch=True))
        self._pronouns_edit = self._edit(tr("Shown next to your name, like she/her or they/them"))
        lay.addLayout(control_row(tr("Pronouns"), self._pronouns_edit, stretch=True))
        self._second_edit = self._edit(tr("A smaller line under your name, like your role. Leave it empty for none"))
        lay.addLayout(control_row(tr("Second line"), self._second_edit, stretch=True))
        self._style_combo = self._combo(name_tag.STYLES, STYLE_TEXTS, name_tag.DEFAULT_STYLE)
        lay.addLayout(control_row(tr("Style"), self._style_combo, stretch=True))
        self._position_combo = self._combo(name_tag.POSITIONS, POSITION_TEXTS, name_tag.DEFAULT_POSITION)
        lay.addLayout(control_row(tr("Position"), self._position_combo, stretch=True))
        self._size_combo = self._combo(name_tag.SIZES, SIZE_TEXTS, name_tag.DEFAULT_SIZE)
        lay.addLayout(control_row(tr("Size"), self._size_combo, stretch=True))
        return card

    def _edit(self, tooltip: str = "") -> QLineEdit:
        edit = QLineEdit()
        edit.setToolTip(tooltip)
        edit.setMaxLength(name_tag.MAX_CHARS)
        edit.textChanged.connect(self._changed)
        return edit

    def _combo(self, keys, texts: dict, default: str) -> NoScrollComboBox:
        combo = NoScrollComboBox()
        combo.setSizeAdjustPolicy(NoScrollComboBox.SizeAdjustPolicy.AdjustToMinimumContentsLengthWithIcon)
        combo.setMinimumContentsLength(6)  # a long choice can't widen the card
        for key in keys:
            combo.addItem(tr(texts[key]), key)
        combo.setCurrentIndex(combo.findData(default))
        combo.currentIndexChanged.connect(self._changed)
        return combo

    def create_actions(self) -> list:
        return [button_action("name_tag.on", tr("Name tag"), tr("Name tag"), self._switch)]

    # ── Handlers (Qt thread) ──────────────────────────────────────────────

    def spec(self) -> name_tag.Spec:
        return name_tag.Spec(
            name=self._name_edit.text(), pronouns=self._pronouns_edit.text(), second=self._second_edit.text(),
            style=self._style_combo.currentData() or name_tag.DEFAULT_STYLE,
            position=self._position_combo.currentData() or name_tag.DEFAULT_POSITION,
            size=self._size_combo.currentData() or name_tag.DEFAULT_SIZE,
            families=name_tag.app_families())

    def _apply(self):
        spec, on = self.spec(), self._switch.isChecked()
        if (on, spec) != (self._settings.on, self._settings.spec):
            self._settings = _Settings(on, spec)

    def _changed(self, *_args):
        self._apply()
        self._host.schedule_save()

    # ── Frames (worker thread) ────────────────────────────────────────────

    def process_frame(self, frame: np.ndarray) -> np.ndarray:
        return self._step(frame, self._settings)

    def frame_step(self):
        settings = self._settings
        if not settings.shows:
            return None
        return lambda frame: self._step(frame, settings)

    @staticmethod
    def _step(frame: np.ndarray, settings: _Settings) -> np.ndarray:
        if not settings.shows:
            return frame
        h, w = frame.shape[:2]
        tag = settings.cache.get(settings.spec, w, h)
        return name_tag.apply(frame, tag) if tag is not None else frame

    # ── Config ────────────────────────────────────────────────────────────

    def get_config(self) -> dict:
        spec = self.spec()
        return {"on": self._switch.isChecked(), "name": spec.name, "pronouns": spec.pronouns, "second": spec.second,
                "style": spec.style, "position": spec.position, "size": spec.size}

    def set_config(self, cfg: dict):
        def text(key: str) -> str:
            value = cfg.get(key)
            return value[:name_tag.MAX_CHARS] if isinstance(value, str) else ""

        def pick(key: str, choices, default: str) -> str:
            return cfg.get(key) if cfg.get(key) in choices else default

        for edit, key in ((self._name_edit, "name"), (self._pronouns_edit, "pronouns"), (self._second_edit, "second")):
            with QSignalBlocker(edit):
                edit.setText(text(key))
        for combo, key, choices, default in (
                (self._style_combo, "style", name_tag.STYLES, name_tag.DEFAULT_STYLE),
                (self._position_combo, "position", name_tag.POSITIONS, name_tag.DEFAULT_POSITION),
                (self._size_combo, "size", name_tag.SIZES, name_tag.DEFAULT_SIZE)):
            with QSignalBlocker(combo):
                combo.setCurrentIndex(combo.findData(pick(key, choices, default)))
        with QSignalBlocker(self._switch):
            self._switch.setChecked(cfg.get("on") is True)
        self._apply()

    def diagnostics(self) -> dict:
        settings = self._settings
        return {"Name tag": f"on ({settings.spec.style}, {settings.spec.size})" if settings.shows else "off"}
