"""The Plugins tab of Settings: every plugin, built in or added, with a switch for each one that can be off."""

import logging
from pathlib import Path
from typing import Callable, Optional

from PyQt6.QtCore import QSignalBlocker, Qt, QUrl
from PyQt6.QtGui import QDesktopServices
from PyQt6.QtWidgets import (
    QCheckBox, QFileDialog, QFrame, QHBoxLayout, QLabel, QMessageBox, QPushButton, QScrollArea, QVBoxLayout, QWidget,
)

from telescope import plugin_loader
from telescope.widgets import vision_download
from telescope.widgets.common import (
    Switch,
    add_card_header, card_action, card_layout, create_card, icon_button, set_ui_role, wrapped_note,
)
from telescope.widgets.controls import setting_row
from telescope.widgets.settings_search import keywords
from telescope.i18n import tr

logger = logging.getLogger(__name__)

DOCS_URL = "https://github.com/LunarKittyy/Telescope/blob/master/docs/plugins.md"


def _ask(parent, title: str, text: str, yes: str, danger: bool = False) -> bool:
    box = QMessageBox(QMessageBox.Icon.Warning if danger else QMessageBox.Icon.Question, title, text, parent=parent)
    yes_btn = box.addButton(yes, QMessageBox.ButtonRole.AcceptRole)
    cancel = box.addButton(tr("Cancel"), QMessageBox.ButtonRole.RejectRole)
    box.setDefaultButton(cancel)
    box.exec()
    return box.clickedButton() is yes_btn


