#!/usr/bin/env python3
"""Everything the Python desktop app has that the Rust one must match, and the ledger that tracks each one.

The inventory comes from the Python app itself: its .pot, its tests, the translation preview's scenes, the real
window built offscreen (settings, shortcut actions), main.py's argparse and a scan of its source. Each item has a
stable id ("kind:key") and an area, the plugin or module that owns it. ledger.toml says how far each one is.

    python inventory.py            # list every item as "id<TAB>area"
    python inventory.py --check    # exit 1 when the ledger misses an item, lists a gone one or has a bad entry
    python inventory.py --update   # add new items as todo, drop gone ones that were still a bare todo
    python inventory.py --report   # progress per area and overall

Run it from anywhere with the desktop app's requirements installed; it builds the window offscreen.
"""

import argparse
import ast
import contextlib
import json
import os
import re
import sys
import tomllib
from collections import Counter, defaultdict
from dataclasses import dataclass
from pathlib import Path
from unittest import mock

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[1]
DESKTOP = REPO / "desktop"
TELESCOPE = DESKTOP / "telescope"
LEDGER = HERE / "ledger.toml"
FIXTURES = DESKTOP / "tests" / "fixtures" / "configs"

KINDS = {
    "text": "UI texts from telescope.pot (ctx|msgid when there's a context)",
    "test": "Python tests, file::Class::name",
    "scene": "window scenes from the translation preview's SCENES",
    "setting": "settings keys, global.plugin.key or device.plugin.key",
    "config": "top-level fields of telescope_config.json",
    "action": "shortcut action ids, which --action also takes",
    "cli": "command line flags",
    "phone": "phone protocol endpoints and the actions sent to them",
    "system": "where the app touches the system: subprocesses, registry, D-Bus, env vars, paths, ports, tray, platform/",
}
STATUSES = ("todo", "ported", "verified", "dropped")
FIELDS = ("status", "rust", "reason", "note")
DONE = ("ported", "verified", "dropped")
# Not the app: the protocol scan skips the browser camera's own server and the On Air sign's web page
NOT_PHONE = ("telescope/web/", "telescope/on_air.py", "telescope/browser_server.py", "telescope/browser_rtc.py")
SUBPROCESS = {("subprocess", f) for f in ("run", "Popen", "call", "check_call", "check_output")} | {
    ("os", "system"), ("os", "startfile"), ("os", "execv"), ("os", "execvp"), ("os", "spawnv"), ("webbrowser", "open"),
    ("QProcess", "startDetached"), ("QDesktopServices", "openUrl")}
SYSTEM_PATH = re.compile(r"^(/(dev|sys|etc|usr|proc|run|tmp|var|lib|opt)/|~[/\\]|[A-Za-z]:\\|%[A-Z]{3,}%)")
REGISTRY = re.compile(r"^(HKEY_[A-Z_]+\\|Software\\|SOFTWARE\\|SYSTEM\\|System\\)")
DBUS = re.compile(r"^org\.(freedesktop|kde|gnome)\.[A-Za-z0-9_.]+$")
FOLDER_CALLS = {("Path", "home"), ("path", "expanduser"), ("QStandardPaths", "writableLocation"),
                ("tempfile", "gettempdir"), ("tempfile", "mkdtemp"), ("tempfile", "NamedTemporaryFile"),
                ("tempfile", "TemporaryDirectory"), ("shutil", "rmtree"), ("os", "symlink")}
COMMAND_CALL = re.compile(r"run|popen|exec|spawn|call|check_output", re.I)
COMMAND_NAME = re.compile(r"cmd|argv|command", re.I)
PROGRAM = re.compile(r"^[A-Za-z0-9_.+-]+$")
PHONE_PATH = re.compile(r"/v1/[a-z0-9_.]+(?:/[a-z0-9_.]+)?")


@dataclass(frozen=True)
class Item:
    kind: str
    key: str
    area: str

    @property
    def id(self):
        return f"{self.kind}:{self.key}"


def rel(path: Path) -> str:
    return path.relative_to(DESKTOP).as_posix()


