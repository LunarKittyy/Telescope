#!/usr/bin/env python3
# Renders the desktop app offscreen in one language and records which catalog texts each screenshot shows.
# python render_desktop.py LANG OUTDIR

import importlib
import json
import os
import sys
import tempfile
import traceback
from pathlib import Path

REPO = Path(__file__).resolve().parents[3]
LANG, OUT = sys.argv[1], Path(sys.argv[2])
OUT.mkdir(parents=True, exist_ok=True)
os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")
os.environ["XDG_CONFIG_HOME"] = tempfile.mkdtemp()
os.environ["APPDATA"] = os.environ["XDG_CONFIG_HOME"]
sys.path.insert(0, str(REPO / "desktop"))

from PyQt6.QtCore import QPoint, Qt, QTimer  # noqa: E402
from PyQt6.QtWidgets import (QAbstractButton, QApplication, QComboBox, QDialog, QGroupBox, QLabel,  # noqa: E402
                             QLineEdit, QListWidget, QMenu, QMessageBox, QPlainTextEdit, QScrollArea, QTabBar,
                             QTextEdit, QTreeWidget, QWidget)

app = QApplication([])
from telescope import i18n  # noqa: E402

# translated text -> catalog keys it came from
REC: dict = {}
_tr, _trn = i18n.tr, i18n.trn


def tr(text, /, ctx="", **fields):
    out = _tr(text, ctx=ctx, **fields)
    REC.setdefault(str(out), set()).add((ctx, text))
    return out


def trn(singular, plural, n, /, ctx="", **fields):
    out = _trn(singular, plural, n, ctx=ctx, **fields)
    REC.setdefault(str(out), set()).add((ctx, singular))
    return out


i18n.tr, i18n.trn = tr, trn
print("language", i18n.load(LANG))

from telescope.app import TelescopeWindow  # noqa: E402
from telescope.theme import apply_theme  # noqa: E402

apply_theme(app)

SHOTS = []
HOOKED = {"menu": None}


def texts_of(root: QWidget) -> tuple:
    seen, tips = [], []
    widgets = [root] + root.findChildren(QWidget)
    for w in widgets:
        if w is not root and not w.isVisibleTo(root) and not isinstance(w, QMenu):
            continue
        if w.windowTitle():
            seen.append(w.windowTitle())
        if w.toolTip():
            tips.append(w.toolTip())
        if isinstance(w, QLabel):
            seen.append(w.text())
        elif isinstance(w, QAbstractButton):
            seen.append(w.text())
        elif isinstance(w, QGroupBox):
            seen.append(w.title())
        elif isinstance(w, QLineEdit):
            seen += [w.text(), w.placeholderText()]
        elif isinstance(w, (QTextEdit, QPlainTextEdit)):
            seen += [w.toPlainText(), w.placeholderText()]
        elif isinstance(w, QComboBox):
            seen += [w.itemText(i) for i in range(w.count())]
            seen += [str(w.itemData(i, Qt.ItemDataRole.ToolTipRole) or "") for i in range(w.count())]
        elif isinstance(w, QTabBar):
            seen += [w.tabText(i) for i in range(w.count())]
        elif isinstance(w, QListWidget):
            seen += [w.item(i).text() for i in range(w.count())]
        elif isinstance(w, QTreeWidget):
            it = w.invisibleRootItem()
            stack = [it]
            while stack:
                node = stack.pop()
                for c in range(node.childCount()):
                    child = node.child(c)
                    seen += [child.text(k) for k in range(w.columnCount())]
                    stack.append(child)
        if isinstance(w, QMenu):
            for a in w.actions():
                seen.append(a.text())
                if a.toolTip() and a.toolTip() != a.text().replace("&", ""):
                    tips.append(a.toolTip())
                if a.menu():
                    seen += [b.text() for b in a.menu().actions()]
        for a in w.actions():
            if a.toolTip():
                tips.append(a.toolTip())
        # painted widgets keep their texts in attributes
        for v in vars(w).values() if hasattr(w, "__dict__") else ():
            if isinstance(v, str):
                seen.append(v)
            elif isinstance(v, (list, tuple)):
                seen += [x for x in v if isinstance(x, str)]
    return [s for s in seen if s], [t for t in tips if t]


