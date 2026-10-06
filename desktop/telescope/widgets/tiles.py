"""The row of tiles shown while several streams run: a small live picture of each, which virtual camera it goes to and
how it's doing. Clicking one points the panels at it; its x stops just that stream."""

from typing import Optional

import cv2
import numpy as np
from PyQt6.QtCore import QSize, Qt, pyqtSignal
from PyQt6.QtGui import QImage, QPixmap
from PyQt6.QtWidgets import QFrame, QHBoxLayout, QLabel, QPushButton, QVBoxLayout, QWidget

from telescope import theme
from telescope.widgets.common import create_vector_icon, set_status_kind, ui_px

THUMB_W, THUMB_H = 128, 72


def thumbnail(frame: np.ndarray, width: int = THUMB_W, height: int = THUMB_H) -> QPixmap:
    """A BGR frame shrunk to fit width x height."""
    h, w = frame.shape[:2]
    scale = min(width / w, height / h)
    small = cv2.resize(frame, (max(1, int(w * scale)), max(1, int(h * scale))), interpolation=cv2.INTER_AREA)
    small = np.ascontiguousarray(small)
    sh, sw = small.shape[:2]
    return QPixmap.fromImage(QImage(small.data, sw, sh, sw * 3, QImage.Format.Format_BGR888).copy())


class _Tile(QFrame):
    clicked = pyqtSignal()

    def __init__(self, on_stop):
        super().__init__()
        self.setObjectName("stream_tile")
        self.setCursor(Qt.CursorShape.PointingHandCursor)
        self.last_frame = None  # the frame its picture shows, so an unchanged one isn't shrunk again
        lay = QHBoxLayout(self)
        lay.setContentsMargins(8, 8, 6, 8)
        lay.setSpacing(10)
        self.thumb = QLabel()
        self.thumb.setObjectName("stream_tile_thumb")
        self.thumb.setFixedSize(ui_px(THUMB_W), ui_px(THUMB_H))
        self.thumb.setAlignment(Qt.AlignmentFlag.AlignCenter)
        lay.addWidget(self.thumb)
        text = QVBoxLayout()
        text.setSpacing(2)
        self.name = QLabel()
        self.name.setObjectName("stream_tile_name")
        self.output = QLabel()
        self.output.setObjectName("stream_tile_out")
        self.state = QLabel()
        text.addWidget(self.name)
        text.addWidget(self.output)
        text.addWidget(self.state)
        text.addStretch()
        lay.addLayout(text)
        self.stop_btn = QPushButton()
        self.stop_btn.setObjectName("icon_btn")
        self.stop_btn.setFixedSize(26, 26)
        self.stop_btn.setIcon(create_vector_icon("close", theme.TEXT_DIM))
        self.stop_btn.setIconSize(QSize(12, 12))
        self.stop_btn.clicked.connect(on_stop)
        lay.addWidget(self.stop_btn, 0, Qt.AlignmentFlag.AlignTop)

    def mousePressEvent(self, event):
        if event.button() == Qt.MouseButton.LeftButton:
            self.clicked.emit()
        super().mousePressEvent(event)


class StreamTiles(QWidget):
    focus_requested = pyqtSignal(str)
    stop_requested = pyqtSignal(str)

    def __init__(self):
        super().__init__()
        self.setObjectName("stream_tiles")
        self._lay = QHBoxLayout(self)
        self._lay.setContentsMargins(16, 12, 16, 0)
        self._lay.setSpacing(10)
        self._lay.addStretch()
        self._tiles: dict = {}
        self.setVisible(False)

    def show_streams(self, entries: list):
        """entries: (id, name, camera it goes to, status text, status kind, whether the panels show it), in order."""
        ids = [e[0] for e in entries]
        for sid in [s for s in self._tiles if s not in ids]:
            self._tiles.pop(sid).deleteLater()
        for i, (sid, name, output, state, kind, focused) in enumerate(entries):
            tile = self._tiles.get(sid)
            if tile is None:
                tile = self._tiles[sid] = _Tile(lambda _=False, s=sid: self.stop_requested.emit(s))
                tile.clicked.connect(lambda s=sid: self.focus_requested.emit(s))
                tile.thumb.setText("…")
            if self._lay.indexOf(tile) != i:
                self._lay.insertWidget(i, tile)
            tile.name.setText(name)
            tile.output.setText(f"to {output}")
            tile.state.setText(f"● {state}")
            set_status_kind(tile.state, kind)
            tile.stop_btn.setToolTip(f"Stop streaming {name}")
            tile.setToolTip("" if focused else "Show this one's settings")
            if tile.property("focused") != focused:
                tile.setProperty("focused", focused)
                tile.style().unpolish(tile)
                tile.style().polish(tile)
        self.setVisible(len(entries) > 1)

    def set_frame(self, sid: str, frame: Optional[np.ndarray]):
        tile = self._tiles.get(sid)
        if tile is not None and frame is not None and frame is not tile.last_frame:
            tile.last_frame = frame
            tile.thumb.setPixmap(thumbnail(frame, tile.thumb.width(), tile.thumb.height()))

    def ids(self) -> list:
        return list(self._tiles)