def area_of(where: str) -> str:
    """The area a desktop file belongs to: the plugin for plugins/, platform and widgets as one each, else the module."""
    parts = where.removeprefix("telescope/").removesuffix(".py").split("/")
    if parts[0] == "plugins" and len(parts) > 1:
        return parts[1]
    return parts[0]


def python_files():
    files = [DESKTOP / "main.py", DESKTOP / "update_guard.py"] + sorted(TELESCOPE.rglob("*.py"))
    return [f for f in files if not rel(f).startswith("telescope/web/")]


def parse(path: Path):
    return ast.parse(path.read_text(encoding="utf-8"), str(path))


def docstrings(tree) -> set:
    """The ids of the string nodes that are docstrings, so a scan of constants can skip prose."""
    found = set()
    for node in ast.walk(tree):
        if isinstance(node, (ast.Module, ast.ClassDef, ast.FunctionDef, ast.AsyncFunctionDef)) and node.body:
            first = node.body[0]
            if isinstance(first, ast.Expr) and isinstance(first.value, ast.Constant) and isinstance(first.value.value, str):
                found.add(id(first.value))
    return found


def strings(tree):
    """Every string constant that isn't a docstring, with the function it's in."""
    skip = docstrings(tree)

    def walk(node, where):
        for child in ast.iter_child_nodes(node):
            inner = child.name if isinstance(child, (ast.FunctionDef, ast.AsyncFunctionDef)) else where
            if isinstance(child, ast.Constant) and isinstance(child.value, str) and id(child) not in skip:
                yield child.value, where
            yield from walk(child, inner)
    yield from walk(tree, "<module>")


def calls(tree):
    """Every call with the function it's in."""
    def walk(node, where):
        for child in ast.iter_child_nodes(node):
            inner = child.name if isinstance(child, (ast.FunctionDef, ast.AsyncFunctionDef)) else where
            if isinstance(child, ast.Call):
                yield child, where
            yield from walk(child, inner)
    yield from walk(tree, "<module>")


def dotted(node) -> tuple:
    """("subprocess", "run") for subprocess.run, ("", "run") for a bare run."""
    if isinstance(node, ast.Attribute):
        base = node.value
        name = base.id if isinstance(base, ast.Name) else base.attr if isinstance(base, ast.Attribute) else ""
        return name, node.attr
    if isinstance(node, ast.Name):
        return "", node.id
    return "", ""


# ── Texts and tests ──────────────────────────────────────────────────────────

def texts():
    from telescope.i18n import read_po
    entries, _header = read_po(TELESCOPE / "locale" / "telescope.pot")
    items = [Item("text", f"{e.ctx}|{e.msgid}" if e.ctx else e.msgid, area_of(e.locations[0].split(":")[0]))
             for e in entries if e.msgid]
    assert len({i.key for i in items}) == len(items), "two .pot entries got the same id"
    return items


def test_area(name: str, areas: set) -> str:
    """test_shortcuts_plugin.py -> shortcuts, test_platform_linux.py -> platform: the longest known area it starts with."""
    stem = name.removeprefix("test_").removesuffix(".py").removesuffix("_plugin")
    known = [a for a in areas if stem == a or stem.startswith(a + "_")]
    return max(known, key=len) if known else stem


def tests(areas: set):
    items = []
    for path in sorted((DESKTOP / "tests").rglob("test_*.py")):
        tree = parse(path)
        name = path.relative_to(DESKTOP / "tests").as_posix()
        area = test_area(path.name, areas)
        for node in tree.body:
            if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)) and node.name.startswith("test_"):
                items.append(Item("test", f"{name}::{node.name}", area))
            elif isinstance(node, ast.ClassDef) and node.name.startswith("Test"):
                for sub in node.body:
                    if isinstance(sub, (ast.FunctionDef, ast.AsyncFunctionDef)) and sub.name.startswith("test_"):
                        items.append(Item("test", f"{name}::{node.name}::{sub.name}", area))
    return items


# ── The real window: scenes, settings, shortcut actions ──────────────────────

