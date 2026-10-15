"""The Plugins dialog (settings menu): every plugin, built in or added, with a switch for each one that can be off."""

import logging
from pathlib import Path
from typing import Callable

from PyQt6.QtCore import QSignalBlocker, Qt, QUrl
from PyQt6.QtGui import QDesktopServices
from PyQt6.QtWidgets import (
    QCheckBox, QDialog, QFileDialog, QFrame, QHBoxLayout, QLabel, QMessageBox, QPushButton, QScrollArea,
    QVBoxLayout, QWidget,
)

from telescope import plugin_loader
from telescope.widgets.common import (
    add_section_heading, dialog_buttons, dialog_header, dialog_layout, set_ui_role, ui_px, wrapped_note,
)

logger = logging.getLogger(__name__)

DOCS_URL = "https://github.com/LunarKittyy/Telescope/blob/master/docs/plugins.md"


def _ask(parent, title: str, text: str, yes: str, danger: bool = False) -> bool:
    box = QMessageBox(QMessageBox.Icon.Warning if danger else QMessageBox.Icon.Question, title, text, parent=parent)
    yes_btn = box.addButton(yes, QMessageBox.ButtonRole.AcceptRole)
    cancel = box.addButton("Cancel", QMessageBox.ButtonRole.RejectRole)
    box.setDefaultButton(cancel)
    box.exec()
    return box.clickedButton() is yes_btn


