#!/usr/bin/env python3
"""Check a plugin you're writing without starting Telescope: its files and plugin.json, that it loads and starts next
to the built-in plugins, makes its card, handles frames, saves and restores its settings and gets through a stream
starting and stopping. Every problem comes with the traceback. Runs in a throwaway profile, so your settings and
installed plugins are left alone. See docs/writing-plugins.md.

Usage: python scripts/check_plugin.py FOLDER
"""

import json
import logging
import os
import shutil
import sys
import tempfile
import time
import traceback
from pathlib import Path

DESKTOP = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(DESKTOP))

_FAILURES: list = []
_FRAME_BUDGET_MS = 8  # a 30 fps stream has 33 ms a frame, shared with decoding and every other plugin


def _ok(text: str):
    print(f"[ok]   {text}")


def _note(text: str):
    print(f"[note] {text}")


class _Problem(Exception):
    """Something this script found wrong, which needs no traceback to explain."""


def _fail(text: str, exc: BaseException = None):
    _FAILURES.append(text)
    if isinstance(exc, _Problem):
        print(f"[FAIL] {text}: {exc}")
        return
    print(f"[FAIL] {text}")
    if exc is not None:
        tb = traceback.TracebackException.from_exception(exc)
        tb.stack = traceback.StackSummary.from_list([f for f in tb.stack if f.filename != __file__])  # just the plugin
        _indented("".join(tb.format()))


def _indented(text: str):
    print("       " + text.rstrip().replace("\n", "\n       "))


def _step(text: str, fn):
    """Run one check; a plugin that raises fails it with the traceback. Returns what fn returned, or None."""
    try:
        result = fn()
    except BaseException as exc:  # noqa: BLE001 (a plugin's sys.exit() is a failure to report too)
        _fail(text, exc)
        return None
    _ok(text)
    return result


class _Logged(logging.Handler):
    """Errors Telescope logged about the plugin (it catches what a hook raises and carries on)."""

    def __init__(self, name: str, folder: Path):
        super().__init__(logging.ERROR)
        self.name, self.folder, self.records = name, str(folder), []

    def emit(self, record):
        text = record.getMessage() + ("\n" + logging.Formatter().formatException(record.exc_info) if record.exc_info else "")
        if "can't be saved" in text:
            return  # the settings check says so already, more precisely
        if self.name in record.getMessage() or self.folder in text:
            self.records.append(text)

    def take(self) -> list:
        records, self.records = self.records, []
        return records


class _Ctrl:
    """Stands in for the phone's control client in on_stream_start: it takes commands and has no state to give."""
    base, auth = "http://127.0.0.1:9/", None

    def send(self, **_params):
        return None

    def get_state(self):
        return None

    def close(self):
        pass


def _frame():
    import numpy as np
    x = np.linspace(0, 255, 1280, dtype=np.uint8)
    frame = np.dstack([np.tile(x, (720, 1)), np.tile(x[::-1], (720, 1)), np.full((720, 1280), 128, np.uint8)])
    return np.ascontiguousarray(frame)


def check_files(folder: Path, reserved: set):
    from telescope import plugin_loader
    item = plugin_loader._inspect(folder, reserved, {}, False, dev=True)
    if item.problem:
        _fail(f"Files and {plugin_loader.MANIFEST}: {item.problem}")
        return None
    _ok(f"Files and {plugin_loader.MANIFEST}: {item.title} {item.version}".rstrip())
    try:
        plugin_loader.digest(folder)
    except ValueError as exc:
        _note(f"Before you share it: {exc} (hidden folders like .git count once it's installed from a .zip)")
    return item


def check_translations(folder: Path):
    """Each locale/<lang>.po can be read, and no translation has a {field} the English lacks."""
    from telescope import i18n
    for po in sorted((folder / i18n.PLUGIN_LOCALE).glob("*.po")):
        try:
            entries, header = i18n.read_po(po)
        except (OSError, ValueError) as exc:
            _fail(f"Translation {po.name}: couldn't be read, so the plugin stays in English ({exc})")
            continue
        bad = [e.msgid for e in entries for text in e.msgstr
               if not i18n.fields(text) <= i18n.fields(e.msgid) | i18n.fields(e.plural)]
        if bad:
            _fail(f"Translation {po.name}: a {{field}} the English doesn't have, so these show in English: "
                  + ", ".join(repr(b) for b in bad[:5]))
            continue
        _ok(f"Translation {po.name}: {sum(1 for e in entries if e.msgstr and all(e.msgstr) and not e.fuzzy)} of "
            f"{len(entries)} texts")


def check_running(folder: Path, name: str, log: _Logged):
    from PyQt6 import sip
    from PyQt6.QtWidgets import QApplication

    from telescope.app import TelescopeWindow

    app = QApplication.instance() or QApplication([])
    win = TelescopeWindow()
    try:
        _check_window(win, name, log, app)
    finally:
        win._shut_down()  # stop the wait screen's drawing thread before Qt destroys its resources
        app.processEvents()
        # The window has reference cycles. Destroy it while QApplication is still alive, including on early returns.
        sip.delete(win)
        _logged("Quitting", log)


