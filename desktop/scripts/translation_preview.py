#!/usr/bin/env python3
"""Local translation workbench: python scripts/translation_preview.py [--port 8769].

--check compares every scene with its coverage record. --check-all also fails for texts no scene shows. Qt runs in a fresh offscreen subprocess per preview; drafts never change the source catalogs.
"""

import argparse
import base64
import hashlib
import json
import os
import re
import subprocess
import sys
import tempfile
import threading
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT))
from telescope import i18n
from scripts import translation_review as review
from scripts import translation_project as project
from scripts import translation_account as account_service
from scripts import translation_scenes as scenes

ASSETS = Path(__file__).with_name("translation_preview")


key = scenes.key
Tagged = scenes.Tagged
remember = scenes.remember


_catalog_rows = {}


def catalog():
    # Extracting walks the whole source tree, so it's done once per revision and shared with render workers through a file
    current = revision()
    if current not in _catalog_rows:
        cached = Path(tempfile.gettempdir()) / f"telescope-translation-catalog-{os.getuid() if hasattr(os, 'getuid') else 0}-{current[:24]}.json"
        try:
            rows = json.loads(cached.read_text(encoding="utf-8"))
        except (OSError, ValueError):
            entries, problems = i18n.find_all(ROOT)
            if problems:
                raise ValueError("\n".join(problems))
            rows = [[e.msgid, e.plural, e.ctx, e.comments, e.locations, e.flags] for e in entries]
            review.atomic_write(cached, json.dumps(rows, ensure_ascii=False).encode("utf-8"))
        _catalog_rows.clear()
        _catalog_rows[current] = rows
    entries = {}
    for msgid, plural, ctx, comments, locations, flags in _catalog_rows[current]:
        entry = i18n.Entry(msgid, plural, ctx)
        entry.comments, entry.locations, entry.flags = list(comments), list(locations), list(flags)
        entries[key(ctx, msgid)] = entry
    return entries


def revision():
    digest = hashlib.sha256()
    for path in sorted((ROOT / "telescope").rglob("*")):
        if path.suffix in (".py", ".po", ".pot"):
            digest.update(path.relative_to(ROOT).as_posix().encode())
            digest.update(path.read_bytes())
    for path in sorted(review.STATE_DIR.glob("*.json")):
        digest.update(path.name.encode())
        digest.update(path.read_bytes())
    return digest.hexdigest()


