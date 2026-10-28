"""What the app makes of each settings file in tests/fixtures/configs, so a rewrite can be held to the same result.

Each fixture folder has a telescope_config.json (none for a fresh install) and an expected.json, which says what
the real window did with it: whether the file was set aside as unreadable, which plugins came up, and the
settings it saves straight after starting. It runs offscreen with every built-in plugin it would load, on
28 October 2026 and on a network where this computer is 192.168.1.10, so every machine gets the same result.
Values the app makes up on the spot (a new computer id, this computer's name, a new shortcut id) show as "<new>".

    python scripts/config_fixtures.py          # write expected.json again for every fixture
    python scripts/config_fixtures.py --check  # say which ones the app no longer matches, exit 1 if any

tests/test_config_fixtures.py runs the check. The expected files are recorded on Linux.
"""

import argparse
import contextlib
import json
import logging
import os
import shutil
import sys
from pathlib import Path
from unittest import mock

DESKTOP = Path(__file__).resolve().parent.parent
FIXTURES = DESKTOP / "tests" / "fixtures" / "configs"
CONFIG_NAME = "telescope_config.json"
EXPECTED_NAME = "expected.json"
NEW = "<new>"
ADDRESS = "192.168.1.10"  # this computer's only address while a fixture loads, the same on every machine
NOW = 1793188800.0  # 28 October 2026, for what the app forgets after a while (browsers not seen for 60 days)
_app = None  # Qt's application, kept for as long as the windows it made

sys.path.insert(0, str(DESKTOP))
os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")


def fixtures() -> list:
    return sorted(p for p in FIXTURES.iterdir() if p.is_dir())


def snapshot(folder: Path) -> dict:
    """What the real window does with folder's settings file."""
    from PyQt6.QtWidgets import QApplication
    import telescope.app as app_module
    from telescope import ip_utils
    from telescope import config
    from telescope.config import config_path
    from telescope.plugins import builtin_plugins
    from scripts.translation_scenes import close_window, quiet_plugin, quiet_system, use_scratch_profile

    global _app
    _app = QApplication.instance() or QApplication([])
    with contextlib.ExitStack() as stack:
        stack.enter_context(mock.patch.dict(os.environ))
        use_scratch_profile(stack)
        source = folder / CONFIG_NAME
        if source.exists():
            config_path().parent.mkdir(parents=True)
            shutil.copyfile(source, config_path())
        quiet_system(stack)
        stack.enter_context(mock.patch.object(app_module, "save_config", config.save_config))  # into the scratch profile
        home = ip_utils.PairingAddress(ADDRESS, "wlan0", "lan")
        stack.enter_context(mock.patch.object(ip_utils, "get_pairing_addresses", lambda: [home]))
        stack.enter_context(mock.patch("time.time", lambda: NOW))
        win = app_module.TelescopeWindow()
        stack.callback(close_window, win)
        win.load_plugins([quiet_plugin(p) for p in builtin_plugins()])
        win.apply_saved_config()
        QApplication.processEvents()
        win.save_now()
        set_aside = any(config_path().parent.glob(CONFIG_NAME + ".invalid-*"))
        plugins = sorted(p.name for p in win._plugins if p.name)
        saved = _read(config_path())
    return {"set_aside": set_aside, "plugins": plugins, "saved": _mark_new(saved, _read(source))}


def _read(path: Path):
    try:
        return json.loads(path.read_bytes().decode("utf-8-sig"))
    except (OSError, ValueError):
        return None


def _mark_new(cfg: dict, original) -> dict:
    """cfg with the values the app made up on the spot (they differ every run) shown as NEW."""
    old = original if isinstance(original, dict) else {}
    conn = cfg.get("plugin_configs", {}).get("connection", {})
    old_conn = old.get("plugin_configs", {}).get("connection", {}) if isinstance(old.get("plugin_configs"), dict) else {}
    for key in ("computer_id", "computer_name"):  # a new random id, and this computer's hostname
        if key in conn and conn[key] != old_conn.get(key):
            conn[key] = NEW
    old_ids = {b.get("id") for b in old.get("plugin_configs", {}).get("shortcuts", {}).get("bindings", [])
               if isinstance(b, dict)} if isinstance(old.get("plugin_configs"), dict) else set()
    for binding in cfg.get("plugin_configs", {}).get("shortcuts", {}).get("bindings", []):
        if binding["id"] not in old_ids:
            binding["id"] = NEW
    return cfg


def _dump(data: dict) -> str:
    return json.dumps(data, indent=2, sort_keys=True, ensure_ascii=False) + "\n"


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--check", action="store_true", help="compare instead of writing")
    args = parser.parse_args(argv)
    logging.disable(logging.CRITICAL)  # several fixtures are broken on purpose, and the app says so loudly
    differ = []
    for folder in fixtures():
        text = _dump(snapshot(folder))
        expected = folder / EXPECTED_NAME
        if not args.check:
            expected.write_text(text, encoding="utf-8")
        elif not expected.exists() or expected.read_text(encoding="utf-8") != text:
            differ.append(folder.name)
    for name in differ:
        print(f"{name}: the app no longer ends up with what {EXPECTED_NAME} says")
    return 1 if differ else 0


if __name__ == "__main__":
    sys.exit(main())
