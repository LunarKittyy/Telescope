"""The settings window: tabs down the left, one scrolling page of cards for each on the right. The tabs are fixed
(plugin.SETTINGS_TABS) and plugins fill them with SettingsSections; a tab with nothing in it isn't shown."""

import logging
import math

from PyQt6.QtCore import QPointF, QRectF, QSignalBlocker, QSize, Qt, QTimer, pyqtSignal
from PyQt6.QtGui import QColor, QFontMetricsF, QGuiApplication, QIcon, QKeySequence, QPainter, QShortcut
from PyQt6.QtWidgets import (
    QFrame, QHBoxLayout, QLabel, QLineEdit, QListWidget, QListWidgetItem, QScrollArea, QStackedWidget,
    QStyle, QStyledItemDelegate, QStyleOptionViewItem, QVBoxLayout, QWidget,
)

from telescope import theme
from telescope.plugin import SETTINGS_TABS
from telescope.widgets import settings_search
from telescope.widgets.common import PlacedDialog, call_later, elided, ui_px, wrapped_note
from telescope.widgets.controls import SCREEN_MARGIN
from telescope.widgets.line_icons import line_icon
from telescope.i18n import tr

logger = logging.getLogger(__name__)

_NAV_WIDTH = 236
_NAV_ROOM = 167  # of that, what a tab's name gets; a longer name (in some languages) widens the column and the window
_NAV_MAX = 320
_SIZE = (940, 680)
_TAB_ICONS = {
    "general": "sliders", "streaming": "bolt", "video": "video", "audio": "mic", "devices": "phone",
    "shortcuts": "keyboard", "plugins": "plug", "about": "download",
}


class _Page(QScrollArea):
    """One tab: its sections stacked in a scroll area. Tells them when it comes on screen."""

    def __init__(self, sections: list):
        super().__init__()
        self.sections = sections
        self.setWidgetResizable(True)
        self.setHorizontalScrollBarPolicy(Qt.ScrollBarPolicy.ScrollBarAlwaysOff)
        self.setFrameShape(QFrame.Shape.NoFrame)
        content = QWidget()
        lay = QVBoxLayout(content)
        lay.setContentsMargins(0, 0, 8, 0)  # room for the scrollbar beside the cards
        lay.setSpacing(14)
        for section in sections:
            lay.addWidget(section.widget)
        lay.addStretch(1)
        self.setWidget(content)

    def showEvent(self, event):
        super().showEvent(event)
        for section in self.sections:
            if section.on_show is not None:
                try:
                    section.on_show()
                except Exception:
                    logger.exception("A settings section couldn't refresh itself")


def _with_footers(page: _Page, footers: list) -> QWidget:
    """The page as it goes in the stack: on its own, or above the bars its sections keep in reach."""
    if not footers:
        return page
    holder = QWidget()
    lay = QVBoxLayout(holder)
    lay.setContentsMargins(0, 0, 0, 0)
    lay.setSpacing(10)
    lay.addWidget(page, 1)
    for footer in footers:
        row = QHBoxLayout()
        row.setContentsMargins(0, 0, 8, 0)  # lined up with the cards, which leave room for the scrollbar
        row.addWidget(footer)
        lay.addLayout(row)
    return holder


def ordered_tabs(sections: list) -> list:
    """[(id, name, its sections)] for the tabs that have any: the fixed ones in order, new ones before Plugins."""
    names = dict(SETTINGS_TABS)
    extras = []
    for section in sections:
        if section.tab not in names and section.tab not in extras:
            names[section.tab] = section.tab.replace("_", " ").capitalize()
            extras.append(section.tab)
        if section.tab_title:
            names[section.tab] = section.tab_title
    ids = [tab for tab, _name in SETTINGS_TABS]
    at = ids.index("plugins")
    ids[at:at] = extras
    tabs = []
    for tab in ids:
        mine = sorted((s for s in sections if s.tab == tab), key=lambda s: s.order)  # stable: registration order
        if mine:
            tabs.append((tab, tr(names[tab]), mine))
    return tabs