def from_window():
    """Scenes, settings keys and shortcut action ids, read from the window built offscreen like main_window does."""
    os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")
    import logging
    logging.disable(logging.CRITICAL)
    from PyQt6.QtWidgets import QApplication
    from scripts import translation_scenes as ts
    from telescope.plugins import builtin_plugins
    app = QApplication.instance() or QApplication([])  # noqa: F841
    items = []
    for scene in ts.SCENES.values():
        if scene.section in ("search", "messages"):
            continue  # made from the .pot (one per keyword text, pages of texts no screen shows), not real screens
        module = scene.build.__module__.rsplit(".", 1)[-1].removeprefix("translation_scenes").lstrip("_")
        items.append(Item("scene", scene.id, module or scene.section))
    builtin = {p.name for p in builtin_plugins() if p.name}
    import telescope.app as app_module
    owners = {}  # action id: the plugin whose create_actions() made it
    register = app_module.TelescopeWindow.register_plugin

    def noting_owner(win, plugin, *args, **kwargs):
        before = len(win._shortcut_actions)
        result = register(win, plugin, *args, **kwargs)
        owners.update((a.id, plugin.name) for a in win._shortcut_actions[before:])
        return result
    with contextlib.ExitStack() as stack:
        stack.enter_context(mock.patch.dict(os.environ))
        stack.enter_context(mock.patch.object(app_module.TelescopeWindow, "register_plugin", noting_owner))
        win = ts.main_window(stack)
        for action in win.shortcut_actions():
            items.append(Item("action", action.id, owners.get(action.id) or "app"))
        settings = set()
        for plugin in win._plugins:
            if plugin.name in builtin:
                scope = "device" if plugin.device_local else "global"
                settings |= {(scope, plugin.name, k) for k in (plugin.get_config() or {})}
    settings |= fixture_settings(builtin)
    items += [Item("setting", f"{s}.{p}.{k}", p) for s, p, k in settings]
    items += [Item("config", field, "config") for field in config_fields()]
    return items, builtin


def fixture_settings(builtin: set) -> set:
    """(scope, plugin, key) for every key a fixture's expected.json saves, for the built-in plugins."""
    found = set()
    for path in sorted(FIXTURES.glob("*/expected.json")):
        saved = json.loads(path.read_text(encoding="utf-8"))["saved"] or {}
        blocks = [("global", saved.get("plugin_configs", {}))]
        blocks += [("device", d.get("plugin_configs", {})) for d in saved.get("devices", {}).values() if isinstance(d, dict)]
        for scope, configs in blocks:
            for plugin, cfg in configs.items():
                if plugin in builtin and isinstance(cfg, dict):
                    found |= {(scope, plugin, k) for k in cfg}
    return found


def config_fields() -> set:
    """Top-level fields at least two fixtures save; a field only one has is that fixture's own (future-version's)."""
    count = Counter()
    for path in FIXTURES.glob("*/expected.json"):
        count.update((json.loads(path.read_text(encoding="utf-8"))["saved"] or {}).keys())
    return {k for k, n in count.items() if n >= 2}


# ── Command line ─────────────────────────────────────────────────────────────

def cli():
    """main.py's argparse, run on its own (importing main.py would start the update guard and the splash), plus
    the flags main.py and update_guard.py look for in sys.argv themselves."""
    tree = parse(DESKTOP / "main.py")
    func = next(n for n in tree.body if isinstance(n, ast.FunctionDef) and n.name == "parse_args")
    from telescope.single_instance import RESTARTED
    from telescope.updates import MOVED_FROM
    space = {"argparse": argparse, "RESTARTED": RESTARTED, "MOVED_FROM": MOVED_FROM}
    exec(compile(ast.Module([func], []), "main.py", "exec"), space)
    box = []
    with mock.patch.object(argparse.ArgumentParser, "parse_known_args", lambda self, *a, **k: box.append(self)):
        space["parse_args"]([])
    flags = {o for a in box[0]._actions for o in a.option_strings}
    for name in ("main.py", "update_guard.py"):
        flags |= {s for s, _ in strings(parse(DESKTOP / name)) if re.fullmatch(r"--[a-z][a-z-]+", s)}
    return [Item("cli", f, "main") for f in flags]


