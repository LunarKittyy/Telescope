"""Named snapshots of the settings of every plugin that's in_presets (the camera, stream output, transforms and so on).

Per phone (lens ids and sizes differ between phones), so the host keeps them with the other per-device
configs. Applying one hands each section to its plugin's apply_preset(), which also sends it to the
phone while streaming.
"""

import logging
from typing import Optional

from PyQt6.QtCore import QPoint
from PyQt6.QtGui import QAction
from PyQt6.QtWidgets import QInputDialog, QLineEdit, QMenu, QMessageBox, QPushButton, QWidget

from telescope import theme
from telescope.plugin import TelescopePlugin
from telescope.session_client import clean_name
from telescope.shortcuts import Param, ShortcutAction
from telescope.widgets.common import set_button_icon
from telescope.widgets.line_icons import line_icon
from telescope.i18n import N_, tr

logger = logging.getLogger(__name__)

# The keys the first three sections were saved under; any other plugin's section is under its name
_KEYS = {"camera_control": "camera", "stream_output": "stream", "transforms": "transforms"}
_MAX_NAME = 40


def _clean(raw) -> str:
    """A preset name as safe display text (no control characters or < >), capped at _MAX_NAME."""
    return clean_name(raw)[:_MAX_NAME].strip()


def _menu_text(name: str) -> str:
    """A preset name as menu text: a menu reads "&" as a shortcut marker, so "Desk & window" would lose it."""
    return name.replace("&", "&&")


def clean_presets(raw) -> list:
    """Keep well-formed entries with a unique non-empty name, in order."""
    out, seen = [], set()
    for p in raw if isinstance(raw, list) else []:
        if not isinstance(p, dict):
            continue
        name = _clean(p.get("name", ""))
        if not name or name in seen:
            continue
        seen.add(name)
        entry = {"name": name}
        for key, section in p.items():
            if key != "name" and isinstance(key, str) and isinstance(section, dict):
                entry[key] = dict(section)  # a switched-off plugin's section stays for when it's back
        out.append(entry)
    return out


