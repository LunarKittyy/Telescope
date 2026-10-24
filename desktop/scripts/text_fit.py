#!/usr/bin/env python3
"""Finds texts that don't fit where they're shown: cut off, cut short with "...", or pushed out of their box.

Builds the main window at the narrowest width of each of its layouts and opens every settings tab, offscreen, then
measures every text on them. tests/test_text_fit.py runs it for each language and fails on anything new; what's
already known is in tests/fixtures/text_fit_known.txt. In the pseudo language it also lists English that never went
through tr(). Fonts differ between machines, so a text that only just fits can show up on one and not on another.

  python scripts/text_fit.py                 every language and the pseudo one
  python scripts/text_fit.py sv pseudo       just these
  python scripts/text_fit.py --shots DIR     also save each window with what doesn't fit boxed in red
  python scripts/text_fit.py --known         print the findings as lines for tests/fixtures/text_fit_known.txt
"""

import argparse
import contextlib
import os
import re
import sys
import tempfile
from dataclasses import dataclass, field
from pathlib import Path
from unittest import mock

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT))

from telescope.i18n import PSEUDO_LANGUAGE as PSEUDO  # noqa: E402

KNOWN = ROOT / "tests" / "fixtures" / "text_fit_known.txt"
SLACK = 2  # px a text may overrun before it counts: what antialiasing and rounding differ by
MAIN_HEIGHT = 900
MAX_HEIGHT = 8000  # windows are made as tall as their content, so nothing hides below a scroll


@dataclass
class Finding:
    lang: str
    view: str      # "main window 560 px", "Settings > General"
    kind: str      # "cut off", "cut short", "out of its box", "window too wide", "not translated"
    text: str      # as shown
    english: str   # the text in the code, "" for one that isn't a tr() text
    need: int = 0
    room: int = 0
    rect: tuple = field(default=(), repr=False)  # where it is in the window, for --shots

    @property
    def key(self) -> str:
        """The line in text_fit_known.txt that marks it known; the same wherever it shows."""
        return f"{self.lang}\t{self.kind}\t{self.english or self.text}".replace("\n", "\\n")

    @property
    def size(self) -> str:
        return f"needs {self.need} px, has {self.room}" if self.need else ""


def report(findings: list, old: set) -> list:
    """The findings as lines to read, one per text and problem with every place it shows, new ones marked."""
    places = {}
    for f in findings:
        places.setdefault(f.key, (f, []))[1].append(f.view + (f" ({f.size})" if f.size else ""))
    lines = []
    for key, (f, where) in places.items():
        what = repr(f.text) + (f" ({f.english!r})" if f.english and f.english != f.text else "")
        lines.append(f"{'  ' if key in old else 'NEW '}{f.lang:7} {f.kind}: {what}")
        lines += [f"            {place}" for place in dict.fromkeys(where)]
    return lines


def known() -> set:
    try:
        lines = KNOWN.read_text(encoding="utf-8").splitlines()
    except OSError:
        return set()
    return {line for line in lines if line.strip() and not line.startswith("#")}


# ── Which shown texts are translated ones ────────────────────────────────────

class _Texts:
    """Tells a shown text's English, for the texts of the loaded language (a filled-in {field} matches anything)."""

    def __init__(self):
        from telescope import i18n
        entries, _header = i18n.read_po(i18n.LOCALE_DIR / "telescope.pot")
        self.exact, self.patterns = {}, []
        for e in entries:
            if e.ctx == "search":
                continue
            forms = i18n._catalog.get(e.key) or []
            for shown in [f for f in forms if f] or [e.msgid] + ([e.plural] if e.plural else []):
                if i18n.fields(shown):
                    if len(re.sub(r"\W", "", i18n.FIELD.sub("", shown))) < 5:
                        continue  # mostly fields ("{items} and {last}"): would match a phone's name
                    pattern = "".join(".+?" if i % 2 else re.escape(part)
                                      for i, part in enumerate(i18n.FIELD.split(shown)) if not i % 2 or part)
                    self.patterns.append((re.compile(pattern, re.S), e.msgid))
                else:
                    self.exact.setdefault(shown, e.msgid)

    def english(self, text: str) -> str:
        if text in self.exact:
            return self.exact[text]
        for pattern, msgid in self.patterns:
            if pattern.fullmatch(text):
                return msgid
        return ""