# ── Phone protocol ───────────────────────────────────────────────────────────

def phone(action_ids: set):
    """Endpoints from paths in the source ("/v1/ping", f"{ctrl.base}/state"), and every action sent to them."""
    found = {}
    for path in python_files():
        where = rel(path)
        if where.startswith(NOT_PHONE):
            continue
        tree = parse(path)
        for text, _ in strings(tree):
            for match in PHONE_PATH.findall(text):
                found.setdefault(match, where)
        for node in ast.walk(tree):
            if isinstance(node, ast.JoinedStr):  # f"{self.base}/state": a phone client's base is .../v1
                for value, after in zip(node.values, node.values[1:]):
                    if (isinstance(value, ast.FormattedValue) and dotted(value.value)[1].endswith("base")
                            and isinstance(after, ast.Constant) and after.value.startswith("/")):
                        path = re.split(r"[?&{]", after.value)[0]
                        found.setdefault(path if path.startswith("/v1/") else "/v1" + path, where)
        for call, _ in calls(tree):
            name = dotted(call.func)[1]
            args = [a.value for a in call.args if isinstance(a, ast.Constant) and isinstance(a.value, str)]
            if name == "_request" and args and args[0].startswith("/"):  # stills.py's ctrl requests
                found.setdefault("/v1" + args[0], where)
    items = [Item("phone", p, area_of(w)) for p, w in found.items()]
    for node in ast.walk(parse(TELESCOPE / "pairing.py")):  # what the phone asks this computer's pairing server
        if isinstance(node, ast.JoinedStr) and isinstance(node.values[0], ast.Constant):
            if re.fullmatch(r"/[a-z]+/", node.values[0].value):
                items.append(Item("phone", f"served {node.values[0].value}", "pairing"))
    for path in python_files():
        where = rel(path)
        if where.startswith(NOT_PHONE) or area_of(where) in ("shortcuts", "single_instance", "main"):
            continue
        tree = parse(path)
        sent = set()
        for call, _ in calls(tree):
            name = dotted(call.func)[1]
            for kw in call.keywords:
                if kw.arg == "action" and isinstance(kw.value, ast.Constant) and isinstance(kw.value.value, str):
                    sent.add(kw.value.value)
            if name == "_session" and call.args and isinstance(call.args[0], ast.Constant):
                items.append(Item("phone", f"session.{call.args[0].value}", area_of(where)))
        for node in ast.walk(tree):
            if isinstance(node, ast.Dict):
                for k, v in zip(node.keys, node.values):
                    if (isinstance(k, ast.Constant) and k.value == "action" and isinstance(v, ast.Constant)
                            and isinstance(v.value, str)):
                        sent.add(v.value)
        endpoint = "still" if where == "telescope/stills.py" else "control"
        items += [Item("phone", f"{endpoint}.{a}", area_of(where)) for a in sent if "." not in a and a not in action_ids]
    return items


# ── System touch points ──────────────────────────────────────────────────────

