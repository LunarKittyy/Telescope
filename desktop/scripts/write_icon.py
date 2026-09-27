#!/usr/bin/env python3
"""Write the app icon (widgets.common.create_app_icon) as a Windows .ico, for the exe (resources/telescope.ico).
Run it again when the icon changes.

Usage: python scripts/write_icon.py resources/telescope.ico

Each size is drawn on its own, so small ones stay crisp, and stored as PNG (what .ico files hold since Vista).
"""

import os
import struct
import sys
from pathlib import Path

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")
sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from PyQt6.QtCore import QBuffer, QByteArray, QIODevice  # noqa: E402
from PyQt6.QtGui import QGuiApplication  # noqa: E402

from telescope.widgets.common import create_app_icon  # noqa: E402

SIZES = (16, 20, 24, 32, 40, 48, 64, 128, 256)


def png(size: int) -> bytes:
    data = QByteArray()
    buf = QBuffer(data)
    buf.open(QIODevice.OpenModeFlag.WriteOnly)
    create_app_icon(size).pixmap(size, size).toImage().save(buf, "PNG")
    buf.close()
    return bytes(data)


def ico(images: dict) -> bytes:
    """An .ico holding each {size: PNG bytes}."""
    header = struct.pack("<HHH", 0, 1, len(images))
    entries, blobs = b"", b""
    offset = len(header) + 16 * len(images)
    for size, data in images.items():
        dim = 0 if size >= 256 else size  # 0 means 256
        entries += struct.pack("<BBBBHHII", dim, dim, 0, 0, 1, 32, len(data), offset + len(blobs))
        blobs += data
    return header + entries + blobs


def main():
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    app = QGuiApplication([])  # noqa: F841 (pixmaps need one)
    out = Path(sys.argv[1])
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_bytes(ico({size: png(size) for size in SIZES}))
    print(f"Wrote {out}")


if __name__ == "__main__":
    main()