class PluginsDialog(QDialog):
    def __init__(self, parent, builtins: list, running: set, problems: dict, restart: Callable[[], bool],
                 is_streaming: Callable[[], bool] = lambda: False):
        """builtins: every built-in plugin object; running: the names of the plugins running now; problems: why each
        added plugin that's on didn't start this time; restart() starts Telescope again, False if it couldn't."""
        super().__init__(parent)
        self._builtins = list(builtins)
        self._reserved = {p.name for p in builtins}
        self._running = set(running)
        self._problems = dict(problems)
        self._restart = restart
        self._is_streaming = is_streaming
        self.setWindowTitle("Plugins")
        self.setWindowFlag(Qt.WindowType.WindowContextHelpButtonHint, False)
        self.setMinimumWidth(ui_px(600))
        self.setAcceptDrops(True)
        lay = dialog_layout(self)
        dialog_header(lay, "Plugins", "Switch parts of Telescope on or off. A change takes effect the next time "
                                      "Telescope starts, and settings are kept either way.")

        self._list = QWidget()
        self._rows = QVBoxLayout(self._list)
        self._rows.setContentsMargins(0, 0, 8, 0)
        self._rows.setSpacing(10)
        scroll = QScrollArea()
        scroll.setWidgetResizable(True)
        scroll.setFrameShape(QFrame.Shape.NoFrame)
        scroll.setWidget(self._list)
        scroll.setMinimumHeight(ui_px(380))
        lay.addWidget(scroll, 1)

        self._restart_bar = QWidget()
        bar = QHBoxLayout(self._restart_bar)
        bar.setContentsMargins(0, 0, 0, 0)
        self._restart_note = wrapped_note("Restart Telescope to apply your changes.", "status_warn")
        bar.addWidget(self._restart_note, 1)
        self._restart_btn = QPushButton("Restart now")
        set_ui_role(self._restart_btn, "primary")
        self._restart_btn.clicked.connect(self._restart_now)
        bar.addWidget(self._restart_btn)
        lay.addWidget(self._restart_bar)

        self._add_btn = QPushButton("Install plugin…")
        self._add_btn.setToolTip("Pick a plugin's .zip file. You can also drop one onto this window.")
        self._add_btn.clicked.connect(self._add_zip)
        self._folder_btn = QPushButton("Open plugins folder")
        self._folder_btn.clicked.connect(self._open_folder)
        self._all_on_btn = QPushButton("Turn all back on")
        self._all_on_btn.setToolTip("Switch every built-in plugin back on")
        self._all_on_btn.clicked.connect(self._all_on)
        help_btn = QPushButton("How plugins work")
        help_btn.clicked.connect(lambda: QDesktopServices.openUrl(QUrl(DOCS_URL)))
        close_btn = QPushButton("Close")
        close_btn.clicked.connect(self.accept)
        tools = QHBoxLayout()
        tools.setSpacing(8)
        for btn in (self._add_btn, self._folder_btn, self._all_on_btn, help_btn):
            tools.addWidget(btn)
        tools.addStretch(1)
        lay.addLayout(tools)
        dialog_buttons(lay, close_btn)
        self.refresh()

    # ── The list ──────────────────────────────────────────────────────────

    def refresh(self):
        while self._rows.count():
            item = self._rows.takeAt(0)
            if item.widget() is not None:
                item.widget().deleteLater()
        off = self._read(plugin_loader.builtin_off, set())
        self._installed = self._read(lambda: plugin_loader.find_installed(self._reserved), [])

        add_section_heading(self._rows, "Built in")
        for plugin in self._builtins:
            if not plugin.required:
                self._rows.addWidget(self._builtin_row(plugin, plugin.name not in off))
        add_section_heading(self._rows, "Added")
        if not self._installed:
            self._rows.addWidget(wrapped_note(
                "No plugins added yet. A plugin someone shared comes as a .zip file: click Install plugin and pick "
                "it, or drop it onto this window. Added plugins start out switched off."))
        for item in self._installed:
            self._rows.addWidget(self._added_row(item))
        add_section_heading(self._rows, "Always on")
        for plugin in self._builtins:
            if plugin.required:
                self._rows.addWidget(self._builtin_row(plugin, True))
        self._rows.addStretch(1)

        self._all_on_btn.setEnabled(any(p.name in off for p in self._builtins if not p.required))
        wanted = {p.name for p in self._builtins if p.required or p.name not in off}
        wanted |= {i.name for i in self._installed if i.enabled and not i.problem}
        known = self._reserved | {i.name for i in self._installed} | set(self._problems)
        self._restart_bar.setVisible(wanted != (self._running & known))

    def _row(self, title: str, on: bool, about: str, note: str = "", note_kind: str = "dim",
             locked: bool = False, tooltip: str = "") -> tuple:
        row = QWidget()
        lay = QVBoxLayout(row)
        lay.setContentsMargins(0, 0, 0, 0)
        lay.setSpacing(2)
        top = QHBoxLayout()
        if locked:
            box = QLabel(title)  # no switch to grey out: a greyed box reads as off
        else:
            box = QCheckBox(title)
            box.setChecked(on)
        box.setToolTip(tooltip)
        top.addWidget(box)
        top.addStretch(1)
        lay.addLayout(top)
        indent = 0 if locked else ui_px(26)  # under the title, past the box
        if about:
            text = wrapped_note(about)
            text.setContentsMargins(indent, 0, 0, 0)
            lay.addWidget(text)
        if note:
            extra = wrapped_note(note, note_kind)
            extra.setContentsMargins(indent, 0, 0, 0)
            lay.addWidget(extra)
        return row, box, top

    def _builtin_row(self, plugin, on: bool) -> QWidget:
        title = plugin.title or plugin.name
        if plugin.required:
            row, _box, _top = self._row(title, True, plugin.about, locked=True,
                                        tooltip="Telescope can't work without this one, so it can't be switched off")
            return row
        note, kind = "", "dim"
        if on and plugin.name not in self._running:
            note, kind = "Switched on. It comes back when Telescope restarts.", "status_warn"
        elif not on and plugin.name in self._running:
            note, kind = "Switched off. It goes away when Telescope restarts.", "status_warn"
        elif not on:
            note = "Off. " + plugin.when_off if plugin.when_off else "Off."
        row, box, _top = self._row(title, on, plugin.about, note, kind)
        box.toggled.connect(lambda checked, p=plugin, b=box: self._switch_builtin(p, checked, b))
        return row

    def _added_row(self, item) -> QWidget:
        title = f"{item.title} {item.version}".strip()
        about = item.description + (f" By {item.author}." if item.author else "")
        note, kind = "", "dim"
        if item.problem:
            note, kind = item.problem, "status_err"
        elif item.changed:
            note, kind = ("Its files changed since you switched it on, so it's off for now. Switch it on again if "
                          "you trust this version."), "status_warn"
        elif item.name in self._problems and item.enabled:
            note, kind = "It didn't start: " + self._problems[item.name], "status_err"
        elif item.enabled and item.name not in self._running:
            note, kind = "Switched on. It starts when Telescope restarts.", "status_warn"
        elif not item.enabled and item.name in self._running:
            note, kind = "Switched off. It goes away when Telescope restarts.", "status_warn"
        row, box, top = self._row(title, item.enabled, about, note, kind)
        box.setEnabled(not item.problem)
        box.toggled.connect(lambda checked, i=item, b=box: self._switch_added(i, checked, b))
        remove = QPushButton("Remove")
        remove.clicked.connect(lambda _=False, i=item: self._remove(i))
        top.addWidget(remove)
        return row

    # ── Switching ─────────────────────────────────────────────────────────

    def _switch_builtin(self, plugin, on: bool, box: QCheckBox):
        title = plugin.title or plugin.name
        if not on and not _ask(self, f"Switch off {title}?",
                               (plugin.when_off + "\n\n" if plugin.when_off else "")
                               + "Its settings are kept, so switching it back on here brings everything back.",
                               "Switch off"):
            with QSignalBlocker(box):
                box.setChecked(True)
            return
        self._save(lambda: plugin_loader.set_builtin_off(plugin.name, not on))

    def _switch_added(self, item, on: bool, box: QCheckBox):
        if on:
            ok = _ask(self, f"Switch on {item.title}?",
                      "A plugin can do anything Telescope can on this computer, like reading your files or using "
                      "the network. Only switch it on if you trust whoever made it.", "Switch on", danger=True)
        else:
            ok = _ask(self, f"Switch off {item.title}?",
                      "Whatever it added to Telescope goes away. Its settings are kept, so switching it back on "
                      "brings everything back.", "Switch off")
        if not ok:
            with QSignalBlocker(box):
                box.setChecked(not on)
            return
        self._save(lambda: plugin_loader.set_enabled(item, on))

    def _all_on(self):
        self._save(plugin_loader.all_builtin_on)

    def _remove(self, item):
        if not _ask(self, f"Remove {item.title}?",
                    f"Its folder ({item.folder.name}) is deleted from the plugins folder. To use it again you'll "
                    "have to add it again.", "Remove", danger=True):
            return
        try:
            plugin_loader.remove(item)
        except (OSError, ValueError) as exc:
            logger.exception("Couldn't remove plugin %s", item.name)
            QMessageBox.warning(self, "Couldn't remove it", f"Telescope couldn't delete its folder: {exc}")
        self.refresh()

    def _save(self, change: Callable[[], None]):
        try:
            change()
        except (OSError, ValueError) as exc:
            logger.exception("Couldn't save a plugin switch")
            QMessageBox.warning(self, "Couldn't save that", f"Telescope couldn't save the change: {exc}")
        self.refresh()

    def _read(self, fn, fallback):
        try:
            return fn()
        except OSError:
            logger.exception("Couldn't read the plugin switches")
            return fallback

    # ── Adding ────────────────────────────────────────────────────────────

    def dragEnterEvent(self, event):
        if self._dropped(event) is not None:
            event.acceptProposedAction()

    def dropEvent(self, event):
        path = self._dropped(event)
        if path is not None:
            event.acceptProposedAction()
            self._install(path)

    @staticmethod
    def _dropped(event):
        """The one .zip file being dragged in, or None."""
        urls = event.mimeData().urls() if event.mimeData().hasUrls() else []
        if len(urls) == 1 and urls[0].isLocalFile() and urls[0].toLocalFile().lower().endswith(".zip"):
            return urls[0].toLocalFile()
        return None

    def _add_zip(self):
        path, _ = QFileDialog.getOpenFileName(self, "Install a plugin", str(Path.home() / "Downloads")
                                              if (Path.home() / "Downloads").is_dir() else str(Path.home()),
                                              "Plugins (*.zip)")
        if path:
            self._install(path)

    def _install(self, path: str):
        try:
            item = plugin_loader.install_zip(Path(path), self._reserved)
        except FileExistsError as exc:
            QMessageBox.warning(self, "Already installed", f"A plugin called {exc.args[0]} is already installed. "
                                                           "Remove that one first to put this one in its place.")
            return
        except (OSError, ValueError) as exc:
            QMessageBox.warning(self, "Couldn't install it", str(exc))
            return
        finally:
            self.refresh()
        if item.problem:
            QMessageBox.warning(self, "Installed, but it won't load", f"{item.title} was installed, but: "
                                                                     f"{item.problem}")
        else:
            QMessageBox.information(self, "Installed", f"{item.title} is installed. It's switched off for now: "
                                                       "switch it on in the list under Added when you're ready.")

    def _open_folder(self):
        try:
            folder = plugin_loader.make_plugins_dir()
        except OSError as exc:
            QMessageBox.warning(self, "Couldn't open it", f"Telescope couldn't make its plugins folder: {exc}")
            return
        QDesktopServices.openUrl(QUrl.fromLocalFile(str(folder)))

    def _restart_now(self):
        if self._is_streaming() and not _ask(self, "Restart Telescope now?",
                                             "Streaming stops while Telescope restarts. It takes a few seconds.",
                                             "Restart"):
            return
        if not self._restart():
            QMessageBox.warning(self, "Couldn't restart", "Telescope couldn't start a new copy of itself. Close "
                                                          "it and open it again to apply your changes.")
            return
        self.accept()