def system():
    items = set()
    for path in python_files():
        where = rel(path)
        area = area_of(where)
        tree = parse(path)
        for text, _ in strings(tree):
            if REGISTRY.match(text):
                items.add(Item("system", f"registry:{text}", area))
            elif DBUS.match(text):
                items.add(Item("system", f"dbus:{text}", area))
            elif SYSTEM_PATH.match(text) and "\n" not in text and len(text) < 120:
                items.add(Item("system", f"path:{text}", area))
        for call, func in calls(tree):
            base, name = dotted(call.func)
            if (base, name) in SUBPROCESS:
                first = call.args[0] if call.args else None
                if isinstance(first, (ast.List, ast.Tuple)) and first.elts:
                    first = first.elts[0]
                program = (Path(first.value).name if isinstance(first, ast.Constant) and isinstance(first.value, str)
                           else f"<{ast.unparse(first)}>" if first is not None else "<>")
                items.add(Item("system", f"run:{where}:{func}:{base}.{name}:{program}", area))
            elif (base, name) in FOLDER_CALLS:  # files outside the config folder start from one of these
                items.add(Item("system", f"folder:{where}:{func}:{base}.{name}", area))
            elif name == "QSystemTrayIcon":
                items.add(Item("system", f"tray:{where}:{func}", area))
            elif name == "showMessage" and base != "statusBar":
                items.add(Item("system", f"tray-message:{where}:{func}", area))
            if COMMAND_CALL.search(name) and (base, name) not in SUBPROCESS:  # _run(["pkexec", ...]) and the like
                for arg in call.args:
                    if program := command_of(arg, loose=True):
                        items.add(Item("system", f"run:{where}:{func}:{name}:{program}", area))
            env = env_name(call, base, name)
            if env:
                items.add(Item("system", f"env:{env}", area))
        for node in ast.walk(tree):
            if isinstance(node, ast.Assign) and any(isinstance(t, ast.Name) and COMMAND_NAME.search(t.id) for t in node.targets):
                if program := command_of(node.value):
                    items.add(Item("system", f"command:{where}:{program}", area))
            if (isinstance(node, ast.Subscript) and ast.unparse(node.value) == "os.environ"
                    and isinstance(node.slice, ast.Constant)):
                items.add(Item("system", f"env:{node.slice.value}", area))
            if isinstance(node, ast.Compare) and any(ast.unparse(c) == "os.environ" for c in node.comparators):
                if isinstance(node.left, ast.Constant):
                    items.add(Item("system", f"env:{node.left.value}", area))
        for node in tree.body:
            if (isinstance(node, ast.Assign) and len(node.targets) == 1 and isinstance(node.targets[0], ast.Name)
                    and re.search(r"(^|_)PORT$", node.targets[0].id) and isinstance(node.value, ast.Constant)
                    and isinstance(node.value.value, int)):
                items.add(Item("system", f"port:{where}:{node.targets[0].id}={node.value.value}", area))
            if where.startswith("telescope/platform/") and isinstance(node, (ast.FunctionDef, ast.ClassDef)):
                if not node.name.startswith("_"):
                    items.add(Item("system", f"platform:{where.removeprefix('telescope/platform/')}:{node.name}", area))
    return list(items)


def command_of(node, loose=False):
    """"pactl load-module" for ["pactl", "load-module", ...]: a list or tuple literal that starts with a program, and
    its first word that isn't an option. loose takes a program that isn't a literal too ([adb_exe(), "devices"])."""
    if not isinstance(node, (ast.List, ast.Tuple)) or len(node.elts) < 2:
        return None
    first = node.elts[0]
    if isinstance(first, ast.Constant) and isinstance(first.value, str) and PROGRAM.match(first.value):
        program = first.value
    elif loose and isinstance(first, (ast.Call, ast.Name, ast.Attribute)):
        program = f"<{ast.unparse(first)}>"
    else:
        return None
    word = next((e.value for e in node.elts[1:] if isinstance(e, ast.Constant) and isinstance(e.value, str)
                 and not e.value.startswith("-")), "")
    return f"{program} {word}".strip()


def env_name(call, base, name):
    if (base, name) in (("environ", "get"), ("environ", "setdefault"), ("environ", "pop"), ("os", "getenv")):
        if call.args and isinstance(call.args[0], ast.Constant) and isinstance(call.args[0].value, str):
            return call.args[0].value
    return None


# ── All of it ────────────────────────────────────────────────────────────────

def inventory() -> list:
    sys.path.insert(0, str(DESKTOP))
    window, builtin = from_window()
    action_ids = {i.key for i in window if i.kind == "action"}
    found = texts() + window + cli() + phone(action_ids) + system()
    areas = {i.area for i in found} | builtin
    found += tests(areas)
    unique = {}
    for item in sorted(found, key=lambda i: (i.id, i.area)):  # one found in two modules goes to the first
        unique.setdefault(item.id, item)
    return sorted(unique.values(), key=lambda i: (list(KINDS).index(i.kind), i.area, i.key))


# ── The ledger ───────────────────────────────────────────────────────────────

