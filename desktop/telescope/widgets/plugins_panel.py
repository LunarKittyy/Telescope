"""The Plugins tab of Settings: every plugin, built in or added, as a tile with its switch, grouped by what it's for."""

import html
import logging
from pathlib import Path
from typing import Callable, Optional

from PyQt6.QtCore import QRect, QSignalBlocker, QSize, Qt, QUrl
from PyQt6.QtGui import QColor, QDesktopServices, QTextLayout
from PyQt6.QtWidgets import (
    QApplication, QButtonGroup, QCheckBox, QFileDialog, QFrame, QHBoxLayout, QLabel, QLayout, QLineEdit, QMessageBox,
    QPushButton, QScrollArea, QSizePolicy, QVBoxLayout, QWidget,
)

from telescope import plugin_loader, theme
from telescope.plugin import PLUGIN_CATEGORIES
from telescope.widgets import vision_download
from telescope.widgets.line_icons import LINE_ICONS, line_icon
from telescope.widgets.plugin_updater import AUTO, CHECK, PluginUpdater
from telescope.widgets.common import (
    FlowLayout, SegmentButton, Switch, WrapLabel,
    card_action, create_vector_icon, icon_button, segmented_row, set_ui_role, ui_px, wrapped_note,
)
from telescope.widgets.controls import setting_row
from telescope.widgets.settings_search import keywords
from telescope.i18n import tr, trn

logger = logging.getLogger(__name__)

DOCS_URL = "https://github.com/LunarKittyy/Telescope/blob/master/docs/plugins.md"

ALL, ON, OFF = "all", "on", "off"
"""What the filter above the tiles shows: every plugin, the ones switched on, or the ones switched off."""

CATEGORY_LOOK = {
    "camera": "camera",
    "look": "colour",
    "vision": "skeleton",
    "preview": "eye",
    "on_screen": "image",
    "sound": "mic",
    "streaming": "stream",
    "extras": "plug",
}
"""Each group's icon on its heading; its colour is theme.hue(group id), which its tiles take too."""

TILE_MIN = 230
"""The narrowest a tile gets before the grid drops to fewer columns."""
COLUMNS_MAX = 3
ABOUT_LINES = 2
"""How many lines of its description a tile shows; the rest is in its tooltip."""


def category_of(value: str) -> str:
    """The group a plugin goes in: its category if the Plugins tab has that one, else Extras."""
    return value if value in dict(PLUGIN_CATEGORIES) else "extras"


def _ask(parent, title: str, text: str, yes: str, danger: bool = False) -> bool:
    box = QMessageBox(QMessageBox.Icon.Warning if danger else QMessageBox.Icon.Question, title, text, parent=parent)
    yes_btn = box.addButton(yes, QMessageBox.ButtonRole.AcceptRole)
    cancel = box.addButton(tr("Cancel"), QMessageBox.ButtonRole.RejectRole)
    box.setDefaultButton(cancel)
    box.exec()
    return box.clickedButton() is yes_btn


def _icon(name: str, color: str):
    return line_icon(name, color) if name in LINE_ICONS else create_vector_icon(name, color)


def _chip(icon_name: str, hue: str, lit: bool = True, size: int = 28) -> QLabel:
    """An icon on a tinted square, as on a card's header; a plugin that's off gets a grey one."""
    chip = QLabel()
    chip.setFixedSize(size, size)
    chip.setAlignment(Qt.AlignmentFlag.AlignCenter)
    color = theme.hue(hue) if lit else theme.TEXT_FAINT
    tint = QColor(color)
    tint.setAlphaF(0.14 if lit else 0.10)
    chip.setStyleSheet(f"background: {tint.name(QColor.NameFormat.HexArgb)}; border-radius: {size // 3}px;")
    chip.setPixmap(_icon(icon_name, color).pixmap(16, 16))
    return chip