def _check_window(win, name: str, log: _Logged, app):
    import numpy as np
    from telescope.plugin import TelescopePlugin
    from telescope.plugins import builtin_plugins

    notified = []
    win.send_notification = lambda title, body, urgent=True: notified.append(f"{title}: {body}")  # not on your desktop
    win.load_plugins(builtin_plugins())
    win.apply_saved_config()
    app.processEvents()
    plugin = win._plugin(name)
    if plugin is None:
        _fail(f"Starting it: {win._plugin_problems.get(name, 'it was left out')}")
        for record in log.take():
            _indented(record)
        return
    _ok(f"Starting it next to the built-in plugins ({len(win._plugins) - 1} of them)")
    _logged("Setting it up", log)
    if type(plugin).create_panel is not TelescopePlugin.create_panel:
        _ok("Making its card")

    frame = _frame()

    def frames():
        out = None
        start = time.perf_counter()
        for _ in range(30):
            out = plugin.process_frame(frame.copy())
        per_frame = (time.perf_counter() - start) / 30 * 1000
        if not isinstance(out, np.ndarray) or out.dtype != np.uint8 or out.ndim != 3 or out.shape[2] != 3:
            raise _Problem(f"process_frame() has to return a BGR frame (uint8, height x width x 3), not "
                            f"{type(out).__name__} {getattr(out, 'shape', '')} {getattr(out, 'dtype', '')}".rstrip())
        if per_frame > _FRAME_BUDGET_MS:
            _note(f"process_frame() takes {per_frame:.1f} ms a frame at 1280x720, which can make streams fall "
                  f"behind. Try to stay under {_FRAME_BUDGET_MS} ms.")
        step = plugin.frame_step()
        if step is not None:
            step(frame.copy())
        return per_frame
    per_frame = _step("process_frame() and frame_step() on a 1280x720 frame", frames)
    if per_frame is not None:
        print(f"       {per_frame:.2f} ms a frame")

    def settings():
        cfg = plugin.get_config()
        if not isinstance(cfg, dict):
            raise _Problem(f"get_config() has to return a dict, not {type(cfg).__name__}")
        try:
            saved = json.loads(json.dumps(cfg))
        except (TypeError, ValueError) as exc:
            raise _Problem(f"get_config() has to return what JSON can hold (dicts, lists, text, numbers, "
                            f"True/False/None): {exc}") from None
        plugin.set_config(saved)
        if plugin.get_config() != saved:
            raise _Problem(f"After set_config(get_config()) it gave back {plugin.get_config()!r} "
                                 f"instead of {saved!r}")
        plugin.set_config({})  # a first start, or settings from before it had any
        plugin.set_config(saved)
    _step("Saving and restoring its settings, and starting from none", settings)
    _logged("Settings", log)

    def stream():
        ctrl = _Ctrl()
        plugin.on_stream_starting()
        plugin.on_stream_start(ctrl.base, ctrl)
        plugin.process_frame(frame.copy())
        plugin.on_focus_lost()
        plugin.on_focus_gained(ctrl.base, ctrl)
        plugin.on_stream_stop()
        plugin.on_plugins_ready()
        plugin.diagnostics()
    _step("A stream starting, moving between tiles and stopping", stream)
    _logged("Stream hooks", log)

    for text in notified:
        _note(f"Telescope would have shown this notification: {text}")


def _logged(what: str, log: _Logged):
    records = log.take()
    if records:
        _fail(f"{what}: Telescope caught an error from it and carried on")
        for record in records:
            _indented(record)


def main(argv) -> int:
    if len(argv) != 1 or argv[0] in ("-h", "--help"):
        print(__doc__.strip())
        return 2
    folder = Path(argv[0]).expanduser().resolve()
    if not (folder / "plugin.json").is_file():
        print(f"{argv[0]} isn't a plugin's folder (the one with plugin.json in it)", file=sys.stderr)
        return 2
    profile = Path(tempfile.mkdtemp(prefix="telescope-check-plugin-"))
    os.environ.update({"QT_QPA_PLATFORM": os.environ.get("QT_QPA_PLATFORM", "offscreen"),
                       "TELESCOPE_DEV_PROFILE": str(profile), "TELESCOPE_DEV_PLUGINS": str(folder)})
    os.environ.pop("TELESCOPE_DEV_SUPERVISED", None)
    try:
        logging.getLogger().setLevel(logging.ERROR)  # the plugin's problems are printed below, with their tracebacks
        from telescope.plugins import builtin_plugins
        item = check_files(folder, {p.name for p in builtin_plugins()})
        check_translations(folder)
        if item is not None:
            log = _Logged(item.name, folder)
            logging.getLogger().addHandler(log)
            check_running(folder, item.name, log)
    finally:
        shutil.rmtree(profile, ignore_errors=True)
    if _FAILURES:
        print(f"\n{len(_FAILURES)} problem{'s' if len(_FAILURES) > 1 else ''} to fix")
        return 1
    print("\nAll good. Try it for real with: python scripts/dev_desktop.py --plugin " + str(folder))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
