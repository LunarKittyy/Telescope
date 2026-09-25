"""QR code painted with QPainter (no Pillow), with the white quiet zone phone cameras need."""

import qrcode
from PyQt6.QtCore import Qt
from PyQt6.QtGui import QBrush, QColor, QPainter
from PyQt6.QtWidgets import QWidget


class QRCodeWidget(QWidget):
    def __init__(self, data: str, module_px: int = 6, parent=None):
        super().__init__(parent)
        qr = qrcode.QRCode(error_correction=qrcode.constants.ERROR_CORRECT_L, box_size=1, border=0)
        qr.add_data(data)
        qr.make(fit=True)
        self._matrix = qr.modules
        self._cell = module_px
        # qrcode's border param doesn't affect .modules, so the quiet zone (4 modules) is added here.
        self._margin = module_px * 4
        side = len(self._matrix) * module_px + self._margin * 2
        self.setFixedSize(side, side)

    def paintEvent(self, event):
        painter = QPainter(self)
        painter.fillRect(self.rect(), QColor("white"))
        painter.setPen(Qt.PenStyle.NoPen)
        painter.setBrush(QBrush(QColor("black")))
        cell, margin = self._cell, self._margin
        for row, line in enumerate(self._matrix):
            for col, dark in enumerate(line):
                if dark:
                    painter.drawRect(margin + col * cell, margin + row * cell, cell, cell)
        painter.end()