class _Clamped(QLabel):
    """A description cut to ABOUT_LINES lines, ending in … when it's longer (the whole of it is the tooltip then)."""

    def __init__(self, text: str):
        super().__init__()
        self._full = text
        self.setObjectName("setting_sub")
        self.setProperty("sub", True)
        self.setWordWrap(True)
        self.setAlignment(Qt.AlignmentFlag.AlignLeft | Qt.AlignmentFlag.AlignTop)
        self.setSizePolicy(QSizePolicy.Policy.Ignored, QSizePolicy.Policy.Fixed)
        super().setText(text)

    def full_text(self) -> str:
        return self._full

    def ensure_height(self):
        self.ensurePolished()
        self.setFixedHeight(self.fontMetrics().lineSpacing() * ABOUT_LINES + 2)

    def resizeEvent(self, event):
        super().resizeEvent(event)
        self._fit()

    def _fit(self):
        width = self.contentsRect().width()
        if width <= 1:
            return
        fm = self.fontMetrics()
        layout = QTextLayout(self._full, self.font())
        layout.beginLayout()
        starts = []
        while True:
            line = layout.createLine()
            if not line.isValid():
                break
            line.setLineWidth(width)
            starts.append(line.textStart())
        layout.endLayout()
        if len(starts) <= ABOUT_LINES:
            shown = self._full
        else:
            cut = starts[ABOUT_LINES - 1]
            shown = self._full[:cut] + fm.elidedText(self._full[cut:], Qt.TextElideMode.ElideRight, width)
        if shown != self.text():
            super().setText(shown)
        tip = self._full if shown != self._full else ""
        if self.toolTip() != tip:
            self.setToolTip(tip)