_BRACKETED = re.compile(r"\[[^\[\]]*\]")
_WORD = re.compile(r"[A-Za-z]{3,}")


NAMES = {"Telescope", "Telescope.apk"}  # shown as they are in every language


def _untranslated(text: str) -> bool:
    """In the pseudo language, a shown text with English left once every translated part is taken out."""
    from telescope.version import display_version
    if text in NAMES or text == display_version():
        return False
    while _BRACKETED.search(text):
        text = _BRACKETED.sub(" ", text)
    return bool(_WORD.search(re.sub(r"<[^<>]*>|&[a-z]+;", " ", text)))


# ── Measuring ────────────────────────────────────────────────────────────────

def _paints_itself(w) -> bool:
    """A widget of ours that draws its own text; those cut it with elided(), which says so."""
    for cls in type(w).__mro__:
        if cls.__module__.startswith("PyQt6"):
            return False
        if "paintEvent" in vars(cls):
            return True
    return False


def _shown_text(w) -> str:
    from PyQt6.QtWidgets import QAbstractButton, QComboBox, QLabel, QLineEdit
    if isinstance(w, QLabel):
        return w.fullText() if hasattr(w, "fullText") else w.text()
    if isinstance(w, QAbstractButton):
        return w.text()
    if isinstance(w, QComboBox):
        return w.currentText() or w.placeholderText()
    if isinstance(w, QLineEdit):
        return w.text() or w.placeholderText()
    return ""


def _measure(w):
    """(kind, need, room) when w's own text doesn't fit it, else None."""
    from PyQt6.QtGui import QTextDocument
    from PyQt6.QtWidgets import QAbstractButton, QAbstractSpinBox, QComboBox, QLabel, QLineEdit, QStyle, \
        QStyleOptionComboBox, QStyleOptionFrame
    if isinstance(w, QLabel):
        if hasattr(w, "fullText"):  # ElidingLabel
            return ("cut short", w.fontMetrics().horizontalAdvance(w.fullText()), w.width()) \
                if w.text() != w.fullText() else None
        if not w.text().strip() or _paints_itself(w):
            return None
        room = w.contentsRect()
        if w.wordWrap():
            need = w.heightForWidth(w.width())
            if need > w.height() + SLACK:
                return "cut off", need, w.height()
            doc = QTextDocument()
            doc.setDefaultFont(w.font())
            if _rich(w):
                doc.setHtml(w.text())
            else:
                doc.setPlainText(w.text())
            longest = max((w.fontMetrics().horizontalAdvance(word) for word in doc.toPlainText().split()), default=0)
            if longest > room.width() + SLACK:  # one word longer than the line can't wrap
                return "cut off", longest, room.width()
            return None
        hint = w.sizeHint()
        if hint.width() > w.width() + SLACK:
            return "cut off", hint.width(), w.width()
        if hint.height() > w.height() + SLACK:
            return "cut off", hint.height(), w.height()
        return None
    if isinstance(w, QAbstractButton):
        if not w.text() or _paints_itself(w):
            return None
        need = w.minimumSizeHint().width() if w.property("segmented") is True else w.sizeHint().width()
        return ("cut off", need, w.width()) if need > w.width() + SLACK else None
    if isinstance(w, QComboBox):
        text = w.currentText() or w.placeholderText()
        if w.isEditable() or not text or _paints_itself(w):
            return None
        opt = QStyleOptionComboBox()
        opt.initFrom(w)  # not initStyleOption(): Qt made some combo boxes itself, and those don't let Python call it
        opt.currentText, opt.currentIcon, opt.editable, opt.frame = text, w.itemIcon(w.currentIndex()), False, w.hasFrame()
        room = w.style().subControlRect(QStyle.ComplexControl.CC_ComboBox, opt,
                                        QStyle.SubControl.SC_ComboBoxEditField, w).width()
        need = w.fontMetrics().horizontalAdvance(text) + (0 if opt.currentIcon.isNull() else w.iconSize().width() + 4)
        return ("cut short", need, room) if need > room + SLACK else None
    if isinstance(w, QLineEdit):
        text = w.text() or w.placeholderText()
        if not text or isinstance(w.parentWidget(), QAbstractSpinBox):
            return None
        opt = QStyleOptionFrame()
        opt.initFrom(w)
        opt.lineWidth = w.style().pixelMetric(QStyle.PixelMetric.PM_DefaultFrameWidth, opt, w) if w.hasFrame() else 0
        room = w.style().subElementRect(QStyle.SubElement.SE_LineEditContents, opt, w).width()
        margins = w.textMargins()
        room -= margins.left() + margins.right() + 4  # QLineEdit's own 2 px each side
        need = w.fontMetrics().horizontalAdvance(text)
        return ("cut off", need, room) if need > room + SLACK else None
    return None