HEADER = """\
# The Rust rewrite's parity ledger: every piece of the Python desktop app and how far the Rust app is with it.
# inventory.py makes the list; see README.md. Edit the values, not the ids. Each entry is
#   status = "todo" | "ported" | "verified" | "dropped"
#   rust = "rust/crates/..."           (needed for ported and verified: a path from the repo root that exists,
#                                       optionally with ::symbol or :line after it; a test names its ::function)
#   reason = "why it goes away"        (needed for dropped)
#   note = "anything else"             (comments in this file are rewritten by --update, notes are kept)
# The "# area" lines are only for reading; inventory.py works the area out itself.
"""
UNKNOWN_HELP = """\
# Gaps someone noticed that no inventory rule caught. Give each a short id, a status and a note saying what it is,
# then add a rule to inventory.py so the whole class gets caught from then on, and move it to its kind.
"""


def read_ledger(path=LEDGER) -> dict:
    if not path.exists():
        return {}
    with path.open("rb") as f:
        return tomllib.load(f)


def quote(text: str) -> str:
    return json.dumps(text, ensure_ascii=False)  # a JSON string is a TOML basic string


def entry_line(key: str, entry: dict) -> str:
    fields = ", ".join(f"{f} = {quote(str(entry[f]))}" for f in FIELDS if f in entry)
    extra = ", ".join(f"{f} = {quote(str(v))}" for f, v in entry.items() if f not in FIELDS)
    return f"{quote(key)} = {{ {', '.join(x for x in (fields, extra) if x)} }}"


def write_ledger(ledger: dict, items: list, path=LEDGER):
    areas = {i.id: i.area for i in items}
    out = [HEADER, "[unknown]", UNKNOWN_HELP.rstrip()]
    out += [entry_line(k, v) for k, v in sorted(ledger.get("unknown", {}).items())]
    for kind, help in KINDS.items():
        out += ["", f"[{kind}]", f"# {help}"]
        entries = ledger.get(kind, {})
        by_area = defaultdict(list)
        for key in entries:
            by_area[areas.get(f"{kind}:{key}", "(gone from the Python app)")].append(key)
        for area in sorted(by_area):
            out.append(f"# ── {area}")
            out += [entry_line(k, entries[k]) for k in sorted(by_area[area])]
    path.write_text("\n".join(out) + "\n", encoding="utf-8")


def problems_with(entry: dict) -> list:
    status = entry.get("status")
    if status not in STATUSES:
        return [f"status {status!r} isn't one of {', '.join(STATUSES)}"]
    if status in ("ported", "verified"):
        if not entry.get("rust"):
            return [f"{status} needs rust = where it lives"]
        path = re.sub(r":\d+$", "", str(entry["rust"]).split("::")[0])
        if not (REPO / path).exists():
            return [f"rust = {entry['rust']!r} doesn't exist (moved?)"]
    if status == "dropped" and not entry.get("reason"):
        return ["dropped needs a reason"]
    return []


def test_problems(entry: dict) -> list:
    """A ported test has to say which function replaced it, or nobody can check it."""
    if entry.get("status") not in ("ported", "verified") or not entry.get("rust"):
        return []
    path, _, name = str(entry["rust"]).partition("::")
    if not (REPO / path).is_file():
        return []  # problems_with said so
    if not name:
        return [f"rust = {entry['rust']!r} names a file, not the test that replaced it"]
    if not re.search(rf"\bfn {re.escape(name)}\b", (REPO / path).read_text(encoding="utf-8")):
        return [f"rust = {entry['rust']!r}: {path} has no fn {name}"]
    return []


def check(items: list, ledger: dict) -> list:
    errors = []
    wanted = {i.id for i in items}
    listed = {f"{kind}:{key}" for kind in KINDS for key in ledger.get(kind, {})}
    errors += [f"not in the ledger: {i}" for i in sorted(wanted - listed)]
    errors += [f"gone from the Python app but still in the ledger: {i}" for i in sorted(listed - wanted)]
    errors += [f"unknown table in the ledger: [{t}]" for t in ledger if t not in KINDS and t != "unknown"]
    for kind in list(KINDS) + ["unknown"]:
        for key, entry in ledger.get(kind, {}).items():
            errors += [f"{kind}:{key}: {p}" for p in problems_with(entry)]
            if kind == "test":
                errors += [f"{kind}:{key}: {p}" for p in test_problems(entry)]
            if kind == "unknown" and not entry.get("note"):
                errors.append(f"unknown:{key}: needs a note saying what it is")
    return errors


