"""Writes every icon the Python app draws as an SVG in ui/icons (white, tinted by Icon) and lists them in icons.slint.

    python make_icons.py

Line icons (widgets/line_icons.py) keep their names; the older vector icons (create_vector_icon in widgets/common.py)
get a v_ prefix.
"""

import re
import sys
from pathlib import Path

KIT = Path(__file__).resolve().parents[1] / "ui"
DESKTOP = Path(__file__).resolve().parents[4] / "desktop"


def main() -> int:
    sys.path.insert(0, str(DESKTOP))
    from telescope.widgets.common import _ICON_SVG
    from telescope.widgets.line_icons import LINE_ICONS
    icons = {}
    for name, body in LINE_ICONS.items():
        icons[name] = ('<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="#ffffff" '
                       'stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round">'
                       + body.replace("currentColor", "#ffffff") + "</svg>")
    for name, body in _ICON_SVG.items():
        icons[f"v_{name}"] = ('<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#ffffff" '
                              'stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round">'
                              + body.replace("{c}", "#ffffff") + "</svg>")
    for name, svg in icons.items():
        (KIT / "icons" / f"{name}.svg").write_text(svg + "\n")
    names = sorted(icons)
    slint = lambda n: n.replace("_", "-")
    props = "\n".join(f'    out property <image> {slint(n)}: @image-url("icons/{n}.svg");' for n in names)
    lines = [f'name == "{n}" ? {slint(n)}' for n in names if not n.startswith("v_")]
    named = "\n            : ".join(lines)
    (KIT / "icons.slint").write_text(
        "// The icons (widgets/line_icons.py, and widgets/common.py's vector icons with a v- prefix), white in the files and\n"
        "// tinted by Icon. Cards pick one by property or by name. tools/make_icons.py writes this file.\n\n"
        "export global Icons {\n" + props + "\n\n"
        "    // An icon by its name in PluginInfo.icon, for cards described as data\n"
        "    public pure function named(name: string) -> image {\n"
        f"        {named}\n            : sliders\n    }}\n}}\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