def _rich(label) -> bool:
    from PyQt6.QtCore import Qt
    fmt = label.textFormat()
    return fmt == Qt.TextFormat.RichText or (fmt == Qt.TextFormat.AutoText and Qt.mightBeRichText(label.text()))


def _list_items(view, top):
    """(item, need, room, rect in top) for each item of a list whose text the list cuts short."""
    from PyQt6.QtCore import QRect
    from PyQt6.QtWidgets import QStyle, QStyleOptionViewItem
    for row in range(view.count()):
        item = view.item(row)
        if item.isHidden() or not item.text():
            continue
        opt = QStyleOptionViewItem()
        opt.initFrom(view)
        opt.rect, opt.text, opt.font = view.visualItemRect(item), item.text(), view.font()
        opt.features = QStyleOptionViewItem.ViewItemFeature.HasDisplay
        if not item.icon().isNull():
            opt.features |= QStyleOptionViewItem.ViewItemFeature.HasDecoration
            opt.decorationSize, opt.icon = view.iconSize(), item.icon()
        room = view.style().subElementRect(QStyle.SubElement.SE_ItemViewItemText, opt, view).width()
        need = view.fontMetrics().horizontalAdvance(item.text())
        if need > room + SLACK:
            at = view.viewport().mapTo(top, opt.rect.topLeft())
            yield item, need, room, QRect(at, opt.rect.size())


def _rect_in(w, top):
    from PyQt6.QtCore import QPoint, QRect
    return QRect(w.mapTo(top, QPoint(0, 0)), w.size())


def _out_of_box(w, top):
    """(need, room) when w sticks out of a widget it's inside, so part of it isn't drawn."""
    from PyQt6.QtCore import Qt
    from PyQt6.QtWidgets import QAbstractScrollArea
    r = _rect_in(w, top)
    p = w.parentWidget()
    while p is not None:
        pr = _rect_in(p, top)
        area = p.parentWidget()
        scrolls = isinstance(area, QAbstractScrollArea) and area.viewport() is p
        over_x = max(pr.left() - r.left(), r.right() - pr.right())
        if scrolls and area.horizontalScrollBarPolicy() != Qt.ScrollBarPolicy.ScrollBarAlwaysOff:
            over_x = 0
        over_y = 0 if scrolls else max(pr.top() - r.top(), r.bottom() - pr.bottom())
        if over_x > SLACK:
            return r.width(), r.width() - over_x
        if over_y > SLACK:
            return r.height(), r.height() - over_y
        if p is top:
            break
        p = p.parentWidget()
    return None


def _grow_to_content(top, width: int, height: int):
    """top at width, as tall as its scrolling parts need so every text is laid out and drawn."""
    from PyQt6.QtWidgets import QApplication, QScrollArea
    top.resize(width, height)
    QApplication.processEvents()
    for _ in range(6):  # a taller window can change what a scroll area needs
        extra = 0
        for area in top.findChildren(QScrollArea):
            if area.isVisible() and area.widget() is not None:
                content = area.widget()
                need = max(content.sizeHint().height(), content.minimumSizeHint().height(), content.height())
                extra = max(extra, need - area.viewport().height())
        if extra <= 0:
            break
        top.resize(width, min(MAX_HEIGHT, top.height() + extra))
        QApplication.processEvents()