def update(items: list, ledger: dict) -> tuple:
    """New items added as todo; gone ones dropped only when they were a bare todo, so nothing anyone wrote is lost."""
    wanted = {i.id for i in items}
    added, removed, kept = 0, 0, []
    for item in items:
        if item.key not in ledger.setdefault(item.kind, {}):
            ledger[item.kind][item.key] = {"status": "todo"}
            added += 1
    for kind in KINDS:
        for key in list(ledger.get(kind, {})):
            if f"{kind}:{key}" not in wanted:
                if ledger[kind][key] == {"status": "todo"}:
                    del ledger[kind][key]
                    removed += 1
                else:
                    kept.append(f"{kind}:{key}")
    return added, removed, kept


def report(items: list, ledger: dict) -> str:
    done = defaultdict(Counter)
    total = defaultdict(Counter)
    for item in items:
        total[item.area][item.kind] += 1
        if ledger.get(item.kind, {}).get(item.key, {}).get("status") in DONE:
            done[item.area][item.kind] += 1
    plural = {"text": "texts", "test": "tests", "scene": "scenes", "setting": "settings", "config": "config fields",
              "action": "actions", "cli": "flags", "phone": "phone calls", "system": "system points"}
    lines = []
    width = max(len(a) for a in total)
    for area in sorted(total):
        parts = [f"{done[area][k]} of {total[area][k]} {plural[k]}" for k in KINDS if total[area][k]]
        lines.append(f"{area:<{width}}  {', '.join(parts)}")
    lines.append("")
    all_done = all_total = 0
    for kind in KINDS:
        d = sum(done[a][kind] for a in total)
        t = sum(total[a][kind] for a in total)
        all_done, all_total = all_done + d, all_total + t
        lines.append(f"{plural[kind]:<14} {d:>5} of {t:>5}  ({100 * d / t if t else 0:.1f}%)")
    lines.append(f"{'overall':<14} {all_done:>5} of {all_total:>5}  ({100 * all_done / all_total:.1f}%)")
    files = defaultdict(set)
    for item in items:
        if item.kind == "test":
            files[item.key.split("::")[0]].add(ledger.get("test", {}).get(item.key, {}).get("status") in DONE)
    lines.append(f"{'test files':<14} {sum(1 for v in files.values() if v == {True}):>5} of {len(files):>5}  "
                 "with every test done")
    unknown = ledger.get("unknown", {})
    if unknown:
        lines += ["", f"{len(unknown)} gaps noticed by hand in [unknown]:"]
        lines += [f"  {k} ({v.get('status')}): {v.get('note', '')}" for k, v in sorted(unknown.items())]
    return "\n".join(lines)


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--check", action="store_true", help="fail when the ledger and the Python app disagree")
    mode.add_argument("--update", action="store_true", help="add new items to the ledger as todo")
    mode.add_argument("--report", action="store_true", help="print progress per area")
    parser.add_argument("--ledger", type=Path, default=LEDGER)
    args = parser.parse_args(argv)
    items = inventory()
    ledger = read_ledger(args.ledger)
    if args.check:
        errors = check(items, ledger)
        print("\n".join(errors) or f"The ledger matches the Python app's {len(items)} items")
        if errors:
            print(f"\n{len(errors)} problems. inventory.py --update adds new items as todo.", file=sys.stderr)
        return 1 if errors else 0
    if args.update:
        added, removed, kept = update(items, ledger)
        write_ledger(ledger, items, args.ledger)
        print(f"Added {added} new items as todo, dropped {removed} gone ones that were still todo")
        for k in kept:
            print(f"gone from the Python app, but the ledger says more than todo, so it stays: {k}")
        return 0
    if args.report:
        print(report(items, ledger))
        return 0
    for item in items:
        print(f"{item.id}\t{item.area}")
    counts = Counter(i.kind for i in items)
    print(", ".join(f"{n} {k}" for k, n in counts.items()), file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
