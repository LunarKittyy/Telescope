#!/usr/bin/env python3
"""Write the Windows start-up splash (resources/splash.png), the picture PyInstaller's launcher shows before Python starts.
Run it again when the icon or the colours change. The status text isn't in the picture: telescope.spec places it.

Usage: python scripts/write_splash.py resources/splash.png
"""

import os
import sys
from pathlib import Path

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")
sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from PyQt6.QtCore import QRectF, Qt  # noqa: E402
from PyQt6.QtGui import QColor, QFont, QGuiApplication, QImage, QPainter, QPen  # noqa: E402

from telescope import theme  # noqa: E402
from telescope.widgets.common import create_app_icon  # noqa: E402

WIDTH, HEIGHT = 420, 180
ICON = 44
MARGIN = 28  # telescope.spec puts the text at this margin from the left and bottom


def draw() -> QImage:
    image = QImage(WIDTH, HEIGHT, QImage.Format.Format_RGB32)
    image.fill(QColor(theme.BG))
    p = QPainter(image)
    p.setRenderHint(QPainter.RenderHint.Antialiasing)
    p.setPen(QPen(QColor(theme.BORDER_STRONG), 1))
    p.drawRect(QRectF(0.5, 0.5, WIDTH - 1, HEIGHT - 1))
    p.drawPixmap(MARGIN, MARGIN, create_app_icon(ICON).pixmap(ICON, ICON))
    font = QFont("Segoe UI")
    font.setPixelSize(22)
    font.setWeight(QFont.Weight.DemiBold)
    p.setFont(font)
    p.setPen(QColor(theme.TEXT))
    p.drawText(QRectF(MARGIN + ICON + 14, MARGIN, WIDTH, ICON), Qt.AlignmentFlag.AlignVCenter, "Telescope")
    p.end()
    return image


def main():
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    app = QGuiApplication(sys.argv[:1])  # noqa: F841 (fonts and pixmaps need it)
    if not draw().save(sys.argv[1], "PNG"):
        sys.exit(f"Couldn't write {sys.argv[1]}")


if __name__ == "__main__":
    main()