def check(top, lang: str, view: str, texts: _Texts, width: int = 0) -> list:
    """Everything on top whose text doesn't fit, as it's laid out now."""
    from PyQt6.QtWidgets import QAbstractButton, QComboBox, QLabel, QLineEdit, QListWidget, QStyledItemDelegate, QWidget
    from telescope.widgets import common
    found = []

    def add(w, kind, text, need=0, room=0, rect=None):
        r = _rect_in(w, top) if rect is None else rect
        found.append(Finding(lang, view, kind, text, texts.english(text), need, room,
                             (r.x(), r.y(), r.width(), r.height())))

    if width and top.width() > width + SLACK:
        add(top, "window too wide", f"{width} px", top.width(), width)
    common.elisions = []
    try:
        top.grab()  # paints everything, so what draws its own text says what it cut
        cut = common.elisions
    finally:
        common.elisions = None
    seen = set()
    for w, text, need, room in cut:
        if isinstance(w, QWidget) and w.isVisible() and (id(w), text) not in seen and texts.english(text):
            seen.add((id(w), text))
            add(w, "cut short", text, need, room)
    for listed in top.findChildren(QListWidget):
        if listed.isVisible() and type(listed.itemDelegate()) is QStyledItemDelegate:
            for item, need, room, rect in _list_items(listed, top):
                add(listed, "cut short", item.text(), need, room, rect)
    for w in top.findChildren(QWidget):
        if not isinstance(w, (QLabel, QAbstractButton, QComboBox, QLineEdit)) or not w.isVisible():
            continue
        text = _shown_text(w)
        if not text.strip():
            continue
        typed = isinstance(w, QLineEdit) and w.text()  # what someone typed isn't the app's to translate
        if lang == PSEUDO and not typed and _untranslated(text):
            add(w, "not translated", text)
        measured = _measure(w)
        if measured:
            if measured[0] == "cut short" and not texts.english(text):
                continue  # a phone's name or a path, cut on purpose
            add(w, measured[0], text, measured[1], measured[2])
        elif out := _out_of_box(w, top):
            add(w, "out of its box", text, *out)
    return found


# ── The windows ──────────────────────────────────────────────────────────────

class _Held:
    """Stands in for what reaches the network or the system: does nothing."""

    def __init__(self, *_args, **_kwargs):
        pass

    def __getattr__(self, _name):
        return lambda *_a, **_k: None

    def lookup(self, _pid):
        return []


class _MicBackend:
    pick_name = "Microphone"

    def __getattr__(self, _name):
        return lambda *_a, **_k: None

    def problem(self):
        return None

    def prepare(self):
        return ""


@contextlib.contextmanager
def _quiet_system():
    """The window as on a machine with no phone, no virtual camera set up and nothing to check online."""
    import telescope.app as app_module
    import telescope.plugins.connection as connection
    import telescope.plugins.setup as setup
    from telescope.plugins.onboarding import OnboardingPlugin
    from telescope.plugins.updates import UpdatesPlugin
    with contextlib.ExitStack() as stack:
        for target, name, value in (
            (app_module.QSystemTrayIcon, "isSystemTrayAvailable", lambda: False),
            (app_module, "save_config", lambda _cfg: True),  # each language starts from the same settings
            (connection, "LanDiscovery", _Held),
            (connection, "stop_adb_server", lambda: None),
            (setup, "uc_registered_name", lambda: None),
            (setup, "uc_in_app_folder", lambda: False),
            (connection.ConnectionPlugin, "_spawn_resolve", lambda self, *a: None),
            (connection.ConnectionPlugin, "ensure_virtual_camera", lambda self, interactive=True: True),
            (OnboardingPlugin, "check_virtual_camera", lambda self: None),
            (UpdatesPlugin, "_maybe_auto_check", lambda self: None),
        ):
            if hasattr(target, name):
                stack.enter_context(mock.patch.object(target, name, value))
        yield


def _main_window():
    from PyQt6.QtWidgets import QApplication
    from telescope.app import TelescopeWindow
    from telescope.plugins import builtin_plugins
    win = TelescopeWindow()
    for plugin in builtin_plugins():
        if plugin.name == "wait_screen":
            plugin._screen, plugin._watch_cls, plugin._extra_screen = _Held(), _Held, _Held
        elif plugin.name == "microphone":
            plugin._backend = _MicBackend()
        elif plugin.name == "shortcuts":
            plugin._backend_factory = lambda: None
        win.register_plugin(plugin)
    win.apply_saved_config()
    QApplication.processEvents()
    return win