def match(strings: list) -> set:
    blob = "\n\x00".join(s.replace("&&", "\x01").replace("&", "").replace("\x01", "&") for s in strings)
    exact = set(s.replace("&", "") for s in strings)
    found = set()
    for out, keys in REC.items():
        plain = out.replace("&", "")
        if not plain.strip():
            continue
        if plain in exact or (len(plain) >= 4 and plain in blob):
            found |= keys
    return found


def shoot(name: str, title: str, widget: QWidget, scope: QWidget = None, note: str = ""):
    app.processEvents()
    app.processEvents()
    path = OUT / f"{len(SHOTS):02d}-{name}.png"
    widget.grab().save(str(path))
    seen, tips = texts_of(scope or widget)
    shown = match(seen)
    tipkeys = match(tips) - shown
    SHOTS.append({"file": path.name, "title": title, "note": note,
                  "shown": sorted(shown), "tips": sorted(tipkeys)})
    print(f"shot {path.name}: {len(shown)} texts, {len(tipkeys)} tooltips")


def safe(label, fn):
    try:
        fn()
    except Exception:
        print(f"FAILED {label}")
        traceback.print_exc()


# Modal dialogs and menus would block: show them, let the current step shoot them, then close
PENDING = {"name": None, "title": None, "note": ""}


def capture_modal(widget):
    name = PENDING["name"] or type(widget).__name__.lower()
    title = PENDING["title"] or widget.windowTitle() or type(widget).__name__
    widget.show()
    app.processEvents()
    if isinstance(widget, QDialog) and not isinstance(widget, QMessageBox) and widget.width() < 200:
        widget.adjustSize()
    shoot(name, title, widget, note=PENDING["note"])
    PENDING.update(name=None, title=None, note="")


def dialog_exec(self, *a, **k):
    capture_modal(self)
    self.hide()
    return 0


def menu_exec(self, *a, **k):
    self.adjustSize()
    capture_modal(self)
    self.hide()
    return None


QDialog.exec = dialog_exec
QMessageBox.exec = dialog_exec
QMenu.exec = menu_exec
QMenu.popup = lambda self, *a, **k: menu_exec(self)

win = TelescopeWindow()
for path in sorted((REPO / "desktop/telescope/plugins").glob("[!_]*.py")):
    mod = importlib.import_module(f"telescope.plugins.{path.stem}")
    for name, cls in vars(mod).items():
        if name.endswith("Plugin") and isinstance(cls, type) and cls.__module__ == mod.__name__:
            win.register_plugin(cls())
win.apply_saved_config()
win.resize(1280, 860)
win.show()
for _ in range(5):
    app.processEvents()

STEPS = Path(__file__).with_name("desktop_steps.py")
exec(compile(STEPS.read_text(), str(STEPS), "exec"))

# every catalog entry, with where it lives, for the report
entries, _ = i18n.read_po(REPO / f"desktop/telescope/locale/{LANG}.po")
catalog = []
for e in entries:
    if e.obsolete or not e.msgid:
        continue
    catalog.append({"ctx": e.ctx, "msgid": e.msgid, "plural": e.plural, "msgstr": e.msgstr,
                    "comments": e.comments, "locations": e.locations, "fuzzy": e.fuzzy})
(OUT / "desktop.json").write_text(json.dumps({"shots": SHOTS, "catalog": catalog}, ensure_ascii=False, indent=1))
covered = set()
for s in SHOTS:
    covered |= set(map(tuple, s["shown"])) | set(map(tuple, s["tips"]))
print(f"{len(covered)} of {len(catalog)} catalog texts appear in a shot")
os._exit(0)