def recorded():
    try:
        return json.loads((ASSETS / "coverage.json").read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return {"scenes": {}, "screens": []}


scenes.register_search(catalog().values())
SCENES = {}


def register_scenes(screens):
    """The scenes there are, with the other-texts pages for what the real screens in screens leave out."""
    scenes.register_galleries(catalog().values(), set(screens))
    SCENES.clear()
    SCENES.update({id: scene.title for id, scene in scenes.SCENES.items()})


register_scenes(recorded().get("screens", []))


def validate(payload):
    if not isinstance(payload, dict):
        raise ValueError("Expected an object")
    if "revision" in payload and payload["revision"] != revision():
        raise ValueError("This checkout changed. Reload the editor to get its current texts and previews.")
    lang = payload.get("lang", "es")
    scene = payload.get("scene", "cool")
    if lang not in ["en", *project.language_options()] or scene not in SCENES:
        raise ValueError("Choose an available language and scene")
    drafts = payload.get("drafts", {})
    if not isinstance(drafts, dict) or len(drafts) > 100:
        raise ValueError("Expected at most 100 draft texts")
    entries = catalog()
    for identity, text in drafts.items():
        entry = entries.get(identity)
        if entry is None or entry.plural or not isinstance(text, str) or len(text) > 4000:
            raise ValueError("Unknown text, plural entry or draft longer than 4000 characters")
        if text and i18n.fields(text) != i18n.fields(entry.msgid):
            raise ValueError("Keep every {placeholder} exactly as in the English")
        if text:
            review.validate_text(entry.msgid, text)
            try:
                text.format(**{f: "example" for f in i18n.fields(entry.msgid)})
            except (ValueError, KeyError, IndexError, AttributeError) as exc:
                raise ValueError("The draft has invalid formatting") from exc
    return lang, scene, drafts


def first_text(args):
    return next((a for a in args if isinstance(a, str)), "")


def instrument(stack):
    """Attach translation identities before Qt converts Python strings into QStrings, only in this worker."""
    from unittest.mock import patch
    from PyQt6 import QtGui, QtWidgets as widgets
    original_tr = i18n.tr

    def translate(text, /, ctx="", **values):
        return Tagged(original_tr(text, ctx=ctx, **values), text, ctx, values)

    stack.enter_context(patch.object(i18n, "tr", translate))

    def after(cls, name, handler):
        original = getattr(cls, name)

        def wrapper(target, *args, **kwargs):
            result = original(target, *args, **kwargs)
            handler(target, args, kwargs, result)
            return result

        stack.enter_context(patch.object(cls, name, wrapper))

    def slot_for(slot):
        return lambda target, args, kwargs, _result: remember(target, slot, first_text(args) or kwargs.get("text", ""))

    for cls in (widgets.QLabel, widgets.QPushButton, widgets.QCheckBox, widgets.QRadioButton):
        after(cls, "__init__", slot_for("text"))
    after(widgets.QGroupBox, "__init__", slot_for("title"))

    def box_texts(box, args, _kwargs, _result):
        if args and not isinstance(args[0], widgets.QWidget):
            remember(box, "window title", first_text(args[1:]))
            remember(box, "text", first_text(args[2:]))

    after(widgets.QMessageBox, "__init__", box_texts)
    after(QtGui.QAction, "__init__", slot_for("text"))
    after(widgets.QListWidgetItem, "__init__", slot_for("text"))
    for cls, name, slot in ((widgets.QLabel, "setText", "text"),
                            (widgets.QAbstractButton, "setText", "text"),
                            (widgets.QGroupBox, "setTitle", "title"),
                            (widgets.QWidget, "setToolTip", "tooltip"),
                            (widgets.QWidget, "setWindowTitle", "window title"),
                            (widgets.QLineEdit, "setPlaceholderText", "placeholder"),
                            (widgets.QAbstractSpinBox, "setSpecialValueText", "special"),
                            (widgets.QSpinBox, "setSuffix", "suffix"),
                            (widgets.QDoubleSpinBox, "setSuffix", "suffix"),
                            (widgets.QMessageBox, "setText", "text"),
                            (widgets.QInputDialog, "setLabelText", "label"),
                            (widgets.QMessageBox, "setInformativeText", "detail"),
                            (QtGui.QAction, "setText", "text"),
                            (QtGui.QAction, "setToolTip", "tooltip"),
                            (widgets.QListWidgetItem, "setText", "text")):
        if hasattr(cls, name):
            after(cls, name, slot_for(slot))

    def combo_item(combo, args, _kwargs, _result):
        remember(combo, f"item {combo.count() - 1}", first_text(args))

    after(widgets.QComboBox, "addItem", combo_item)
    after(widgets.QComboBox, "setItemText", lambda combo, args, _k, _r: remember(combo, f"item {args[0]}", args[1]))
    after(widgets.QComboBox, "addItems",
          lambda combo, args, _k, _r: [remember(combo, f"item {combo.count() - len(args[0]) + i}", t) for i, t in enumerate(args[0])])
    after(widgets.QTabWidget, "addTab",
          lambda tabs, args, _k, index: remember(tabs.tabBar(), f"tab {index}", first_text(args[1:])))
    after(widgets.QTabWidget, "setTabText", lambda tabs, args, _k, _r: remember(tabs.tabBar(), f"tab {args[0]}", args[1]))
    after(widgets.QTabBar, "addTab", lambda bar, args, _k, index: remember(bar, f"tab {index}", first_text(args)))
    after(widgets.QTabBar, "setTabText", lambda bar, args, _k, _r: remember(bar, f"tab {args[0]}", args[1]))
    after(widgets.QMenu, "addAction",
          lambda _menu, args, _k, action: remember(action, "text", first_text(args)) if action is not None and first_text(args) else None)
    after(widgets.QMenu, "addMenu",
          lambda _menu, args, _k, sub: remember(sub.menuAction(), "text", first_text(args)) if sub is not None and first_text(args) else None)
    after(widgets.QListWidget, "addItem",
          lambda view, args, _k, _r: remember(view.item(view.count() - 1), "text", first_text(args)) if args and isinstance(args[0], str) else None)
    after(widgets.QMessageBox, "addButton",
          lambda box, args, _k, button: remember(button, "text", first_text(args)) if first_text(args) else None)

    from PyQt6.QtCore import QObject

    def named_property(target, args, _kwargs, _result):
        if args and args[0] == "keywords":
            remember(target, "search keywords", args[1])

    after(QObject, "setProperty", named_property)
    from telescope.widgets import settings_search
    original_keywords = settings_search.keywords

    def keywords(widget, text):
        remember(widget, "search keywords", text)
        return original_keywords(widget, text)

    stack.enter_context(patch.object(settings_search, "keywords", keywords))

    from telescope.widgets.common import StatusPill
    original_set = StatusPill.set

    def set_pill(widget, value, note, kind, icon_name):
        original_set(widget, value, note, kind, icon_name)
        remember(widget, "value", value)
        remember(widget, "note", note)

    stack.enter_context(patch.object(StatusPill, "set", set_pill))


def text_regions(top):
    """Every translated text visible on top, with the rectangle of what shows it."""
    from PyQt6.QtCore import QPoint, QRect
    from PyQt6.QtWidgets import QListWidget, QMenu, QTabBar, QWidget
    regions = []

    def add(owner, slot, text, rect, widget):
        for part in scenes.parts_of(text):
            regions.append({"key": part.identity, "slot": slot, "shown": str(part), "values": part.values,
                            "rect": [rect.x(), rect.y(), rect.width(), rect.height()], "widget": widget})

    for widget in [top, *top.findChildren(QWidget)]:
        if not widget.isVisible():
            continue
        origin = widget.mapTo(top, QPoint(0, 0))
        whole = QRect(origin, widget.size())
        for slot, text in getattr(widget, "_preview_texts", {}).items():
            if not text or slot == "search keywords":
                continue
            rect = whole
            if slot == "window title":
                rect = QRect(origin, widget.size()).intersected(QRect(origin.x(), origin.y(), widget.width(), 28))
            elif slot.startswith("tab ") and isinstance(widget, QTabBar):
                rect = QRect(widget.mapTo(top, widget.tabRect(int(slot[4:])).topLeft()), widget.tabRect(int(slot[4:])).size())
            add(widget, slot, text, rect, type(widget).__name__)
        if isinstance(widget, QMenu):
            for action in widget.actions():
                for slot, text in getattr(action, "_preview_texts", {}).items():
                    if text and slot == "text" and action.isVisible():
                        box = widget.actionGeometry(action)
                        add(action, slot, text, QRect(widget.mapTo(top, box.topLeft()), box.size()), "QMenu")
        if isinstance(widget, QListWidget):
            for row in range(widget.count()):
                item = widget.item(row)
                for slot, text in getattr(item, "_preview_texts", {}).items():
                    if text:
                        box = widget.visualItemRect(item)
                        add(item, slot, text, QRect(widget.viewport().mapTo(top, box.topLeft()), box.size()), "QListWidget")
    return regions


def tooltip_pictures(top, regions):
    """The popup each tooltip would show, by text key, with where it opens: under the control that owns it."""
    from PyQt6.QtCore import QBuffer, QByteArray, QIODevice, QPoint
    from PyQt6.QtWidgets import QApplication, QToolTip
    tips = {}
    for region in regions:
        if region["slot"] != "tooltip" or region["key"] in tips or len(tips) >= 120:
            continue
        QToolTip.showText(QPoint(0, 0), region["shown"], top)
        QApplication.processEvents()
        label = next((w for w in QApplication.topLevelWidgets() if w.metaObject().className() == "QTipLabel"), None)
        if label is None:
            continue
        array = QByteArray()
        buffer = QBuffer(array)
        buffer.open(QIODevice.OpenModeFlag.WriteOnly)
        label.grab().save(buffer, "PNG")
        x, y, _w, h = region["rect"]
        tips[region["key"]] = {"image": base64.b64encode(bytes(array)).decode(), "x": x, "y": y + h + 4,
                               "width": label.width(), "height": label.height()}
    QToolTip.hideText()
    return tips


def render(payload):
    import contextlib
    os.environ["QT_QPA_PLATFORM"] = "offscreen"
    from PyQt6.QtCore import QByteArray, QBuffer, QIODevice
    from PyQt6.QtWidgets import QApplication
    from telescope.theme import apply_theme
    from scripts import text_fit

    if "screens" in payload:
        register_scenes(payload["screens"])
    lang, scene, drafts = validate(payload)
    app = QApplication.instance() or QApplication([])
    apply_theme(app)
    i18n.load(lang)
    if lang != 'en' and lang not in i18n.available():
        i18n._set_qt_locale(lang)
        i18n._language = lang
        i18n._plural = i18n.plural_rule(project.language_header(lang))
    for identity, text in drafts.items():
        i18n._catalog[tuple(json.loads(identity))] = [text]

    with contextlib.ExitStack() as stack:
        instrument(stack)
        top = scenes.SCENES[scene].build(stack)
        app.processEvents()
        picture = top.grab()
        regions = text_regions(top)
        findings = text_fit.check(top, lang, SCENES[scene], text_fit._Texts(), width=scenes.SCENES[scene].width)
        array = QByteArray()
        buffer = QBuffer(array)
        buffer.open(QIODevice.OpenModeFlag.WriteOnly)
        picture.save(buffer, "PNG")
        result = {"image": base64.b64encode(bytes(array)).decode(), "width": top.width(), "height": top.height(),
                  "regions": regions, "tips": tooltip_pictures(top, regions), "findings": [{"text": f.text, "kind": f.kind} for f in findings],
                  "scene": scene, "lang": lang}
        top.close()
        return result


_spare, _spare_lock = None, threading.Lock()


class _Worker:
    def __init__(self):
        self.process = subprocess.Popen([sys.executable, str(Path(__file__).resolve()), "--render"], stdin=subprocess.PIPE,
                                        stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, encoding="utf-8")
        self.revision, self.loaded = revision(), False

    def wait_loaded(self):
        if not self.loaded:
            self.process.stdout.readline()
            self.loaded = True


def warm():
    global _spare
    with _spare_lock:
        if _spare is None:
            _spare = _Worker()


def worker(payload):
    # Every render gets a fresh process; one is kept started ahead so Python and Qt have already loaded when a request comes
    global _spare
    with _spare_lock:
        current = _spare or _Worker()
        if current.revision != revision():
            current.process.kill()
            current = _Worker()
        _spare = spare = _Worker()
    try:
        current.wait_loaded()
        output, errors = current.process.communicate(json.dumps(payload), timeout=25)
    except subprocess.TimeoutExpired:
        current.process.kill()
        current.process.communicate()
        raise RuntimeError("The Qt preview took too long")
    finally:
        # Hosting only gives the service CPU while a request runs, so the next process finishes loading before replying
        spare.wait_loaded()
    if current.process.returncode:
        raise RuntimeError(errors[-2500:] or "The Qt preview failed")
    return json.loads(output)


def render_once(payload):
    """One render in its own process, without the spare the server keeps waiting: for the batches the checks run."""
    result = subprocess.run([sys.executable, str(Path(__file__).resolve()), "--render"], input=json.dumps(payload),
                            capture_output=True, text=True, encoding="utf-8", timeout=120)
    if result.returncode:
        raise RuntimeError(result.stderr[-2500:] or "The Qt preview failed")
    return json.loads(result.stdout.splitlines()[-1])


def render_all(ids=None, screens=None):
    """Scenes rendered in English, a few workers at a time, in scene order. screens: the texts the real screens show,
    which decides what the other-texts pages hold (the recorded ones when None)."""
    from concurrent.futures import ThreadPoolExecutor
    extra = {} if screens is None else {"screens": sorted(screens)}
    with ThreadPoolExecutor(min(8, os.cpu_count() or 2)) as pool:
        return list(pool.map(lambda scene: render_once({"lang": "en", "scene": scene, **extra}), ids or list(SCENES)))


def render_for_record():
    """Real screens first, then the other-texts pages for whatever those leave out."""
    real = render_all([id for id, scene in scenes.SCENES.items() if scene.section != "messages"])
    screens = {r["key"] for result in real for r in result["regions"]}
    register_scenes(screens)
    rest = render_all([id for id, scene in scenes.SCENES.items() if scene.section == "messages"], screens)
    return real + rest


def coverage(results):
    """Which catalog texts the scenes show. Plural texts have no scene yet; messages scenes only list wording."""
    entries = catalog()
    plural = {identity for identity, entry in entries.items() if entry.plural}
    known = set(entries)  # a plugin's own text or one built at run time can be tagged without being in the catalog
    mapped = {region["key"] for result in results for region in result["regions"]} & known
    in_screens = {region["key"] for result in results if not result["scene"].startswith("messages-")
                  for region in result["regions"]} & known
    return {"mapped": sorted(mapped), "missing": sorted(set(entries) - plural - mapped), "plural": sorted(plural),
            "screens": sorted(in_screens), "messages_only": sorted(mapped - in_screens),
            "scenes": {result["scene"]: sorted({r["key"] for r in result["regions"]} & known) for result in results},
            "total": len(entries)}


def check_coverage(actual, expected):
    problems = []
    for name in ("missing", "plural", "messages_only", "screens"):
        added = set(actual[name]) - set(expected[name])
        removed = set(expected[name]) - set(actual[name])
        problems.extend(f"{name}: added {identity}" for identity in sorted(added))
        problems.extend(f"{name}: removed {identity}" for identity in sorted(removed))
    for name in sorted(set(actual["scenes"]) | set(expected["scenes"])):
        if actual["scenes"].get(name) != expected["scenes"].get(name):
            problems.append(f"The texts in scene {name} changed; inspect its preview before updating coverage.json")
    return problems


def language_progress(entries, found):
    current = {entry.key: entry for entry in entries.values()}
    translated = {entry.key for entry in found if entry.key in current and entry.msgstr
                  and all(entry.msgstr) and not entry.fuzzy and entry.plural == current[entry.key].plural}
    return {"translated": len(translated), "total": len(current)}


def metadata(fresh=False):
    entries = catalog()
    translations = {}
    language_stats = {}
    approvals = {}
    for lang in i18n.available():
        found, _ = i18n.read_po(i18n.LOCALE_DIR / f"{lang}.po")
        state = review.load(lang)
        by_key = {entry.key: entry for entry in found}
        statuses = {k: review.status(entry, by_key.get(entry.key), state) for k, entry in entries.items()}
        approvals[lang] = {k: item['reviewer'] for k, item in statuses.items() if item['status'] == 'approved'}
        language_stats[lang] = {**language_progress(entries, found), 'approved': len(approvals[lang]),
                                'machine': sum(item['origin'] == 'machine' for item in statuses.values())}
        translations[lang] = {key(e.ctx, e.msgid): (e.msgstr[0] if e.msgstr and not e.fuzzy else "") for e in found}
    expected = json.loads((ASSETS / "coverage.json").read_text(encoding="utf-8"))
    actual, drift = recorded_coverage(expected, fresh)
    return {"scenes": SCENES, "sections": scenes.sections(), "languages": ["en", *i18n.available()], "translations": translations, "language_stats": language_stats, "approvals": approvals,
            "entries": {k: {"english": e.msgid, "context": e.ctx, "locations": e.locations,
                             "comments": e.comments, "plural": e.plural,
                             "id": review.identity(e), "sourceDigest": review.source_digest(e)} for k, e in entries.items()},
            "coverage": actual, "total": len(entries), "revision": revision(), "drift": drift}


_fresh = {}


def recorded_coverage(expected, fresh):
    """The recorded coverage, or this checkout's own (rendered once per revision) when fresh, with how they differ."""
    if not fresh:
        return {**expected, "mapped": sorted({k for keys in expected["scenes"].values() for k in keys})}, []
    current = revision()
    if _fresh.get("revision") != current:
        _fresh.update(revision=current, coverage=coverage(render_all()))
    return _fresh["coverage"], check_coverage(_fresh["coverage"], expected)


def emulator_identity(id_token):
    # Only with --emulators: the Auth emulator's tokens are unsigned, and the name typed into its sign-in form becomes the username
    claims = json.loads(base64.urlsafe_b64decode(id_token.split('.')[1] + '==='))
    name = re.sub(r'[^A-Za-z0-9-]', '', claims.get('name') or claims.get('email', '').split('@')[0])[:39] or 'Tester'
    store = account_service.Store('demo-telescope')
    store.put('identities/' + claims['sub'], {'username': {'stringValue': name}})


def export_po(payload):
    lang, _scene, drafts = validate(payload)
    if lang == "en":
        raise ValueError("Choose a translation language to export")
    path = i18n.LOCALE_DIR / f"{lang}.po"
    if path.exists():
        entries, header = i18n.read_po(path)
    else:
        header = project.language_header(lang)
        entries = list(catalog().values())
        for entry in entries:
            entry.msgstr = project.blank_record(entry, header)['forms']
    for entry in entries:
        identity = key(entry.ctx, entry.msgid)
        if identity in drafts:
            entry.msgstr = [drafts[identity]]
            entry.flags = [flag for flag in entry.flags if flag != "fuzzy"]
    with tempfile.TemporaryDirectory(prefix="telescope-translation-export-") as folder:
        path = Path(folder) / f"{lang}.po"
        i18n.write_po(path, header, entries)
        return path.read_bytes()


class Handler(BaseHTTPRequestHandler):
    def reply(self, status, data, content_type):
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(data)))
        self.send_header("Cache-Control", "no-store")
        self.send_header("X-Content-Type-Options", "nosniff")
        self.end_headers()
        self.wfile.write(data)

    def allowed(self):
        origin = f"http://127.0.0.1:{self.server.server_port}"
        return self.headers.get("Host") == origin.removeprefix("http://") and self.headers.get("Origin", origin) == origin

    def do_GET(self):
        if not self.allowed():
            return self.reply(403, b"Use the printed local address", "text/plain")
        if self.path == "/api/warm":
            warm()
            return self.reply(200, b"{}", "application/json")
        if self.path == "/api/catalog":
            return self.reply(200, json.dumps({**metadata(fresh=True), "mode": "admin" if getattr(self.server, "reviewer", None) else "manual", "shared_available": getattr(self.server, 'shared', False), "emulators": bool(getattr(self.server, 'emulators', False)), "account_deletion_available": bool(getattr(self.server, 'cloud_account', None))}).encode(), "application/json")
        if self.path == '/api/project-snapshot':
            return self.reply(200, json.dumps(project.repository_snapshot(catalog())).encode(), 'application/json')
        assets = {"/": ("index.html", "text/html; charset=utf-8"), "/app.js": ("app.js", "text/javascript"),
                  "/style.css": ("style.css", "text/css"), "/shared.js": ("shared.js", "text/javascript"),
                  "/admin.js": ("admin.js", "text/javascript"),
                  "/language-picker.js": ("language-picker.js", "text/javascript"),
                  "/languages.json": ("languages.json", "application/json"),
                  "/firebase-config.json": ("firebase-config.json", "application/json")}
        if self.path not in assets:
            return self.reply(404, b"Not found", "text/plain")
        file, mime = assets[self.path]
        self.reply(200, (ASSETS / file).read_bytes(), mime)

    def do_POST(self):
        if not self.allowed() or self.headers.get("Content-Type") != "application/json":
            return self.reply(403, b"Expected a local JSON request", "text/plain")
        try:
            length = int(self.headers.get("Content-Length", "0"))
            maximum = 32_000_000 if self.path == '/api/project-action' else 4_100_000 if self.path == '/api/import-po' else 100_000
            if not 0 < length <= maximum:
                raise ValueError("Request is too large or empty")
            payload = json.loads(self.rfile.read(length))
            if self.path == '/api/identity' and getattr(self.server, 'emulators', False):
                emulator_identity(payload.get('idToken'))
                return self.reply(200, b'{}', 'application/json')
            if isinstance(payload, dict):
                payload.pop("screens", None)  # only the coverage tools say what the real screens show
            if self.path == '/api/delete-account':
                if not getattr(self.server, 'cloud_account', None):
                    raise ValueError('The account deletion service is not configured on this development server')
                config = json.loads((ASSETS / 'firebase-config.json').read_text(encoding='utf-8'))
                account_service.delete_account(payload.get('idToken'), config, self.server.cloud_account)
                return self.reply(200, b'{"deleted":true}', 'application/json')
            if self.path == '/api/project-action':
                action = payload.get('action')
                if action not in ('initialise', 'restore') and ('approvals' in payload or 'languages' in payload):
                    payload['platform'] = project.apply_reviews(payload['platform'], payload.get('approvals', []), payload.get('languages', []))
                if action == 'initialise':
                    result = project.initialise(payload['incoming'])
                elif action == 'restore':
                    result = project.validate(payload['incoming'])
                    if result.get('kind') != 'platform' or not isinstance(result.get('conflicts'), list):
                        raise ValueError('Choose a whole-project backup')
                    project.validate(result['base'])
                elif action == 'fold':
                    result = payload['platform']
                elif action == 'merge':
                    result = project.merge(payload['platform'], payload['incoming'])
                elif action == 'resolve':
                    result = project.resolve(payload['platform'], payload['index'], payload['choice'])
                elif action == 'export':
                    return self.reply(200, project.export_language(payload['platform'], payload['language']), 'application/zip')
                elif action == 'status':
                    return self.reply(200, json.dumps(project.projection(payload['platform'])).encode(), 'application/json')
                elif action == 'import-po':
                    result = project.import_po(payload['platform'], payload['language'], payload['content'], payload['author'])
                else:
                    raise ValueError('Choose a project action')
                result = {'platform': result, 'projection': project.projection(result)}
                return self.reply(200, json.dumps(result).encode(), 'application/json')
            validate(payload)
            if self.path == '/api/approve':
                if not getattr(self.server, 'reviewer', None):
                    return self.reply(403, json.dumps({'error': 'Only an administrator can approve translations'}).encode(), 'application/json')
                lang, _scene, drafts = validate(payload)
                if lang == 'en':
                    raise ValueError('Choose a translation language')
                sources = catalog()
                identity = payload.get('identity')
                if identity not in sources:
                    raise ValueError('Choose a known text')
                target_entries, header = i18n.read_po(i18n.LOCALE_DIR / f'{lang}.po')
                target = next((entry for entry in target_entries if key(entry.ctx, entry.msgid) == identity), None)
                if target is None:
                    raise ValueError('Extract the current catalogue before approving this text')
                if identity in drafts:
                    target.msgstr = [drafts[identity]]
                    target.flags = [flag for flag in target.flags if flag != 'fuzzy']
                state = review.load(lang)
                review.approve(sources[identity], target, state, self.server.reviewer)
                with tempfile.TemporaryDirectory(prefix='telescope-approval-') as folder:
                    path = Path(folder) / f'{lang}.po'
                    i18n.write_po(path, header, target_entries)
                    content = path.read_bytes()
                review.save(state)
                review.atomic_write(i18n.LOCALE_DIR / f'{lang}.po', content)
                return self.reply(200, json.dumps({'revision': revision()}).encode(), 'application/json')
            if self.path == '/api/import-po':
                if not getattr(self.server, 'reviewer', None):
                    return self.reply(403, json.dumps({'error': 'Only an administrator can import translations'}).encode(), 'application/json')
                count = review.import_po(payload.get('lang'), payload.get('content', ''), catalog(), payload.get('author', ''))
                return self.reply(200, json.dumps({'changed': count, 'revision': revision()}).encode(), 'application/json')
            if self.path == "/api/render":
                data = json.dumps(worker(payload)).encode()
                return self.reply(200, data, "application/json")
            if self.path == "/api/export":
                return self.reply(200, export_po(payload), "text/plain; charset=utf-8")
            self.reply(404, b"Not found", "text/plain")
        except (ValueError, KeyError, TypeError, RuntimeError, subprocess.TimeoutExpired) as exc:
            self.reply(400, json.dumps({"error": str(exc)}).encode(), "application/json")

    def log_message(self, _format, *_args):
        pass


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--port", type=int, default=8769)
    parser.add_argument("--admin", metavar="REVIEWER", help="Local administrator mode; approvals record this public reviewer name")
    parser.add_argument("--shared", action="store_true", help="Enable GitHub sign-in and pending Firebase submissions")
    parser.add_argument("--emulators", action="store_true", help="Use the local Firebase emulators for shared mode (with --shared)")
    parser.add_argument("--cloud-account", metavar="ACCOUNT", help="Explicit gcloud account for verified account erasure, only on this loopback development server")
    parser.add_argument("--export-project", metavar="PATH", help="Write a repository snapshot for an administrator to import")
    parser.add_argument("--record-machine-baseline", action="store_true", help="Record current machine translations without approving them")
    parser.add_argument("--render", action="store_true", help=argparse.SUPPRESS)
    parser.add_argument("--check", action="store_true")
    parser.add_argument("--check-all", action="store_true")
    parser.add_argument("--record-coverage", action="store_true", help="Record coverage after inspecting the scenes")
    args = parser.parse_args()
    if args.export_project:
        review.atomic_write(Path(args.export_project), (json.dumps(project.repository_snapshot(catalog()), ensure_ascii=False) + '\n').encode())
        print('Exported repository texts, translations and approval records')
        return 0
    if args.record_machine_baseline:
        sources = catalog()
        for lang in i18n.available():
            entries, _header = i18n.read_po(i18n.LOCALE_DIR / f'{lang}.po')
            found = {entry.key: entry for entry in entries}
            state = review.load(lang)
            for source in sources.values():
                target = found.get(source.key)
                if review.complete(source, target):
                    state['machine'].setdefault(review.identity(source), review.digest(source, target))
            review.save(state)
        print('Recorded machine provenance without approving translations')
        return 0
    if args.render:
        # Qt loads while the process waits as the spare; telescope modules load after instrument() in render()
        os.environ["QT_QPA_PLATFORM"] = "offscreen"
        from PyQt6.QtWidgets import QApplication
        app = QApplication.instance() or QApplication([])
        print("ready", flush=True)
        print(json.dumps(render(json.load(sys.stdin))))
        del app
        return 0
    if args.check or args.check_all or args.record_coverage:
        actual = coverage(render_for_record() if args.record_coverage else render_all())
        if args.record_coverage:
            path = ASSETS / "coverage.json"
            temporary = path.with_suffix(".tmp")
            actual.pop("mapped")  # what the scenes list, joined
            temporary.write_text(json.dumps(actual, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
            temporary.replace(path)
            print("Recorded the current mapped texts and known gaps")
            return 0
        expected = json.loads((ASSETS / "coverage.json").read_text(encoding="utf-8"))
        problems = check_coverage(actual, expected)
        print(f"{len(actual['mapped'])} of {actual['total']} texts shown by {len(SCENES)} scenes; "
              f"{len(actual['messages_only'])} not on a real screen; {len(actual['missing'])} missing; "
              f"{len(actual['plural'])} plural texts not editable yet")
        print("\n".join(problems))
        return int(bool(problems) or (args.check_all and bool(actual["missing"])))
    import threading
    threading.Thread(target=lambda: metadata(fresh=True), daemon=True).start()  # the first page load waits for the scenes
    with HTTPServer(("127.0.0.1", args.port), Handler) as server:
        server.reviewer = args.admin
        server.shared = args.shared
        server.emulators = args.emulators
        server.cloud_account = args.cloud_account
        print(f"Translation preview: http://127.0.0.1:{server.server_port}", flush=True)
        try:
            server.serve_forever()
        except KeyboardInterrupt:
            pass
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