def _tab_icon(tab: str) -> QIcon:
    name = _TAB_ICONS.get(tab, "sliders")
    icon = line_icon(name, theme.TEXT_DIM)
    icon.addPixmap(line_icon(name, "#ffffff").pixmap(16, 16), QIcon.Mode.Selected)  # white on the selected fill
    return icon


_RESULT_ROLE = Qt.ItemDataRole.UserRole
_FLASH_MS = 1800


class _SearchEdit(QLineEdit):
    step = pyqtSignal(int)

    def keyPressEvent(self, event):
        if event.key() == Qt.Key.Key_Escape and self.text():
            self.clear()
            event.accept()
        elif event.key() in (Qt.Key.Key_Down, Qt.Key.Key_Up):
            self.step.emit(1 if event.key() == Qt.Key.Key_Down else -1)
            event.accept()
        else:
            super().keyPressEvent(event)


class _ResultDelegate(QStyledItemDelegate):
    """A result as its name with where it lives under it."""

    def sizeHint(self, option, index):
        return QSize(0, ui_px(52))

    def paint(self, painter, option, index):
        opt = QStyleOptionViewItem(option)
        self.initStyleOption(opt, index)
        name, _, path = str(index.data(Qt.ItemDataRole.DisplayRole)).partition("\n")  # not opt.text: it has the newline turned
        style = opt.widget.style() if opt.widget else QGuiApplication.style()
        style.drawPrimitive(QStyle.PrimitiveElement.PE_PanelItemViewItem, opt, painter, opt.widget)
        selected = bool(opt.state & QStyle.StateFlag.State_Selected)
        painter.save()
        painter.setRenderHint(QPainter.RenderHint.Antialiasing)
        rect = QRectF(opt.rect).adjusted(14, 0, -10, 0)
        painter.setFont(opt.font)
        fm = opt.fontMetrics
        top = rect.top() + (rect.height() - 2 * fm.height() - 2) / 2
        width = int(rect.width())
        painter.setPen(QColor("#ffffff" if selected else theme.TEXT))
        painter.drawText(QPointF(rect.left(), top + fm.ascent()), elided(opt.widget, fm, name, width))
        painter.setPen(QColor("#e6e1ff" if selected else theme.TEXT_DIM))
        painter.drawText(QPointF(rect.left(), top + fm.height() + 2 + fm.ascent()),
                         elided(opt.widget, fm, path, width))
        painter.restore()