class PresetsPlugin(TelescopePlugin):
    name = "presets"
    title = N_("Presets")
    about = N_("Saving and loading groups of settings with one click.")
    category = "extras"
    icon = "preset"
    when_off = N_("Without it there's no Presets button in the header (your saved presets aren't deleted).")
    device_local = True
    header_side = "left"

    def setup(self, host, bus):
        self._host = host
        self._bus = bus
        self._presets: list = []
        self._last: Optional[str] = None  # the preset applied last, for Next and Previous

    # ── Data ──────────────────────────────────────────────────────────────────

    def names(self) -> list:
        return [p["name"] for p in self._presets]

    def find(self, name: str) -> Optional[dict]:
        return next((p for p in self._presets if p["name"] == name), None)

    def save(self, name: str):
        """Snapshot the current settings under name, replacing a preset of the same name in place."""
        name = _clean(name)
        if not name:
            return
        existing = self.find(name)
        entry = dict(existing) if existing is not None else {"name": name}
        for section in self._host.preset_plugins():
            cfg = self._host.plugin_config(section)
            if cfg is not None:
                entry[_KEYS.get(section, section)] = cfg
        if existing is not None:
            self._presets[self._presets.index(existing)] = entry
        else:
            self._presets.append(entry)
        self._host.schedule_save()

    def apply(self, name: str):
        preset = self.find(name)
        if preset is None:
            return
        self._last = name
        # In registration order: the lens switch goes out before stream output looks up its sizes
        for section in self._host.preset_plugins():
            cfg = preset.get(_KEYS.get(section, section))
            if cfg is not None:
                try:
                    self._host.apply_preset(section, cfg)
                except Exception:  # one section that won't apply still leaves the others applied
                    logger.exception("Couldn't apply the %s part of preset %r", section, name)
        self._host.schedule_save()

    def rename(self, old: str, new: str):
        new = _clean(new)
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

    # ── Shortcuts ─────────────────────────────────────────────────────────────

    def create_actions(self) -> list:
        def apply(p):
            if self.find(p.get("name")) is None:
                return None
            self.apply(p["name"])
            return tr("Preset: {name}", name=p["name"])

        def cycle(p):
            names = self.names()
            if not names:
                return None
            at = names.index(self._last) if self._last in names else (-1 if p.get("op") == "next" else 0)
            name = names[(at + (1 if p.get("op") == "next" else -1)) % len(names)]
            self.apply(name)
            return tr("Preset: {name}", name=name)

        ops = (("next", tr("Next")), ("previous", tr("Previous")))
        group = tr("Presets")
        return [
            ShortcutAction("presets.apply", tr("Apply a preset"), group, apply,
                           params=(Param("name", tr("Preset"), "choice", "",
                                         lambda: [(n, n) for n in self.names()]),),
                           describe=lambda p: tr("Preset: {name}", name=p.get('name') or '…')),
            ShortcutAction("presets.cycle", tr("Next or previous preset"), group, cycle,
                           params=(Param("op", tr("Does"), "choice", "next", ops),),
                           describe=lambda p: tr("Preset: {name}", name=dict(ops)[p['op']].lower())),
        ]

    # ── UI ────────────────────────────────────────────────────────────────────

    def create_header_widget(self) -> QWidget:
        self._btn = QPushButton(tr("Presets"))
        self._btn.setFixedHeight(34)
        set_button_icon(self._btn, line_icon("bookmark", theme.ACCENT_SOFT))
        self._btn.setToolTip(tr("This phone's saved camera, output, transform, framing and background settings"))
        self._btn.clicked.connect(self._show_menu)
        return self._btn

    def build_menu(self, parent=None) -> QMenu:
        menu = QMenu(parent)
        for name in self.names():
            action = QAction(_menu_text(name), menu)
            action.triggered.connect(lambda _=False, n=name: self.apply(n))
            menu.addAction(action)
        if self._presets:
            menu.addSeparator()
        save = QAction(tr("Save current as…"), menu)
        save.triggered.connect(self._ask_save)
        menu.addAction(save)
        for label, handler in ((tr("Rename"), self._ask_rename), (tr("Delete"), self.delete)):
            sub = menu.addMenu(label)
            sub.setEnabled(bool(self._presets))
            for name in self.names():
                action = QAction(_menu_text(name), sub)
                action.triggered.connect(lambda _=False, n=name, h=handler: h(n))
                sub.addAction(action)
        return menu

    def _show_menu(self):
        menu = self.build_menu(self._btn)
        menu.exec(self._btn.mapToGlobal(self._btn.rect().bottomLeft()) + QPoint(0, 6))

    def unused_name(self) -> str:
        n = len(self._presets) + 1
        while self.find(tr("Preset {n}", n=n)) is not None:
            n += 1
        return tr("Preset {n}", n=n)

    def _may_replace(self, name: str, keep: Optional[str] = None) -> bool:
        """Whether saving as name may go ahead: yes when no other preset has it, else ask first."""
        name = _clean(name)
        if not name or name == keep or self.find(name) is None:
            return True
        answer = QMessageBox.question(self._btn.window(), tr("Replace preset"),
                                      tr('There\'s already a preset called "{name}". Replace it?', name=name))
        return answer == QMessageBox.StandardButton.Yes

    def _ask_save(self):
        name, ok = QInputDialog.getText(self._btn.window(), tr("Save preset"), tr("Name:"), QLineEdit.EchoMode.Normal,
                                        self.unused_name())
        if ok and self._may_replace(name):
            self.save(name)

    def _ask_rename(self, old: str):
        name, ok = QInputDialog.getText(self._btn.window(), tr("Rename preset"), tr("Name:"),
                                        QLineEdit.EchoMode.Normal, old)
        if ok and self._may_replace(name, keep=old):
            self.rename(old, name)
