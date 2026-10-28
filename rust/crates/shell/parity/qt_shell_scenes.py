"""Qt renders of the window with only the plugins the Rust shell has ported, plus where each widget sits, so the shell
can be compared whole while the other cards are still Python only.

    python qt_shell_scenes.py OUT [--plugins camera_control,onboarding,preview,setup] [--scale 1]

Writes OUT/<scale>x/<scene>.png and OUT/<scale>x/<scene>.json (object name, class, text and rect of every visible
widget, in window coordinates). Uses the translation preview's scene helpers, so nothing in desktop/ changes.
"""

import argparse
import json
import os
import sys
from contextlib import ExitStack
from pathlib import Path

DESKTOP = Path(__file__).resolve().parents[4] / "desktop"
PORTED = "camera_control,onboarding,preview,setup"


def geometry(win):
    from PyQt6.QtCore import QPoint
    from PyQt6.QtWidgets import QAbstractButton, QLabel, QWidget
    out = []
    for w in [win, *win.findChildren(QWidget)]:
        if not w.isVisibleTo(win) and w is not win:
            continue
        p = w.mapTo(win, QPoint(0, 0)) if w is not win else QPoint(0, 0)
        text = w.text() if isinstance(w, (QLabel, QAbstractButton)) else ""
        out.append({"name": w.objectName(), "class": type(w).__name__, "text": text,
                    "rect": [p.x(), p.y(), w.width(), w.height()]})
    return out


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("out", type=Path)
    parser.add_argument("--plugins", default=PORTED)
    parser.add_argument("--scale", default="1")
    args = parser.parse_args(argv)
    os.environ["QT_QPA_PLATFORM"] = "offscreen"
    os.environ["QT_SCALE_FACTOR"] = args.scale
    sys.path.insert(0, str(DESKTOP))
    from PyQt6.QtWidgets import QApplication
    from telescope import theme
    app = QApplication.instance() or QApplication(["telescope"])
    theme.apply_theme(app)
    from scripts import translation_scenes as scenes
    from telescope.app import TelescopeWindow
    from telescope.plugins import builtin_plugins
    wanted = args.plugins.split(",")
    folder = args.out / f"{args.scale}x"
    folder.mkdir(parents=True, exist_ok=True)
    for name, width in (("shell-wide", 1300), ("shell-medium", 900), ("shell-narrow", 560)):
        with ExitStack() as stack:
            scenes.use_scratch_profile(stack)
            scenes.quiet_system(stack, False)
            win = TelescopeWindow()
            for plugin in builtin_plugins():
                if plugin.name in wanted:
                    win.register_plugin(scenes.quiet_plugin(plugin))
            win.apply_saved_config()
            QApplication.processEvents()
            scenes.grow(win, width, 900)
            QApplication.processEvents()
            win.grab().save(str(folder / f"{name}.png"))
            (folder / f"{name}.json").write_text(json.dumps(geometry(win), indent=1))
            scenes.close_window(win)
    return 0


if __name__ == "__main__":
    sys.exit(main())
