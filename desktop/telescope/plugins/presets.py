"""Named snapshots of a phone's camera, stream output and transform settings.

Per phone (lens ids and sizes differ between phones), so the host keeps them with the other per-device
configs. Applying one hands each section to its plugin's apply_preset(), which also sends it to the
phone while streaming.
"""

from typing import Optional

from PyQt6.QtCore import QPoint, QSize
from PyQt6.QtGui import QAction
from PyQt6.QtWidgets import QInputDialog, QLineEdit, QMenu, QPushButton, QWidget

from telescope import theme
from telescope.plugin import TelescopePlugin
from telescope.widgets.common import create_vector_icon

# Applied in this order: the lens switch goes out before stream output looks up its sizes.
SECTIONS = ("camera_control", "stream_output", "transforms")
_KEYS = {"camera_control": "camera", "stream_output": "stream", "transforms": "transforms"}
_MAX_NAME = 40


def clean_presets(raw) -> list:
    """Keep well-formed entries with a unique non-empty name, in order."""
    out, seen = [], set()
    for p in raw if isinstance(raw, list) else []:
        if not isinstance(p, dict):
            continue
        name = str(p.get("name", "")).strip()[:_MAX_NAME]
        if not name or name in seen:
            continue
        seen.add(name)
        entry = {"name": name}
        for key in _KEYS.values():
            if isinstance(p.get(key), dict):
                entry[key] = dict(p[key])
        out.append(entry)
    return out


class PresetsPlugin(TelescopePlugin):
    name = "presets"
    header_side = "left"

    def setup(self, host, bus):
        self._host = host
        self._bus = bus
        self._presets: list = []

    # ── Data ──────────────────────────────────────────────────────────────────

    def names(self) -> list:
        return [p["name"] for p in self._presets]

    def find(self, name: str) -> Optional[dict]:
        return next((p for p in self._presets if p["name"] == name), None)

    def save(self, name: str):
        """Snapshot the current settings under name, replacing a preset of the same name in place."""
        name = name.strip()[:_MAX_NAME]
        if not name:
            return
        entry = {"name": name}
        for section in SECTIONS:
            cfg = self._host.plugin_config(section)
            if cfg is not None:
                entry[_KEYS[section]] = cfg
        existing = self.find(name)
        if existing is not None:
            self._presets[self._presets.index(existing)] = entry
        else:
            self._presets.append(entry)
        self._host.schedule_save()

    def apply(self, name: str):
        preset = self.find(name)
        if preset is None:
            return
        for section in SECTIONS:
            cfg = preset.get(_KEYS[section])
            if cfg is not None:
                self._host.apply_preset(section, cfg)
        self._host.schedule_save()

    def rename(self, old: str, new: str):
        new = new.strip()[:_MAX_NAME]
        preset = self.find(old)
        if preset is None or not new or new == old:
            return
        clash = self.find(new)
        if clash is not None:
            self._presets.remove(clash)
        preset["name"] = new
        self._host.schedule_save()

    def delete(self, name: str):
        preset = self.find(name)
        if preset is not None:
            self._presets.remove(preset)
            self._host.schedule_save()

    def get_config(self) -> dict:
        return {"presets": [dict(p) for p in self._presets]}

    def set_config(self, cfg: dict):
        self._presets = clean_presets(cfg.get("presets"))

    # ── UI ────────────────────────────────────────────────────────────────────

    def create_header_widget(self) -> QWidget:
        self._btn = QPushButton("Presets")
        self._btn.setFixedHeight(34)
        self._btn.setIcon(create_vector_icon("preset", theme.TEXT_DIM))
        self._btn.setIconSize(QSize(16, 16))
        self._btn.setToolTip("This phone's saved camera, output and transform settings")
        self._btn.clicked.connect(self._show_menu)
        return self._btn

    def build_menu(self, parent=None) -> QMenu:
        menu = QMenu(parent)
        for name in self.names():
            action = QAction(name, menu)
            action.triggered.connect(lambda _=False, n=name: self.apply(n))
            menu.addAction(action)
        if self._presets:
            menu.addSeparator()
        save = QAction("Save current as…", menu)
        save.triggered.connect(self._ask_save)
        menu.addAction(save)
        for label, handler in (("Rename", self._ask_rename), ("Delete", self.delete)):
            sub = menu.addMenu(label)
            sub.setEnabled(bool(self._presets))
            for name in self.names():
                action = QAction(name, sub)
                action.triggered.connect(lambda _=False, n=name, h=handler: h(n))
                sub.addAction(action)
        return menu

    def _show_menu(self):
        menu = self.build_menu(self._btn)
        menu.exec(self._btn.mapToGlobal(self._btn.rect().bottomLeft()) + QPoint(0, 6))

    def _ask_save(self):
        name, ok = QInputDialog.getText(self._btn.window(), "Save preset", "Name:", QLineEdit.EchoMode.Normal,
                                        f"Preset {len(self._presets) + 1}")
        if ok:
            self.save(name)

    def _ask_rename(self, old: str):
        name, ok = QInputDialog.getText(self._btn.window(), "Rename preset", "Name:", QLineEdit.EchoMode.Normal, old)
        if ok:
            self.rename(old, name)