class PluginsPanel(QWidget):
    def __init__(self, builtins: list, running: set, problems: dict, restart: Callable[[], bool],
                 is_streaming: Callable[[], bool] = lambda: False, download=None):
        """builtins: every built-in plugin object; running: the names of the plugins running now; problems: why each
        added plugin that's on didn't start this time; restart() starts Telescope again, False if it couldn't."""
        super().__init__()
        self._builtins = list(builtins)
        self._reserved = {p.name for p in builtins}
        self._running = set(running)
        self._problems = dict(problems)
        self._restart = restart
        self._is_streaming = is_streaming
        self._download = download or vision_download.shared()
        self._download.progress.connect(self._on_download_progress)
        self._download.finished.connect(self._on_download_done)
        self.setAcceptDrops(True)
        body = QVBoxLayout(self)
        body.setContentsMargins(0, 0, 0, 0)
        body.setSpacing(14)
        body.addWidget(wrapped_note(tr("Switch parts of Telescope on or off. A change takes effect the next time "
                                       "Telescope starts, and settings are kept either way.")))
        self._keep_at = 0
        self._watching = None

        self._restart_bar = QFrame()
        self._restart_bar.setObjectName("setting_row")
        bar = QHBoxLayout(self._restart_bar)
        bar.setContentsMargins(12, 8, 8, 8)
        bar.setSpacing(12)
        self._restart_note = wrapped_note(tr("Restart Telescope to apply your changes."), "status_warn")
        bar.addWidget(self._restart_note, 1)
        self._restart_btn = QPushButton(tr("Restart now"))
        set_ui_role(self._restart_btn, "primary")
        self._restart_btn.clicked.connect(self._restart_now)
        bar.addWidget(self._restart_btn)
        body.addWidget(self._restart_bar)
        self._list = QWidget()
        self._rows = QVBoxLayout(self._list)
        self._rows.setContentsMargins(0, 0, 0, 0)
        self._rows.setSpacing(body.spacing())
        body.addWidget(self._list)

        self._folder_btn = QPushButton(tr("Open plugins folder"))
        self._folder_btn.clicked.connect(self._open_folder)
        help_btn = QPushButton(tr("How plugins work"))
        help_btn.clicked.connect(lambda: QDesktopServices.openUrl(QUrl(DOCS_URL)))
        links = QHBoxLayout()
        links.setSpacing(8)
        links.addWidget(help_btn)
        links.addWidget(self._folder_btn)
        links.addStretch(1)
        body.addLayout(links)
        self.refresh()

    # ── The list ──────────────────────────────────────────────────────────

    def _scroll(self) -> Optional[QScrollArea]:
        """The page this sits in, once it's in one."""
        area = self.parentWidget()
        while area is not None and not isinstance(area, QScrollArea):
            area = area.parentWidget()
        if area is not None and area is not self._watching:
            self._watching = area
            area.verticalScrollBar().rangeChanged.connect(self._restore_scroll)
        return area

    def _restore_scroll(self, _low: int, high: int):
        if self._keep_at and high >= self._keep_at:
            self._watching.verticalScrollBar().setValue(self._keep_at)
            self._keep_at = 0

    def refresh(self):
        area = self._scroll()
        if area is not None:
            self._keep_at = area.verticalScrollBar().value()  # stay where the person was once the rows are back
        while self._rows.count():
            item = self._rows.takeAt(0)
            if item.widget() is not None:
                item.widget().hide()  # deleteLater waits for the event loop, and until then it'd paint under the new one
                item.widget().deleteLater()
        self._restart_rows = 0
        off = self._read(plugin_loader.builtin_off, set())
        on = self._read(plugin_loader.optional_on, set())
        self._installed = self._read(lambda: plugin_loader.find_installed(self._reserved), [])

        self._all_on_btn = card_action(tr("Turn all back on"), "reset", tr("Switch every built-in plugin back on"))
        self._all_on_btn.clicked.connect(self._all_on)
        self._all_on_btn.setEnabled(any(p.name in off for p in self._builtins if not p.required and not p.optional))
        rows = self._card(tr("Built in"), "plug", self._all_on_btn)
        for plugin in self._builtins:
            if not plugin.required and not plugin.optional:
                rows.addWidget(self._builtin_row(plugin, plugin.name not in off))
        optional = [p for p in self._builtins if p.optional]
        if optional:
            dl = self._download
            self._remove_pack_btn = card_action(tr("Remove download"), "trash",
                                                tr("Delete the download the plugins here use. Switching one on gets "
                                                   "it again."))
            self._remove_pack_btn.clicked.connect(self._remove_pack)
            rows = self._card(tr("Optional"), "download", self._remove_pack_btn)
            self._remove_pack_btn.setVisible(dl.ready() and not dl.running() and not any(
                p.needs_vision and (p.name in on or p.name in self._running) for p in optional))
            self._progress_note = None
            for plugin in optional:
                rows.addWidget(self._optional_row(plugin, plugin.name in on))
        self._add_btn = card_action(tr("Install"), "plus", tr("Pick a plugin's .zip file. You can also drop one "
                                                              "onto this window."))
        self._add_btn.clicked.connect(self._add_zip)
        rows = self._card(tr("Added"), "download", self._add_btn)
        if not self._installed:
            rows.addWidget(wrapped_note(tr(
                "No plugins added yet. Plugins people share come as .zip files. Click Install and pick one or "
                "drop it onto this window. Added plugins start out switched off.")))
        for item in self._installed:
            rows.addWidget(self._added_row(item))
        rows = self._card(tr("Always on"), "lock")
        for plugin in self._builtins:
            if plugin.required:
                rows.addWidget(self._builtin_row(plugin, True))

        wanted = {p.name for p in self._builtins
                  if p.required or (p.name in on if p.optional else p.name not in off)}
        wanted |= {i.name for i in self._installed if i.enabled and not i.problem}
        known = self._reserved | {i.name for i in self._installed} | set(self._problems)
        # Rows that wait on a restart carry the button; the bar is for a change no row shows, like a removed plugin.
        self._restart_bar.setVisible(wanted != (self._running & known) and not self._restart_rows)

    def _card(self, title: str, icon: str, action: QWidget = None) -> QVBoxLayout:
        """A card in the list, returning the layout its rows go in."""
        card = create_card()
        lay = card_layout(card)
        add_card_header(lay, title, icon, action=action)
        keywords(card, tr("plugins extensions addons add ons modules features turn off on disable enable install zip",
                          ctx="search"))
        self._rows.addWidget(card)
        return lay

    def _row(self, title: str, on: bool, about: str, note: str = "", note_kind: str = "dim",
             locked: bool = False, tooltip: str = "") -> tuple:
        """A plugin as a strip: its name and what it does, its switch on the right (none when locked), and a line
        under it about what happens next. Returns the strip, the switch (None when locked) and the strip's controls."""
        box = None
        controls = QHBoxLayout()
        controls.setSpacing(8)
        if not locked:
            box = Switch()
            box.setChecked(on)
            box.setAccessibleName(title)
            box.setToolTip(tooltip)
            controls.addWidget(box)
        extra = None
        if note:
            label = wrapped_note(note, {"dim": "setting_sub", "restart": "status_warn"}.get(note_kind, note_kind))
            label.setProperty("sub", True)
            extra = label
        if note_kind == "restart":  # the button right where the change was made, not only at the top of the tab
            extra = QHBoxLayout()
            extra.setSpacing(8)
            extra.addWidget(label, 1)
            restart = card_action(tr("Restart now"), "reset", tr("Start Telescope again to apply your changes"))
            restart.setObjectName("row_restart")
            restart.clicked.connect(self._restart_now)
            extra.addWidget(restart, 0, Qt.AlignmentFlag.AlignVCenter)
            self._restart_rows += 1
        strip, _line = setting_row(title, about, controls, extra)
        strip.setToolTip(tooltip if locked else "")
        return strip, box, controls

    def _builtin_row(self, plugin, on: bool) -> QWidget:
        title = tr(plugin.title or plugin.name)
        if plugin.required:
            row, _box, _top = self._row(title, True, tr(plugin.about), locked=True,
                                        tooltip=tr("Telescope can't work without this one, so it can't be switched "
                                                   "off"))
            return row
        note, kind = "", "dim"
        if on and plugin.name not in self._running:
            note, kind = tr("It comes back after Telescope restarts."), "restart"
        elif not on and plugin.name in self._running:
            note, kind = tr("It goes away after Telescope restarts."), "restart"
        elif not on and plugin.when_off:
            note = tr(plugin.when_off)
        row, box, _top = self._row(title, on, tr(plugin.about), note, kind)
        box.toggled.connect(lambda checked, p=plugin, b=box: self._switch_builtin(p, checked, b))
        return row

    def _optional_row(self, plugin, on: bool) -> QWidget:
        """An optional built-in plugin: off until switched on, which first downloads what it needs."""
        title, dl = tr(plugin.title or plugin.name), self._download
        needs = plugin.needs_vision and not dl.ready()
        note, kind = "", "dim"
        downloading = plugin.needs_vision and dl.running() and dl.waiting == plugin.name
        if downloading:
            note, kind = tr("Downloading…"), "status_warn"
        elif dl.failed_for == plugin.name and dl.error:
            note, kind = tr("The download didn't work. {error}", error=dl.error), "status_err"
        elif needs and not dl.available():
            note, kind = tr("There's no download of what it needs for this computer yet, so it can't be switched "
                            "on."), "status_err"
        elif on and needs:
            note, kind = tr("Its download (about {mb} MB) isn't there anymore. Switch it off and on again to get "
                            "it.", mb=dl.size_mb()), "status_warn"
        elif on and plugin.name not in self._running:
            note, kind = tr("It starts after Telescope restarts."), "restart"
        elif not on and plugin.name in self._running:
            note, kind = tr("It goes away after Telescope restarts."), "restart"
        elif needs:
            note = tr("Switching it on downloads about {mb} MB first.", mb=dl.size_mb())
        row, box, _controls = self._row(title, on, tr(plugin.about), note, kind)
        if downloading:
            self._progress_note = next((lbl for lbl in row.findChildren(QLabel) if lbl.text() == note), None)
        box.setEnabled(not (needs and not dl.available()) and not (dl.running() and dl.waiting == plugin.name))
        box.toggled.connect(lambda checked, p=plugin, b=box: self._switch_optional(p, checked, b))
        return row

    def _switch_optional(self, plugin, on: bool, box: QCheckBox):
        title, dl = tr(plugin.title or plugin.name), self._download
        if on and plugin.needs_vision and not dl.ready():
            if not _ask(self, tr("Download about {mb} MB?", mb=dl.size_mb()),
                        tr("{plugin} uses MediaPipe to find people in the picture. It comes as a separate download "
                           "from Telescope's releases on GitHub, which the other optional plugins share.",
                           plugin=title),
                        tr("Download and switch on")):
                with QSignalBlocker(box):
                    box.setChecked(False)
                return
            dl.start(switch_on=plugin.name)
            self.refresh()
            return
        self._save(lambda: plugin_loader.set_optional_on(plugin.name, on))

    def _on_download_progress(self, done: int, size: int):
        if self._progress_note is not None and size:
            self._progress_note.setText(tr("Downloading… {percent}%", percent=min(99, done * 100 // size)))

    def _on_download_done(self, _error: str):
        self.refresh()

    def _remove_pack(self):
        if not _ask(self, tr("Remove the download?"),
                    tr("It frees about {mb} MB. Switching on one of the optional plugins downloads it again.",
                       mb=self._download.size_mb()), tr("Remove")):
            return
        try:
            self._download.remove()
        except OSError as exc:
            QMessageBox.warning(self, tr("Couldn't remove it"), tr("Telescope couldn't delete it: {error}", error=exc))
        self.refresh()

    def _added_row(self, item) -> QWidget:
        title = f"{item.title} {item.version}".strip()
        about = item.description + (" " + tr("By {author}.", author=item.author) if item.author else "")
        note, kind = "", "dim"
        if item.dev:
            return self._dev_row(item, title, about)
        if item.problem:
            note, kind = item.problem, "status_err"
        elif item.changed:
            note, kind = tr("Its files changed since you switched it on, so it's off for now. Switch it on again "
                            "if you trust this version."), "status_warn"
        elif item.name in self._problems and item.enabled:
            note, kind = tr("It didn't start ({problem}).", problem=self._problems[item.name].rstrip('.')), "status_err"
        elif item.enabled and item.name not in self._running:
            note, kind = tr("It starts after Telescope restarts."), "restart"
        elif not item.enabled and item.name in self._running:
            note, kind = tr("It goes away after Telescope restarts."), "restart"
        row, box, controls = self._row(title, item.enabled, about, note, kind)
        box.setEnabled(not item.problem or item.enabled)  # one that's on can always be switched off
        box.toggled.connect(lambda checked, i=item, b=box: self._switch_added(i, checked, b))
        remove = icon_button("trash", tr("Remove {plugin}", plugin=item.title), tr("Remove"))
        remove.clicked.connect(lambda _=False, i=item: self._remove(i))
        controls.insertWidget(0, remove)
        return row

    def _dev_row(self, item, title: str, about: str) -> QWidget:
        """A plugin dev_desktop.py --plugin loads from where it's written: always on, nothing to switch or remove."""
        if item.problem or item.name in self._problems:
            note, kind = tr("It didn't start ({problem}).",
                            problem=(item.problem or self._problems[item.name]).rstrip('.')), "status_err"
        else:
            note, kind = tr("Loaded from {folder} by dev_desktop.py. Telescope restarts by itself when you save one "
                            "of its files.", folder=item.folder), "dim"
        row, _box, _top = self._row(title, True, about, note, kind, locked=True,
                                    tooltip=tr("dev_desktop.py --plugin loads this one, so it's always on"))
        return row

    # ── Switching ─────────────────────────────────────────────────────────

    def _switch_builtin(self, plugin, on: bool, box: QCheckBox):
        title = tr(plugin.title or plugin.name)
        if not on and not _ask(self, tr("Switch off {plugin}?", plugin=title),
                               (tr(plugin.when_off) + "\n\n" if plugin.when_off else "")
                               + tr("Its settings stay saved for when you switch it back on."),
                               tr("Switch off")):
            with QSignalBlocker(box):
                box.setChecked(True)
            return
        self._save(lambda: plugin_loader.set_builtin_off(plugin.name, not on))

    def _switch_added(self, item, on: bool, box: QCheckBox):
        if on:
            ok = _ask(self, tr("Switch on {plugin}?", plugin=item.title),
                      tr("A plugin can do anything on your computer. Only switch it on if you trust its source."),
                      tr("Switch on"), danger=True)
        else:
            ok = _ask(self, tr("Switch off {plugin}?", plugin=item.title),
                      tr("Without it whatever it added to Telescope is gone. Its settings stay saved for when you "
                         "switch it back on."), tr("Switch off"))
        if not ok:
            with QSignalBlocker(box):
                box.setChecked(not on)
            return
        self._save(lambda: plugin_loader.set_enabled(item, on))

    def _all_on(self):
        self._save(plugin_loader.all_builtin_on)

    def _remove(self, item):
        if not _ask(self, tr("Remove {plugin}?", plugin=item.title),
                    tr("Its folder ({folder}) is deleted from the plugins folder. To use it again you'll have to add "
                       "it again.", folder=item.folder.name), tr("Remove"), danger=True):
            return
        try:
            plugin_loader.remove(item)
        except (OSError, ValueError) as exc:
            logger.exception("Couldn't remove plugin %s", item.name)
            QMessageBox.warning(self, tr("Couldn't remove it"), tr("Telescope couldn't delete its folder: {error}", error=exc))
        self.refresh()

    def _save(self, change: Callable[[], None]):
        try:
            change()
        except (OSError, ValueError) as exc:
            logger.exception("Couldn't save a plugin switch")
            QMessageBox.warning(self, tr("Couldn't save that"), tr("Telescope couldn't save the change: {error}", error=exc))
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
        path, _ = QFileDialog.getOpenFileName(self, tr("Install a plugin"), str(Path.home() / "Downloads")
                                              if (Path.home() / "Downloads").is_dir() else str(Path.home()),
                                              tr("Plugins (*.zip)"))
        if path:
            self._install(path)

    def _install(self, path: str):
        try:
            item = plugin_loader.install_zip(Path(path), self._reserved)
        except FileExistsError as exc:
            QMessageBox.warning(self, tr("Already installed"),
                                tr("A plugin called {plugin} is already installed. Remove that one first to put this "
                                   "one in its place.", plugin=exc.args[0]))
            return
        except (OSError, ValueError) as exc:
            QMessageBox.warning(self, tr("Couldn't install it"), str(exc))
            return
        finally:
            self.refresh()
        if item.problem:
            QMessageBox.warning(self, tr("Installed, but it won't load"),
                                tr("{plugin} was installed, but it can't run yet. {problem}",
                                   plugin=item.title, problem=item.problem))
        else:
            QMessageBox.information(self, tr("Installed"),
                                    tr("{plugin} is installed but switched off for now. To use it, switch it on under "
                                       "Added and restart Telescope.", plugin=item.title))

    def _open_folder(self):
        try:
            folder = plugin_loader.make_plugins_dir()
        except OSError as exc:
            QMessageBox.warning(self, tr("Couldn't open it"), tr("Telescope couldn't make its plugins folder: {error}", error=exc))
            return
        QDesktopServices.openUrl(QUrl.fromLocalFile(str(folder)))

    def _restart_now(self):
        if self._is_streaming() and not _ask(self, tr("Restart Telescope now?"),
                                             tr("Streaming stops while Telescope restarts. It takes a few seconds."),
                                             tr("Restart")):
            return
        if not self._restart():
            QMessageBox.warning(self, tr("Couldn't restart"), tr("Telescope couldn't start a new copy of itself. "
                                                                 "Close it and open it again to apply your changes."))
            return
        self.window().close()