class SettingsWindow(PlacedDialog):
    def __init__(self, sections: list, parent=None):
        """sections: every SettingsSection to show; its tab and order say where each goes."""
        super().__init__(parent)
        self.setWindowTitle(tr("Settings"))
        self.setWindowFlag(Qt.WindowType.WindowContextHelpButtonHint, False)
        self.setMinimumSize(ui_px(720), ui_px(480))
        outer = QHBoxLayout(self)
        outer.setContentsMargins(0, 0, 0, 0)
        outer.setSpacing(0)

        side = QFrame()
        side.setObjectName("settings_side")
        self._side = side
        side_lay = QVBoxLayout(side)
        side_lay.setContentsMargins(10, 14, 10, 0)
        side_lay.setSpacing(8)
        self._search = _SearchEdit()
        self._search.setPlaceholderText(tr("Search settings"))
        self._search.setAccessibleName(tr("Search settings"))
        self._search.setClearButtonEnabled(True)
        self._search.addAction(line_icon("search", theme.TEXT_DIM), QLineEdit.ActionPosition.LeadingPosition)
        self._search.textChanged.connect(self._on_query)
        self._search.step.connect(self._step_results)
        self._search.returnPressed.connect(self._open_current)
        side_lay.addWidget(self._search)
        self._nav = QListWidget()
        self._nav.setObjectName("settings_nav")
        self._nav.setIconSize(QSize(16, 16))
        self._nav.setAccessibleName(tr("Settings tabs"))
        self._nav.setFrameShape(QFrame.Shape.NoFrame)
        side_lay.addWidget(self._nav, 1)
        outer.addWidget(side)

        right = QVBoxLayout()
        right.setContentsMargins(24, 18, 16, 16)
        right.setSpacing(12)
        self._title = QLabel()
        self._title.setObjectName("dialog_title")
        right.addWidget(self._title)
        self._stack = QStackedWidget()
        right.addWidget(self._stack, 1)
        outer.addLayout(right, 1)

        self._tabs = ordered_tabs(sections)
        self._ids: list = []
        self._names: list = []
        self._pages: list = []
        for tab, name, mine in self._tabs:
            self._ids.append(tab)
            self._names.append(name)
            page = _Page(mine)
            self._pages.append(page)
            self._stack.addWidget(_with_footers(page, [s.footer for s in mine if s.footer is not None]))
            item = QListWidgetItem(_tab_icon(tab), name)
            item.setSizeHint(QSize(0, ui_px(38)))
            self._nav.addItem(item)
        self._nav.ensurePolished()  # the theme's font comes with the stylesheet
        fm = QFontMetricsF(self._nav.font())
        longest = max((math.ceil(fm.horizontalAdvance(n)) + 1 for n in self._names), default=0)  # Qt elides past a fraction
        side.setFixedWidth(max(ui_px(_NAV_WIDTH), min(ui_px(_NAV_MAX), longest + ui_px(_NAV_WIDTH - _NAV_ROOM))))

        self._results_page = QWidget()
        results_lay = QVBoxLayout(self._results_page)
        results_lay.setContentsMargins(0, 0, 0, 0)
        self._note = wrapped_note(tr("Nothing matches every word. These match some of them."))
        results_lay.addWidget(self._note)
        self._results = QListWidget()
        self._results.setObjectName("settings_results")
        self._results.setAccessibleName(tr("Search results"))
        self._results.setItemDelegate(_ResultDelegate(self._results))
        self._results.setFrameShape(QFrame.Shape.NoFrame)
        self._results.itemClicked.connect(self._open_item)
        self._results.itemActivated.connect(self._open_item)
        results_lay.addWidget(self._results, 1)
        self._empty = wrapped_note("")
        results_lay.addWidget(self._empty)
        results_lay.addStretch(1)
        self._stack.addWidget(self._results_page)

        self._query = ""
        self._entries: list = []
        self._hits: list = []
        self._tab_row = 0
        self._flashed = None
        QShortcut(QKeySequence.StandardKey.Find, self, activated=self.focus_search)
        self._nav.currentRowChanged.connect(self._on_row)
        if self._ids:
            self._nav.setCurrentRow(0)

    # ── Tabs ──────────────────────────────────────────────────────────────

    def _show_row(self, row: int):
        self._stack.setCurrentIndex(row)
        self._title.setText(self._names[row])

    def _on_row(self, row: int):
        if not 0 <= row < len(self._ids):
            return
        self._tab_row = row
        if self._query:
            self._search.clear()  # leaving the search shows this tab
        else:
            self._show_row(row)

    def tab_names(self) -> dict:
        """id: the name each tab shows."""
        return dict(zip(self._ids, self._names))

    def tabs(self) -> list:
        """The ids of the tabs shown, top to bottom."""
        return list(self._ids)

    def current_tab(self) -> str:
        return self._ids[self._tab_row] if self._ids else ""

    def show_tab(self, tab: str) -> bool:
        """Go to the tab with this id; False if there isn't one (nothing in it, or its plugin is off)."""
        if tab not in self._ids:
            return False
        if self._query:
            self._search.clear()
        self._nav.setCurrentRow(self._ids.index(tab))
        return True

    # ── Search ────────────────────────────────────────────────────────────

    def focus_search(self):
        self._search.setFocus()
        self._search.selectAll()

    def search_results(self, query: str) -> list:
        """The settings that fit the query as they are now, best first."""
        return settings_search.search(settings_search.collect(self._tabs), query)

    def _on_query(self, text: str):
        text = text.strip()
        if not text:
            self._leave_search()
            return
        if not self._query:
            self._entries = settings_search.collect(self._tabs)  # read fresh: rows come and go
            with QSignalBlocker(self._nav):
                self._nav.setCurrentRow(-1)
        self._query = text
        self._hits, partial = settings_search.search_ex(self._entries, text)
        self._note.setVisible(partial and bool(self._hits))
        self._results.clear()
        for i, entry in enumerate(self._hits):
            item = QListWidgetItem(f"{entry.name}\n{entry.path}")
            item.setData(_RESULT_ROLE, i)
            self._results.addItem(item)
        found = bool(self._hits)
        self._results.setVisible(found)
        self._empty.setVisible(not found)
        if found:
            self._results.setCurrentRow(0)
        else:
            self._empty.setText(tr("Nothing matches “{text}”. Try fewer words, or say what it does "
                                   "(resolution, microphone, startup).", text=text))
        self._title.setText(tr("Search results"))
        self._stack.setCurrentWidget(self._results_page)

    def _leave_search(self):
        self._query = ""
        self._entries = []
        self._hits = []
        if self._ids:
            with QSignalBlocker(self._nav):
                self._nav.setCurrentRow(self._tab_row)
            self._show_row(self._tab_row)

    def _step_results(self, delta: int):
        if self._hits:
            row = min(max(self._results.currentRow() + delta, 0), len(self._hits) - 1)
            self._results.setCurrentRow(row)

    def _open_current(self):
        if self._hits:
            self.go_to(self._hits[max(self._results.currentRow(), 0)])

    def _open_item(self, item):
        index = item.data(_RESULT_ROLE)
        if self._query and 0 <= index < len(self._hits):
            self.go_to(self._hits[index])

    def go_to(self, entry):
        """Show the tab the setting is on, scrolled to it and lit up for a moment."""
        self.show_tab(entry.tab)
        if not entry.tab_entry:
            QTimer.singleShot(0, lambda: self._reveal(entry))

    def _reveal(self, entry):
        try:
            page = self._pages[self._ids.index(entry.tab)]
            page.widget().layout().activate()
            page.ensureWidgetVisible(entry.target, 0, 40)
            self._flash(entry.target)
        except (RuntimeError, ValueError):
            pass  # the row went away meanwhile

    def _flash(self, target: QWidget):
        node = target
        while node is not None and not (isinstance(node, QFrame) and node.objectName() in ("setting_row", "card")):
            node = node.parentWidget()
        node = node or target
        self._unflash()
        self._flashed = node
        self._mark(node, True)
        call_later(_FLASH_MS, self, lambda: self._unflash(node))

    def _unflash(self, only=None):
        node = self._flashed
        if node is None or (only is not None and node is not only):
            return
        self._flashed = None
        self._mark(node, False)

    @staticmethod
    def _mark(node: QWidget, on: bool):
        try:
            node.setProperty("found", on)
            node.style().unpolish(node)
            node.style().polish(node)
        except RuntimeError:
            pass  # gone already

    def fit_before_show(self):
        self._fit_to_screen()

    def showEvent(self, event):
        super().showEvent(event)
        self.focus_search()

    def default_size(self) -> QSize:
        """The size it opens at, before a small screen gets a say: wider by what the tab column grew."""
        return QSize(ui_px(_SIZE[0]) + self._side.width() - ui_px(_NAV_WIDTH), ui_px(_SIZE[1]))

    def _fit_to_screen(self):
        w, h = self.default_size().width(), self.default_size().height()
        screen = self.screen() or QGuiApplication.primaryScreen()
        if screen is not None:
            area = screen.availableGeometry()
            w, h = min(w, area.width() - SCREEN_MARGIN), min(h, area.height() - SCREEN_MARGIN)
            self.setMinimumSize(min(self.minimumWidth(), w), min(self.minimumHeight(), h))  # a small screen wins
        self.resize(w, h)

    def keyPressEvent(self, event):
        # Enter belongs to the focused control. QDialog would also click a default button after a search opens a tab.
        if event.key() in (Qt.Key.Key_Return, Qt.Key.Key_Enter):
            event.accept()
        else:
            super().keyPressEvent(event)