def _close(win):
    from PyQt6 import sip
    from PyQt6.QtCore import QCoreApplication, QEvent, QTimer
    from PyQt6.QtWidgets import QApplication
    if win._settings_win is not None:
        win._settings_win.close()
    win._shut_down()
    for plugin in win._plugins:
        for value in vars(plugin).values():
            if isinstance(value, QTimer):
                value.stop()
    win._session = None
    win.deleteLater()
    QCoreApplication.sendPostedEvents(None, QEvent.Type.DeferredDelete)
    QApplication.processEvents()
    if not sip.isdeleted(win):
        sip.delete(win)


def _shot(top, findings: list, path: Path):
    from PyQt6.QtGui import QColor, QPainter, QPen
    picture = top.grab()
    p = QPainter(picture)
    p.setPen(QPen(QColor("#ff2d55"), 2))
    ratio = picture.devicePixelRatio()
    for f in findings:
        if f.rect:
            x, y, w, h = f.rect
            p.drawRect(round(x * ratio), round(y * ratio), round(w * ratio) - 1, round(h * ratio) - 1)
    p.end()
    path.parent.mkdir(parents=True, exist_ok=True)
    picture.save(str(path))


_app = None


def run(lang: str, shots=None) -> list:
    """Every text that doesn't fit, in lang, on the main window and the settings tabs."""
    from PyQt6.QtWidgets import QApplication
    from telescope import i18n
    from telescope.theme import apply_theme
    from telescope.widgets.common import ui_px
    from telescope.widgets.settings_window import _SIZE
    import telescope.app as app_module
    global _app
    app = _app = QApplication.instance() or QApplication([])  # kept: it owns what lives as long as the app
    apply_theme(app)
    i18n.load(lang)
    texts = _Texts()
    found = []
    try:
        with _quiet_system():
            win = _main_window()
            try:
                win.show()
                for width in (app_module._WIDTH_THREE_COL, app_module._WIDTH_TWO_COL, 560):
                    width = ui_px(width)
                    _grow_to_content(win, width, ui_px(MAIN_HEIGHT))
                    view = f"main window {width} px"
                    here = check(win, lang, view, texts, width)
                    found += here
                    if shots and here:
                        _shot(win, here, Path(shots) / f"{lang}-main-{width}.png")
                win.resize(ui_px(1380), ui_px(MAIN_HEIGHT))
                win.open_settings()
                settings = win._settings_win
                for tab, title in settings.tab_names().items():
                    settings.show_tab(tab)
                    _grow_to_content(settings, ui_px(_SIZE[0]), ui_px(_SIZE[1]))
                    view = f"Settings > {i18n.english(title)}"
                    here = check(settings, lang, view, texts)
                    found += here
                    if shots and here:
                        _shot(settings, here, Path(shots) / f"{lang}-settings-{tab}.png")
            finally:
                _close(win)
    finally:
        i18n.load("en")
    return found


def languages() -> list:
    from telescope import i18n
    return [i18n.SOURCE_LANGUAGE, *i18n.available(), i18n.PSEUDO_LANGUAGE]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("langs", nargs="*", help="languages to check (default: all, and pseudo)")
    parser.add_argument("--shots", metavar="DIR", help="save each window with a finding, boxed in red")
    parser.add_argument("--known", action="store_true", help="print the findings as text_fit_known.txt lines")
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="telescope-text-fit-") as home:
        os.environ["XDG_CONFIG_HOME"] = os.path.join(home, "xdg")  # a fresh config, not yours
        os.environ["APPDATA"] = os.path.join(home, "appdata")
        old = known()
        findings = [f for lang in args.langs or languages() for f in run(lang, args.shots)]
    if args.known:
        print("\n".join(sorted({f.key for f in findings})))
        return 0
    print("\n".join(report(findings, old)))
    new = {f.key for f in findings} - old
    print(f"\n{len({f.key for f in findings})} found, {len(new)} not in {KNOWN.relative_to(ROOT).as_posix()}"
          + (f", pictures in {args.shots}" if args.shots else ""))
    return 1 if new else 0


if __name__ == "__main__":
    sys.exit(main())