class _TileGrid(QLayout):
    """Tiles in equal columns (three when there's room, else two, one only when very narrow), all as tall as the
    tallest. Hidden tiles leave no gap."""

    def __init__(self, parent=None, spacing: int = 10):
        super().__init__(parent)
        self._items: list = []
        self._spacing = spacing
        self.setContentsMargins(0, 0, 0, 0)

    def addItem(self, item):        self._items.append(item)
    def count(self):                return len(self._items)
    def itemAt(self, i):            return self._items[i] if 0 <= i < len(self._items) else None
    def takeAt(self, i):            return self._items.pop(i) if 0 <= i < len(self._items) else None
    def expandingDirections(self):  return Qt.Orientation(0)
    def hasHeightForWidth(self):    return True
    def heightForWidth(self, width): return self._place(QRect(0, 0, width, 0), apply=False)
    def sizeHint(self):             return self.minimumSize()

    def minimumSize(self):
        return QSize(ui_px(TILE_MIN) // 2, 0)

    def setGeometry(self, rect):
        super().setGeometry(rect)
        self._place(rect, apply=True)

    def columns(self, width: int) -> int:
        fit = (width + self._spacing) // (ui_px(TILE_MIN) + self._spacing)
        return max(1 if width < ui_px(TILE_MIN) * 2 - ui_px(60) else 2, min(COLUMNS_MAX, fit))

    def _place(self, rect: QRect, apply: bool) -> int:
        shown = [item for item in self._items if not item.isEmpty()]
        if not shown or rect.width() <= 0:
            return 0
        per_row = self.columns(rect.width())
        width = (rect.width() - (per_row - 1) * self._spacing) // per_row
        height = max(max(i.heightForWidth(width) if i.hasHeightForWidth() else i.sizeHint().height(),
                         i.minimumSize().height()) for i in shown)
        if apply:
            for index, item in enumerate(shown):
                row, col = divmod(index, per_row)
                item.setGeometry(QRect(rect.x() + col * (width + self._spacing),
                                       rect.y() + row * (height + self._spacing), width, height))
        rows = -(-len(shown) // per_row)
        return rows * height + (rows - 1) * self._spacing


class _SearchField(QLineEdit):
    def keyPressEvent(self, event):
        if event.key() == Qt.Key.Key_Escape and self.text():
            self.clear()
            event.accept()
        else:
            super().keyPressEvent(event)


class PluginsPanel(QWidget):
    def __init__(self, builtins: list, running: set, problems: dict, restart: Callable[[], bool],
                 is_streaming: Callable[[], bool] = lambda: False, download=None,
                 updater: Optional[PluginUpdater] = None):
        """builtins: every built-in plugin object; running: the names of the plugins running now; problems: why each
        added plugin that's on didn't start this time; restart() starts Telescope again, False if it couldn't.
        self.footer is the restart bar to keep under the tab's scrolling page (SettingsSection.footer)."""
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
        self._updater = updater or PluginUpdater(self._reserved, parent=self)
        self._updater.changed.connect(self.refresh)
        self._filter = ALL
        self._tiles: list = []
        self._groups: list = []
        self._pending: set = set()
        self.setAcceptDrops(True)
        keywords(self, tr("plugins extensions addons add ons modules features turn off on disable enable install zip",
                          ctx="search"))
        body = QVBoxLayout(self)
        body.setContentsMargins(0, 0, 0, 0)
        body.setSpacing(14)
        intro = QHBoxLayout()
        intro.setSpacing(12)
        intro.addWidget(wrapped_note(tr("Switch parts of Telescope on or off. A change takes effect the next time "
                                        "Telescope starts, and settings are kept either way.")), 1)
        self._all_on_btn = card_action(tr("Turn all back on"), "reset", tr("Switch every built-in plugin back on"))
        self._all_on_btn.clicked.connect(self._all_on)
        intro.addWidget(self._all_on_btn, 0, Qt.AlignmentFlag.AlignTop)
        body.addLayout(intro)
        self._keep_at = 0
        self._watching = None

        tools = QHBoxLayout()
        tools.setSpacing(10)
        self._search = _SearchField()
        self._search.setPlaceholderText(tr("Search plugins"))
        self._search.setAccessibleName(tr("Search plugins"))
        self._search.setClearButtonEnabled(True)
        self._search.addAction(line_icon("search", theme.TEXT_DIM), QLineEdit.ActionPosition.LeadingPosition)
        self._search.textChanged.connect(lambda _text: self._apply_filter())
        tools.addWidget(self._search, 1)
        self._filter_btns = {}
        group = QButtonGroup(self)
        group.setExclusive(True)
        for value, text in ((ALL, tr("All", ctx="plugins")), (ON, tr("On", ctx="plugins")),
                            (OFF, tr("Off", ctx="plugins"))):
            btn = SegmentButton(text)
            btn.setAccessibleName(tr("Show: {which}", which=text))
            btn.setChecked(value == ALL)
            btn.clicked.connect(lambda _=False, v=value: self._set_filter(v))
            group.addButton(btn)
            self._filter_btns[value] = btn
        tools.addLayout(segmented_row(*self._filter_btns.values(), fill=False))
        for btn in self._filter_btns.values():
            btn.setFixedWidth(max(ui_px(56), btn.minimumSizeHint().width()))
        body.addLayout(tools)

        self._list = QWidget()
        self._rows = QVBoxLayout(self._list)
        self._rows.setContentsMargins(0, 0, 0, 0)
        self._rows.setSpacing(22)
        body.addWidget(self._list)

        self._empty = QWidget()
        empty = QVBoxLayout(self._empty)
        empty.setContentsMargins(0, 24, 0, 24)
        empty.setSpacing(10)
        self._empty_note = QLabel()
        self._empty_note.setObjectName("dim")
        self._empty_note.setWordWrap(True)
        self._empty_note.setAlignment(Qt.AlignmentFlag.AlignCenter)
        empty.addWidget(self._empty_note)
        self._show_all_btn = card_action(tr("Show all plugins"), "reset")
        self._show_all_btn.clicked.connect(self.show_all)
        empty.addWidget(self._show_all_btn, 0, Qt.AlignmentFlag.AlignHCenter)
        self._empty.hide()
        body.addWidget(self._empty)

        self._added_box = QWidget()
        added = QVBoxLayout(self._added_box)
        added.setContentsMargins(0, 0, 0, 0)
        added.setSpacing(8)
        self._added_rows = QVBoxLayout()
        self._added_rows.setSpacing(8)
        added.addLayout(self._added_rows)
        self._folder_btn = QPushButton(tr("Open plugins folder"))
        self._folder_btn.clicked.connect(self._open_folder)
        help_btn = QPushButton(tr("How plugins work"))
        help_btn.clicked.connect(lambda: QDesktopServices.openUrl(QUrl(DOCS_URL)))
        links = QHBoxLayout()
        links.setSpacing(8)
        links.addWidget(help_btn)
        links.addWidget(self._folder_btn)
        links.addStretch(1)
        added.addLayout(links)
        body.addWidget(self._added_box)

        self.footer = QFrame()
        self.footer.setObjectName("plugins_footer")
        bar = QHBoxLayout(self.footer)
        bar.setContentsMargins(16, 10, 10, 10)
        bar.setSpacing(12)
        self._footer_note = QLabel()
        self._footer_note.setObjectName("footer_count")
        self._footer_note.setWordWrap(True)
        bar.addWidget(self._footer_note, 1)
        self._restart_btn = QPushButton(tr("Restart now"))
        set_ui_role(self._restart_btn, "primary")
        self._restart_btn.setToolTip(tr("Start Telescope again to apply your changes"))
        self._restart_btn.clicked.connect(self._restart_now)
        bar.addWidget(self._restart_btn)
        self.footer.hide()  # hidden on purpose, so the window placing it doesn't show it before there's a change
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

    @staticmethod
    def _clear(layout):
        while layout.count():
            item = layout.takeAt(0)
            if item.widget() is not None:
                item.widget().hide()  # deleteLater waits for the event loop, and until then it'd paint under the new one
                item.widget().deleteLater()
            elif item.layout() is not None:
                PluginsPanel._clear(item.layout())

    def refresh(self):
        area = self._scroll()
        if area is not None:
            self._keep_at = area.verticalScrollBar().value()  # stay where the person was once the tiles are back
        focused = QApplication.focusWidget()
        if focused is not None and self.isAncestorOf(focused):
            self.setFocus()  # a hidden switch hands focus to the next button down, and the page scrolls after it
        self._clear(self._rows)
        self._clear(self._added_rows)
        self._tiles, self._groups, self._pending = [], [], set()
        self._progress_note = None
        off = self._read(plugin_loader.builtin_off, set())
        on = self._read(plugin_loader.optional_on, set())
        self._installed = self._read(lambda: plugin_loader.find_installed(self._reserved), [])

        self._all_on_btn.setEnabled(any(p.name in off for p in self._builtins if not p.required and not p.optional))
        tiles = {cid: [] for cid, _name in PLUGIN_CATEGORIES}
        for plugin in self._builtins:
            cid = category_of(plugin.category)
            if plugin.optional:
                tile = self._optional_tile(plugin, plugin.name in on, cid)
            else:
                tile = self._builtin_tile(plugin, plugin.required or plugin.name not in off, cid)
            tiles[cid].append(tile)
        for item in self._installed:
            cid = category_of(item.category)
            tiles[cid].append(self._dev_tile(item, cid) if item.dev else self._added_tile(item, cid))
        vision = [category_of(p.category) for p in self._builtins if p.optional and p.needs_vision]
        self._vision_group = "vision" if "vision" in vision else (vision[0] if vision else "")
        self._remove_pack_btn = None
        for cid, name in PLUGIN_CATEGORIES:
            if tiles[cid]:
                self._group(cid, tr(name), sorted(tiles[cid], key=lambda t: t.sort_key))
        self._fill_added()

        installed = {i.name for i in self._installed}
        self._pending |= {n for n in self._running if n not in self._reserved and n not in installed}  # removed
        n = len(self._pending)
        self._footer_note.setText(trn("Restart to apply {n} change", "Restart to apply {n} changes", n, n=n))
        if self.footer.parentWidget() is not None:  # a widget with no parent would come up as a window of its own
            self.footer.setVisible(bool(n))
        self._apply_filter()

    def pending_changes(self) -> int:
        """How many switches, installs and removals wait for Telescope to start again."""
        return len(self._pending)

    def _group(self, cid: str, name: str, tiles: list):
        """A group of tiles under its heading, which carries the group's buttons."""
        icon, hue = CATEGORY_LOOK.get(cid, "plug"), self._hue(cid)
        frame = QFrame()
        frame.setObjectName("card")  # what the settings search lists it as; flat, the tiles are the cards
        frame.setProperty("flat", True)
        keywords(frame, tr("plugins extensions addons add ons modules features turn off on disable enable install zip",
                           ctx="search"))
        lay = QVBoxLayout(frame)
        lay.setContentsMargins(0, 0, 0, 0)
        lay.setSpacing(10)
        head = QHBoxLayout()
        head.setSpacing(10)
        head.addWidget(_chip(icon, hue, size=26))
        title = QLabel(name)
        title.setObjectName("card_title")
        head.addWidget(title)
        count = QLabel()
        count.setObjectName("card_subtitle")
        head.addWidget(count)
        head.addStretch(1)
        if cid == self._vision_group:  # the vision download's own controls, where the plugins that use it are
            self._vision_head(head)
        lay.addLayout(head)
        if cid == self._vision_group:
            status = self._vision_status()
            if status:
                lay.addWidget(wrapped_note(status))
        holder = QWidget()
        grid = _TileGrid(holder, spacing=10)
        for tile in tiles:
            grid.addWidget(tile)
        lay.addWidget(holder)
        self._rows.addWidget(frame)
        self._groups.append((frame, tiles, count))

    def _tile(self, title: str, about: str, icon: str, hue: str, on: bool, *, locked: bool = False,
              tooltip: str = "", note: str = "", note_kind: str = "dim", badges: tuple = (), byline: str = "",
              rich_note: bool = False, pending: str = "") -> tuple:
        """A plugin as a tile: its icon, name and switch on top, what it does under them, then anything it has to say
        and its badges. A locked switch stays on. pending: its name when it waits on a restart. Returns the tile, its
        switch and the layout of the controls next to the switch."""
        tile = QFrame()
        tile.setObjectName("setting_row")  # the settings search finds and lights it up like any setting
        tile.setProperty("tile", True)
        tile.setProperty("off", not on)
        tile.sort_key = title.casefold()
        tile.on = on
        tile.haystack = f"{title} {about}".casefold()
        outer = QVBoxLayout(tile)
        outer.setContentsMargins(12, 10, 12, 12)
        outer.setSpacing(6)
        top = QHBoxLayout()
        top.setSpacing(10)
        top.addWidget(_chip(icon, hue, lit=on), 0, Qt.AlignmentFlag.AlignTop)
        name = QLabel(title)
        name.setObjectName("setting_name")
        name.setWordWrap(True)
        name.setSizePolicy(QSizePolicy.Policy.Ignored, QSizePolicy.Policy.Preferred)
        top.addWidget(name, 1)
        controls = QHBoxLayout()
        controls.setSpacing(6)
        box = Switch()
        box.setSizePolicy(QSizePolicy.Policy.Fixed, QSizePolicy.Policy.Fixed)
        box.setChecked(on)
        box.setAccessibleName(title)
        if locked:
            box.setEnabled(False)
            box.setProperty("locked", True)
        box.setToolTip(tooltip)
        controls.addWidget(box)
        top.addLayout(controls)
        outer.addLayout(top)
        line = _Clamped(about)
        line.ensure_height()
        outer.addWidget(line)
        if byline:
            by = WrapLabel(byline)
            by.setObjectName("setting_sub")
            by.setProperty("sub", True)
            by.setTextFormat(Qt.TextFormat.RichText)
            by.setOpenExternalLinks(True)
            outer.addWidget(by)
        tile.note = None
        if note:
            label = wrapped_note(note, {"dim": "setting_sub"}.get(note_kind, note_kind))
            label.setProperty("sub", True)
            if rich_note:  # HTML the caller escaped, with links that open in the browser
                label.setTextFormat(Qt.TextFormat.RichText)
                label.setOpenExternalLinks(True)
            outer.addWidget(label)
            tile.note = label
        outer.addStretch(1)
        if pending:
            self._pending.add(pending)
            badges = (*badges, (tr("Waits for a restart"), "restart"))
        if badges:
            holder = QWidget()
            flow = FlowLayout(holder, spacing=4)
            for text, kind in badges:
                badge = QLabel(text)
                badge.setObjectName("plugin_badge")
                badge.setProperty("kind", kind)
                flow.addWidget(badge)
            outer.addWidget(holder)
        tile.setToolTip(tooltip if locked else "")
        self._tiles.append(tile)
        return tile, box, controls

    def _hue(self, cid: str) -> str:
        return cid if cid in CATEGORY_LOOK else "extras"

    def _builtin_tile(self, plugin, on: bool, cid: str) -> QWidget:
        title, about = tr(plugin.title or plugin.name), tr(plugin.about)
        if plugin.required:
            tile, _box, _controls = self._tile(
                title, about, plugin.icon, self._hue(cid), True, locked=True,
                tooltip=tr("Telescope can't work without this one, so it can't be switched off"),
                badges=((tr("Required"), "lock"),))
            return tile
        pending = plugin.name if on != (plugin.name in self._running) else ""
        tile, box, _controls = self._tile(title, about, plugin.icon, self._hue(cid), on, pending=pending,
                                          tooltip=tr(plugin.when_off) if plugin.when_off and on else "")
        box.toggled.connect(lambda checked, p=plugin, b=box: self._switch_builtin(p, checked, b))
        return tile

    def _optional_tile(self, plugin, on: bool, cid: str) -> QWidget:
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
        badges = ((tr("Needs the vision download"), "vision"),) if plugin.needs_vision else ()
        pending = plugin.name if on != (plugin.name in self._running) and not needs else ""
        tile, box, _controls = self._tile(title, tr(plugin.about), plugin.icon, self._hue(cid), on, note=note,
                                          note_kind=kind, badges=badges, pending=pending)
        if downloading:
            self._progress_note = tile.note
        box.setEnabled(not (needs and not dl.available()) and not (dl.running() and dl.waiting == plugin.name))
        box.toggled.connect(lambda checked, p=plugin, b=box: self._switch_optional(p, checked, b))
        return tile

    def _vision_head(self, head: QHBoxLayout):
        """The vision download's button on its group's heading."""
        dl = self._download
        self._remove_pack_btn = card_action(tr("Remove download"), "trash",
                                            tr("Delete the download the plugins here use. Switching one on gets it "
                                               "again."))
        self._remove_pack_btn.clicked.connect(self._remove_pack)
        on = self._read(plugin_loader.optional_on, set())
        self._remove_pack_btn.setVisible(dl.ready() and not dl.running() and not any(
            p.needs_vision and (p.name in on or p.name in self._running) for p in self._builtins if p.optional))
        head.addWidget(self._remove_pack_btn)

    def _vision_status(self) -> str:
        dl = self._download
        if dl.ready() or dl.running():
            return ""
        if not dl.available():
            return tr("There's no vision download for this computer yet.")
        return tr("Switching one of these on downloads about {mb} MB first, once for all of them.", mb=dl.size_mb())

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

    def _added_tile(self, item, cid: str) -> QWidget:
        title = f"{item.title} {item.version}".strip()
        byline = ""
        if item.author or item.repo:
            parts = [tr("By {author}.", author=html.escape(item.author, quote=False))] if item.author else []
            if item.repo:  # the address without https://, as a link
                shown = html.escape(item.repo.split("://", 1)[1].rstrip("/"), quote=False)
                parts.append(f'<a href="{html.escape(item.repo)}">{shown}</a>')
            byline = " ".join(parts)
        note, kind, pending = "", "dim", ""
        release = self._updater.newer(item)
        if item.name in self._updater.updated and item.enabled and not item.changed:
            note, kind = tr("Updated to {version} automatically. The new version runs after Telescope restarts.",
                            version=self._updater.updated[item.name]), "dim"
            pending = item.name
        elif item.problem:
            note, kind = item.problem, "status_err"
        elif item.changed:
            note, kind = tr("Its files changed since you switched it on, so it's off for now. Switch it on again "
                            "if you trust this version."), "status_warn"
        elif item.name in self._problems and item.enabled:
            note, kind = tr("It didn't start ({problem}).", problem=self._problems[item.name].rstrip('.')), "status_err"
            pending = item.name  # a restart tries it again
        elif item.enabled != (item.name in self._running):
            pending = item.name
        linked = not note and release is not None
        if linked:
            # Translators: keep the <a href="{url}"> link
            note, kind = tr('Version {version} is out. <a href="{url}">Download it from its release page</a> and '
                            'install it here to replace this one.', version=html.escape(release.tag, quote=False),
                            url=html.escape(release.page, quote=True)), "status_warn"
        tile, box, controls = self._tile(title, item.description, "plug", self._hue(cid), item.enabled, note=note,
                                         note_kind=kind, badges=((tr("Added"), "added"),), byline=byline,
                                         rich_note=linked, pending=pending)
        tile.haystack += f" {item.author}".casefold()
        box.setEnabled(not item.problem or item.enabled)  # one that's on can always be switched off
        box.toggled.connect(lambda checked, i=item, b=box: self._switch_added(i, checked, b))
        remove = icon_button("trash", tr("Remove {plugin}", plugin=item.title), tr("Remove"))
        remove.clicked.connect(lambda _=False, i=item: self._remove(i))
        controls.insertWidget(0, remove)
        return tile

    def _dev_tile(self, item, cid: str) -> QWidget:
        """A plugin dev_desktop.py --plugin loads from where it's written: always on, nothing to switch or remove."""
        if item.problem or item.name in self._problems:
            note, kind = tr("It didn't start ({problem}).",
                            problem=(item.problem or self._problems[item.name]).rstrip('.')), "status_err"
        else:
            note, kind = tr("Loaded from {folder} by dev_desktop.py. Telescope restarts by itself when you save one "
                            "of its files.", folder=item.folder), "dim"
        tile, _box, _controls = self._tile(f"{item.title} {item.version}".strip(), item.description, "plug",
                                           self._hue(cid), True, locked=True, note=note, note_kind=kind,
                                           tooltip=tr("dev_desktop.py --plugin loads this one, so it's always on"),
                                           badges=((tr("Being written"), "added"),))
        return tile

    def _fill_added(self):
        """Installing plugins and keeping them up to date, under the groups."""
        frame = QFrame()
        frame.setObjectName("card")
        frame.setProperty("flat", True)
        keywords(frame, tr("plugins extensions addons add ons modules features turn off on disable enable install zip",
                           ctx="search"))
        lay = QVBoxLayout(frame)
        lay.setContentsMargins(0, 0, 0, 0)
        lay.setSpacing(8)
        head = QHBoxLayout()
        head.setSpacing(10)
        head.addWidget(_chip("download", "", size=26))
        title = QLabel(tr("Added plugins"))
        title.setObjectName("card_title")
        head.addWidget(title)
        head.addStretch(1)
        self._add_btn = card_action(tr("Install"), "plus", tr("Pick a plugin's .zip file. You can also drop one "
                                                              "onto this window."))
        self._add_btn.clicked.connect(self._add_zip)
        head.addWidget(self._add_btn)
        lay.addLayout(head)
        if not self._installed:
            lay.addWidget(wrapped_note(tr(
                "No plugins added yet. Plugins people share come as .zip files. Click Install and pick one or "
                "drop it onto this window. Added plugins start out switched off.")))
        else:
            lay.addWidget(wrapped_note(tr("Plugins you add are listed with the others, marked Added.")))
        if any(not i.dev for i in self._installed):
            lay.addWidget(self._update_check_row())
            lay.addWidget(self._auto_update_row())
        self._added_rows.addWidget(frame)

    def _row(self, title: str, on: bool, about: str, note: str = "", note_kind: str = "dim") -> tuple:
        """A setting as a strip: its name and what it does, its switch on the right and a note under it."""
        box = Switch()
        box.setChecked(on)
        box.setAccessibleName(title)
        extra = None
        if note:
            extra = wrapped_note(note, {"dim": "setting_sub"}.get(note_kind, note_kind))
            extra.setProperty("sub", True)
        strip, _line = setting_row(title, about, box, extra)
        return strip, box

    def _update_check_row(self) -> QWidget:
        on = self._read(self._updater.checking, True)
        row, box = self._row(
            tr("Check for updates"), on,
            tr("Looks once a day for new versions of added plugins that publish releases on GitHub and mentions them "
               "here. Nothing gets downloaded."))
        box.toggled.connect(lambda checked: self._switch_updates(CHECK, checked))
        return row

    def _auto_update_row(self) -> QWidget:
        """The switch that installs updates without asking: red, and it asks before it goes on."""
        checking = self._read(self._updater.checking, True)
        on = checking and self._read(self._updater.auto, False)
        row, box = self._row(
            tr("Install updates without asking"), on,
            tr("Installs new versions of the plugins you switched on and switches them on again."),
            tr("Dangerous! Whoever controls a plugin's repo can then run new code on this computer every time they "
               "publish a release."), "status_err")
        box.setProperty("danger", True)
        box.setEnabled(checking or on)
        box.toggled.connect(lambda checked, b=box: self._switch_auto(checked, b))
        return row

    def _switch_auto(self, on: bool, box: QCheckBox):
        if on and not _ask(self, tr("Install plugin updates without asking?"),
                           tr("Telescope will install new versions of the plugins you switched on and switch them on "
                              "too, without showing them to you first.\n\nWhoever controls a plugin's repo on GitHub "
                              "(its author, or anyone who gets into their account) can then run new code on this "
                              "computer every time they publish a release. Only switch this on if you trust all of "
                              "them with that."),
                           tr("Switch on"), danger=True):
            with QSignalBlocker(box):
                box.setChecked(False)
            return
        self._switch_updates(AUTO, on)

    def _switch_updates(self, key: str, on: bool):
        self._save(lambda: plugin_loader.set_plugin_switch(key, on))
        if on:
            self._updater.maybe_check()

    # ── Finding one ───────────────────────────────────────────────────────

    def _set_filter(self, value: str):
        self._filter = value
        for key, btn in self._filter_btns.items():
            with QSignalBlocker(btn):
                btn.setChecked(key == value)
        self._apply_filter()

    def show_all(self):
        """Clear the search and the filter."""
        with QSignalBlocker(self._search):
            self._search.clear()
        self._set_filter(ALL)

    def shown_tiles(self) -> list:
        """The names of the tiles the search and the filter leave, group by group, in order."""
        return [t.findChild(QLabel, "setting_name").text() for _f, tiles, _c in self._groups for t in tiles
                if not t.isHidden()]

    def _apply_filter(self):
        query = self._search.text().strip()
        words = query.casefold().split()
        any_shown = False
        for frame, tiles, count in self._groups:
            shown = 0
            for tile in tiles:
                fits = all(w in tile.haystack for w in words) and (self._filter == ALL or tile.on == (self._filter == ON))
                tile.setHidden(not fits)
                shown += fits
            frame.setHidden(not shown)
            any_shown |= bool(shown)
            # Translators: next to a group's name on the Plugins tab, how many of its plugins are switched on
            count.setText(tr("{on} of {total} on", on=sum(t.on for t in tiles), total=len(tiles)))
        narrowed = bool(words) or self._filter != ALL
        self._added_box.setHidden(narrowed)
        self._empty.setHidden(any_shown)
        if not any_shown:
            if words:
                self._empty_note.setText(tr("No plugins match “{text}”.", text=query))
            elif self._filter == OFF:
                self._empty_note.setText(tr("Every plugin is switched on."))
            else:
                self._empty_note.setText(tr("No plugin is switched on."))

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

    def _install(self, path: str, replace: bool = False):
        try:
            item = plugin_loader.install_zip(Path(path), self._reserved, replace=replace)
        except FileExistsError as exc:
            old = next((i for i in self._installed if i.name == exc.args[0] and not i.dev), None)
            title = f"{old.title} {old.version}".strip() if old else exc.args[0]
            if _ask(self, tr("Replace {plugin}?", plugin=title),
                    tr("The version in this .zip takes its place with the same settings. Like any plugin you add it "
                       "starts out switched off."), tr("Replace")):
                self._install(path, replace=True)
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
        elif replace:
            QMessageBox.information(self, tr("Replaced"),
                                    tr("{plugin} {version} is in place but switched off for now. To use it, switch it "
                                       "on again and restart Telescope.",
                                       plugin=item.title, version=item.version))
        else:
            QMessageBox.information(self, tr("Installed"),
                                    tr("{plugin} is installed but switched off for now. To use it, switch it on and "
                                       "restart Telescope.", plugin=item.title))

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
