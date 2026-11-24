#!/usr/bin/env python3
"""Draft a docs/device-compatibility.md row from the phone app's Copy diagnostics text (stdin or files)."""

import argparse
import re
import sys
from dataclasses import dataclass, field

DEVICE = re.compile(r"^Device: (.+?), Android (\S+) \(SDK \d+\)", re.M)
APP = re.compile(r"^App version: (\S+)", re.M)
CODECS = re.compile(r"^Phone codecs: (.+)$", re.M)
CAMERA = re.compile(r"^\s*(\d+)(?: \(in (\d+)\))? (Back|Front|Ext|\?) (?:~(\d+)mm|\?)(?: OIS)?(?: \[(auto|phys)\])?: (.+)$")
TELESCOPE_MAX_FPS = 60  # the desktop's FPS setting tops out here


@dataclass
class Lens:
    """One camera line of the diagnostics."""

    facing: str
    focal: int
    kind: str
    max_size: tuple[int, int] | None = None
    fps: list[int] = field(default_factory=list)
    lens_zooms: str = ""
    manual: set[str] = field(default_factory=set)


def parse_cameras(text: str) -> list[Lens]:
    """Every camera line under the first Cameras: heading."""
    lenses = []
    section = text.split("Cameras:", 1)[1] if "Cameras:" in text else ""
    for line in section.splitlines()[1:] if section else []:
        if not line.strip():
            break
        m = CAMERA.match(line)
        if not m:
            continue
        rest = m.group(6)
        lens = Lens(m.group(3), int(m.group(4) or 0), m.group(5) or "")
        if size := re.search(r"max (\d+)x(\d+)", rest):
            lens.max_size = (int(size.group(1)), int(size.group(2)))
        if fps := re.search(r"fps ((?:\d+-\d+ ?)+)", rest):
            lens.fps = [int(r.split("-")[1]) for r in fps.group(1).split()]
        if zooms := re.search(r"lenses at ([\d.x, ]+?)(?:, [A-Za-z]|$)", rest):
            lens.lens_zooms = zooms.group(1).strip()
        if manual := re.search(r"manual ([a-zA-Z/]+)", rest):
            lens.manual = set(manual.group(1).split("/"))
        lenses.append(lens)
    return lenses


def main_back(lenses: list[Lens]) -> Lens | None:
    """The first back camera listed, which phones make their main one."""
    return next((lens for lens in lenses if lens.facing == "Back"), None)


def lens_names(lenses: list[Lens]) -> str:
    """Back lenses by kind (ultrawide, main, 3x) and how many front ones, each focal length counted once."""
    main = main_back(lenses)
    back = sorted({lens.focal for lens in lenses if lens.facing == "Back"})
    front = {lens.focal for lens in lenses if lens.facing == "Front"}
    names = []
    for focal in back:
        if main and focal == main.focal or not focal:
            names.append("main")
        elif main and main.focal and focal < main.focal * 0.8:
            names.append("ultrawide")
        elif main and main.focal:
            names.append(f"{focal / main.focal:.1f}x")
        else:
            names.append(f"{focal} mm")
    if front:
        names.append(f"{len(front)} front" if len(front) > 1 else "front")
    return ", ".join(dict.fromkeys(names)) or "➖"


def top_fps(lens: Lens | None) -> str:
    """The highest frame rate the lens offers, up to what Telescope can ask for."""
    usable = [fps for fps in (lens.fps if lens else []) if fps <= TELESCOPE_MAX_FPS]
    return f"{max(usable)} fps" if usable else "➖"


def manual_cell(lens: Lens | None) -> str:
    """✅ when the main lens takes manual exposure, white balance and focus, else which of them it takes."""
    if not lens:
        return "➖"
    have = [name for key, name in (("exposure", "exposure"), ("WB", "WB"), ("focus", "focus")) if key in lens.manual]
    if len(have) == 3:
        return "✅"
    return ", ".join(have) + " only" if have else "❌"


def photo_cell(lens: Lens | None) -> str:
    """The main lens's largest JPEG in megapixels, rounded down like phone makers' own figures."""
    if not lens or not lens.max_size:
        return "➖"
    w, h = lens.max_size
    return f"{w * h // 1_000_000} MP"


def row(text: str) -> str:
    """The Markdown row for one phone."""
    device = DEVICE.search(text)
    app = APP.search(text)
    lenses = parse_cameras(text)
    main = main_back(lenses)
    auto = "n/a"
    zoomed = [lens.lens_zooms for lens in lenses if lens.facing == "Back" and lens.lens_zooms]
    if zoomed:
        auto = f"➖ (lenses at {zoomed[0]})"
    codecs = CODECS.search(text)
    light = "➖"
    if codecs and codecs.group(1).strip() != "unknown":
        light = "➖ (offered)" if "h264" in codecs.group(1) else "❌"
    cells = [
        device.group(1)[:1].upper() + device.group(1)[1:] if device else "➖",
        device.group(2) if device else "➖",
        (app.group(1) if app else "➖") if "Stream started:" in text else "not streamed yet",
        lens_names(lenses),
        auto,
        top_fps(main),
        manual_cell(main),
        light,
        photo_cell(main),
        "➖",
    ]
    return "| " + " | ".join(cells) + " |"


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("files", nargs="*", help="files with the pasted diagnostics (stdin when none)")
    args = parser.parse_args()
    text = "\n".join(open(f, encoding="utf-8").read() for f in args.files) if args.files else sys.stdin.read()
    if not DEVICE.search(text):
        sys.exit("No 'Device:' line found. Paste the text from Copy diagnostics in the phone app's About section.")
    print(row(text))
    print("\nFill in Auto zoom, Light and Microphone from what the stream did, mark Lenses ⚠️ if the phone has "
          "a lens that's missing here, and put the app version under Tested with only once it has streamed.",
          file=sys.stderr)


if __name__ == "__main__":
    main()
