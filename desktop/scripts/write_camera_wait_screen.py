#!/usr/bin/env python3
"""Write the Windows 11 camera's wait screen, which its DLL shows while Telescope sends nothing (wait_screen.cpp).

Renders the default screen's lens and name with vcam.py, so the DLL needs no image decoding or text drawing: the lens
as NV12 on the app's background, the name as an 8-bit mask the DLL tints. Both at a fixed scale, which the DLL resizes
for the size an app picks. No "Waiting for camera" line: the camera can't know the user's language.

Usage: python scripts/write_camera_wait_screen.py <output.bin>  (virtualcamera/CMakeLists.txt runs it)
"""

import os
import struct
import sys
from pathlib import Path

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")
sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import cv2  # noqa: E402
import numpy as np  # noqa: E402

MAGIC = 0x53575354  # "TSWS"
VERSION = 1
LENS_SCALE = 1.4  # pixels per banner unit: the lens is sharp up to 1080p and resized up beyond
NAME_SCALE = 3.0  # the name's edges show more, so it's kept sharper
NAME_ROOM = 1000  # banner units across the canvas the name is drawn on, before its mask is cut to it


def nv12(rgb: np.ndarray) -> tuple:
    """(Y, interleaved UV) as vcam.MediaFoundationCamera.send converts frames."""
    h, w = rgb.shape[:2]
    planar = cv2.cvtColor(np.ascontiguousarray(rgb), cv2.COLOR_RGB2YUV_I420).reshape(-1)
    luma, quarter = w * h, w * h // 4
    uv = np.empty((h // 2, w // 2, 2), np.uint8)
    uv[..., 0] = planar[luma:luma + quarter].reshape(h // 2, w // 2)
    uv[..., 1] = planar[luma + quarter:].reshape(h // 2, w // 2)
    return planar[:luma].reshape(h, w), uv


def solid(hex_: str) -> tuple:
    from PyQt6.QtGui import QColor
    c = QColor(hex_)
    y, uv = nv12(np.full((2, 2, 3), (c.red(), c.green(), c.blue()), np.uint8))
    return int(y[0, 0]), int(uv[0, 0, 0]), int(uv[0, 0, 1])


def canvas(width: int, height: int, fill: str):
    from PyQt6.QtGui import QColor, QImage, QPainter
    image = QImage(width, height, QImage.Format.Format_RGB888)
    image.fill(QColor(fill))
    p = QPainter(image)
    p.setRenderHint(QPainter.RenderHint.Antialiasing)
    p.setRenderHint(QPainter.RenderHint.TextAntialiasing)
    return image, p


def build() -> bytes:
    from PyQt6.QtWidgets import QApplication
    from telescope import vcam
    from telescope.theme import BG, TEXT
    app = QApplication.instance() or QApplication([])  # noqa: F841

    size = 2 * round(vcam.SCREEN_REACH * LENS_SCALE)
    image, p = canvas(size, size, BG)
    vcam.paint_lens(p, (size / 2, size / 2), LENS_SCALE, size, size)
    p.end()
    lens_y, lens_uv = nv12(vcam._to_rgb(image))

    room, name_h = round(NAME_ROOM * NAME_SCALE), round(vcam.SCREEN_NAME_HEIGHT * NAME_SCALE)
    image, p = canvas(room, name_h, "#000000")
    vcam.paint_name(p, 0, NAME_SCALE, room, "#ffffff")
    p.end()
    mask = vcam._to_rgb(image)[..., 0]
    used = np.flatnonzero(mask.any(axis=0))
    if not len(used) or used[0] == 0 or used[-1] == room - 1:
        raise SystemExit("The name doesn't fit its canvas: raise NAME_ROOM")
    reach = max(room // 2 - used[0], used[-1] + 1 - room // 2) + 2  # cut evenly, so the mask stays centred on the name
    mask = np.ascontiguousarray(mask[:, room // 2 - reach:room // 2 + reach])
    name_w = mask.shape[1]

    header = struct.pack(
        "<22I", MAGIC, VERSION, *solid(BG), *solid(TEXT),
        round(vcam.SCREEN_WIDTH_SHARE * 1000), round(vcam.SCREEN_HEIGHT_SHARE * 1000), vcam.SCREEN_WIDTH,
        vcam.SCREEN_LENS, vcam.SCREEN_WITH_NAME, vcam.SCREEN_CENTRE, vcam.SCREEN_NAME_TOP, vcam.SCREEN_NAME_PX,
        vcam.SCREEN_NAME_MIN_PX, size, round(LENS_SCALE * 1000), name_w, name_h, round(NAME_SCALE * 1000))
    return header + lens_y.tobytes() + lens_uv.tobytes() + mask.tobytes()


def main():
    if len(sys.argv) != 2:
        raise SystemExit(__doc__)
    out = Path(sys.argv[1])
    out.parent.mkdir(parents=True, exist_ok=True)
    data = build()
    tmp = out.with_suffix(".tmp")
    tmp.write_bytes(data)
    tmp.replace(out)
    print(f"Wrote {out} ({len(data) // 1024} KB)")


if __name__ == "__main__":
    main()
