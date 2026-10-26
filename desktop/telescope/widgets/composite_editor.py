"""The editor for Several cameras in one picture: a live preview to drag, resize and crop the streams on, and the
controls for borders, corners, the background and saved layouts. Every change goes out at once."""

from dataclasses import replace
from typing import Optional

from PyQt6.QtCore import QPointF, QRectF, Qt, QTimer, pyqtSignal
from PyQt6.QtGui import QColor, QImage, QPainter, QPen
from PyQt6.QtWidgets import (
    QCheckBox, QColorDialog, QComboBox, QFileDialog, QHBoxLayout, QInputDialog, QLabel, QLineEdit, QMessageBox, QPushButton,
    QScrollArea, QSizePolicy, QVBoxLayout, QWidget,
)

from telescope import composite, theme
from telescope.composite import Compositor, Layout, Tile
from telescope.widgets.common import (
    FlowLayout, NoScrollComboBox, NoScrollSlider, PlacedDialog, add_section_heading, button_row, dialog_buttons, dialog_header,
    dialog_layout, slider_row, ui_px, value_label, wrapped_note,
)
from telescope.i18n import N_, tr

_PICTURES = N_("Pictures (*.png *.jpg *.jpeg *.webp *.bmp)")
_HANDLES = ("tl", "t", "tr", "r", "br", "b", "bl", "l")
_HANDLE_PX = 8
_PREVIEW_MS = 66  # about 15 pictures a second is plenty to line things up by
_PREVIEW_MAX_W = 1280
_NUDGE_PX = 1  # arrow keys move the picked stream this many pixels of the picture (Shift: ten times as far)
_ZOOM_STEP = 1.1


class PictureView(QWidget):
    """The live preview, with the tiles' outlines on top to grab."""

    picked = pyqtSignal(int)  # the tile at this index was clicked (-1: none)
    edited = pyqtSignal()  # a tile was moved, resized or zoomed here

    def __init__(self, model, parent=None):
        super().__init__(parent)
        self._model = model
        self._compositor = Compositor()
        self._prepared = None  # (layout, size) the compositor was last prepared with
        self._image: Optional[QImage] = None
        self._pixels = None  # the array the image is drawn from, kept alive with it
        self.selected = -1
        self._drag: Optional[dict] = None
        self._guides: tuple = ((), ())  # snap lines to draw while dragging, in picture fractions
        self.setMinimumSize(ui_px(480), ui_px(270))
        self.setSizePolicy(QSizePolicy.Policy.Expanding, QSizePolicy.Policy.Expanding)
        self.setFocusPolicy(Qt.FocusPolicy.StrongFocus)
        self.setMouseTracking(True)
        self.setAccessibleName(tr("Picture preview"))
        self._timer = QTimer(self)
        self._timer.setInterval(_PREVIEW_MS)
        self._timer.timeout.connect(self.refresh)
        self._timer.start()

    # ── Geometry ──────────────────────────────────────────────────────────

    def area(self) -> QRectF:
        """Where the picture sits in the widget, as big as its shape allows."""
        w, h = self._model.size()
        r = QRectF(self.rect()).adjusted(1, 1, -1, -1)
        scale = min(r.width() / w, r.height() / h)
        aw, ah = w * scale, h * scale
        return QRectF(r.x() + (r.width() - aw) / 2, r.y() + (r.height() - ah) / 2, aw, ah)

    def _to_picture(self, pos: QPointF) -> tuple:
        a = self.area()
        return (pos.x() - a.x()) / a.width(), (pos.y() - a.y()) / a.height()

    def _tile_rect(self, tile: Tile) -> QRectF:
        a = self.area()
        return QRectF(a.x() + tile.x * a.width(), a.y() + tile.y * a.height(), tile.w * a.width(), tile.h * a.height())

    def _handle_points(self, rect: QRectF) -> dict:
        cx, cy = rect.center().x(), rect.center().y()
        return {"tl": rect.topLeft(), "t": QPointF(cx, rect.top()), "tr": rect.topRight(), "r": QPointF(rect.right(), cy),
                "br": rect.bottomRight(), "b": QPointF(cx, rect.bottom()), "bl": rect.bottomLeft(),
                "l": QPointF(rect.left(), cy)}

    def _hit(self, pos: QPointF) -> tuple:
        """(tile index, handle or "") under pos, the picked tile's handles first and then the frontmost tile."""
        tiles = self._model.layout().tiles
        reach = ui_px(_HANDLE_PX)
        if 0 <= self.selected < len(tiles):
            for name, point in self._handle_points(self._tile_rect(tiles[self.selected])).items():
                if abs(point.x() - pos.x()) <= reach and abs(point.y() - pos.y()) <= reach:
                    return self.selected, name
        for i in range(len(tiles) - 1, -1, -1):
            if self._tile_rect(tiles[i]).contains(pos):
                return i, ""
        return -1, ""

    # ── Drawing ───────────────────────────────────────────────────────────

    def refresh(self):
        if not self.isVisible():
            return
        a = self.area()
        dpr = self.devicePixelRatioF()
        pw = max(16, min(_PREVIEW_MAX_W, round(a.width() * dpr)))
        full_w, full_h = self._model.size()
        size = (pw, max(9, round(pw * full_h / full_w)))
        layout = self._model.layout()
        if self._prepared != (layout, size):
            self._compositor.prepare(layout, size)
            self._prepared = (layout, size)
        pixels = self._compositor.render(self._model.frames())
        if pixels is not None:
            self._pixels = pixels
            self._image = QImage(pixels.data, pixels.shape[1], pixels.shape[0], pixels.strides[0],
                                 QImage.Format.Format_BGR888)
        self.update()

    def paintEvent(self, _event):
        p = QPainter(self)
        p.setRenderHint(QPainter.RenderHint.Antialiasing)
        p.setRenderHint(QPainter.RenderHint.SmoothPixmapTransform)
        a = self.area()
        if self._image is not None:
            p.drawImage(a, self._image)
        p.setPen(QPen(QColor(theme.BORDER), 1))
        p.setBrush(Qt.BrushStyle.NoBrush)
        p.drawRect(a)
        live = set(self._model.frames())
        for i, tile in enumerate(self._model.layout().tiles):
            rect = self._tile_rect(tile)
            if tile.source_id not in live:
                p.setPen(QPen(QColor(theme.TEXT_DIM), 1, Qt.PenStyle.DashLine))
                p.drawRect(rect)
                p.drawText(rect, Qt.AlignmentFlag.AlignCenter | Qt.TextFlag.TextWordWrap,
                           tr("{name} isn't streaming", name=tile.name or tile.source_id))
            if i == self.selected:
                p.setPen(QPen(QColor(theme.ACCENT), 2))
                p.drawRect(rect)
                p.setBrush(QColor(theme.ACCENT))
                half = ui_px(_HANDLE_PX) / 2
                for point in self._handle_points(rect).values():
                    p.drawRect(QRectF(point.x() - half, point.y() - half, 2 * half, 2 * half))
                p.setBrush(Qt.BrushStyle.NoBrush)
        p.setPen(QPen(QColor(theme.ACCENT_SOFT), 1, Qt.PenStyle.DashLine))
        for x in self._guides[0]:
            p.drawLine(QPointF(a.x() + x * a.width(), a.top()), QPointF(a.x() + x * a.width(), a.bottom()))
        for y in self._guides[1]:
            p.drawLine(QPointF(a.left(), a.y() + y * a.height()), QPointF(a.right(), a.y() + y * a.height()))
        p.end()

    # ── Mouse and keys ────────────────────────────────────────────────────

    def mousePressEvent(self, event):
        index, handle = self._hit(event.position())
        self.select(index)
        if index < 0:
            return
        tile = self._model.layout().tiles[index]
        kind = "crop" if event.button() == Qt.MouseButton.RightButton else ("resize" if handle else "move")
        self._drag = {"kind": kind, "handle": handle, "start": self._to_picture(event.position()), "tile": tile,
                      "pos": event.position()}

    def mouseMoveEvent(self, event):
        if self._drag is None:
            index, handle = self._hit(event.position())
            cursors = {"tl": Qt.CursorShape.SizeFDiagCursor, "br": Qt.CursorShape.SizeFDiagCursor,
                       "tr": Qt.CursorShape.SizeBDiagCursor, "bl": Qt.CursorShape.SizeBDiagCursor,
                       "l": Qt.CursorShape.SizeHorCursor, "r": Qt.CursorShape.SizeHorCursor,
                       "t": Qt.CursorShape.SizeVerCursor, "b": Qt.CursorShape.SizeVerCursor}
            self.setCursor(cursors.get(handle, Qt.CursorShape.SizeAllCursor if index >= 0 else Qt.CursorShape.ArrowCursor))
            return
        d = self._drag
        x, y = self._to_picture(event.position())
        dx, dy = x - d["start"][0], y - d["start"][1]
        tile: Tile = d["tile"]
        free = bool(event.modifiers() & Qt.KeyboardModifier.AltModifier)  # Alt: no snapping
        tiles = self._model.layout().tiles
        xs, ys = composite.snap_lines(tiles, self.selected)
        guides_x, guides_y = [], []
        if d["kind"] == "move":
            nx, ny = tile.x + dx, tile.y + dy
            if not free:
                sx, gx = composite.snap((nx, nx + tile.w / 2, nx + tile.w), xs)
                sy, gy = composite.snap((ny, ny + tile.h / 2, ny + tile.h), ys)
                nx, ny = nx + sx, ny + sy
                guides_x, guides_y = [g for g in (gx,) if g is not None], [g for g in (gy,) if g is not None]
            new = composite.moved(tile, nx, ny)
        elif d["kind"] == "resize":
            keep = bool(event.modifiers() & Qt.KeyboardModifier.ShiftModifier)
            box = (tile.x, tile.y, tile.w, tile.h)
            if not free and not keep:
                ex = (tile.x + dx) if "l" in d["handle"] else (tile.x + tile.w + dx) if "r" in d["handle"] else None
                ey = (tile.y + dy) if "t" in d["handle"] else (tile.y + tile.h + dy) if "b" in d["handle"] else None
                if ex is not None:
                    sx, gx = composite.snap((ex,), xs)
                    dx += sx
                    guides_x = [gx] if gx is not None else []
                if ey is not None:
                    sy, gy = composite.snap((ey,), ys)
                    dy += sy
                    guides_y = [gy] if gy is not None else []
            nx, ny, nw, nh = composite.resized(box, d["handle"], dx, dy, aspect=tile.w / tile.h if keep else None)
            new = replace(tile, x=nx, y=ny, w=nw, h=nh)
        else:
            new = self._panned(tile, event.position() - d["pos"])
        self._guides = (tuple(guides_x), tuple(guides_y))
        self._replace(self.selected, new)

    def _panned(self, tile: Tile, delta: QPointF) -> Tile:
        """tile with its crop moved along with a right-drag of delta (widget pixels), as if dragging the stream."""
        frame = self._model.frames().get(tile.source_id)
        if frame is None:
            return tile
        fh, fw = frame[0].shape[:2]
        rect = self._tile_rect(tile)
        x0, y0, x1, y1 = composite.crop_px(tile, fw, fh, max(1, round(rect.width())), max(1, round(rect.height())))
        cx = self._drag["tile"].cx - delta.x() / max(1.0, rect.width()) * (x1 - x0) / fw
        cy = self._drag["tile"].cy - delta.y() / max(1.0, rect.height()) * (y1 - y0) / fh
        return replace(tile, cx=min(1.0, max(0.0, cx)), cy=min(1.0, max(0.0, cy)))

    def mouseReleaseEvent(self, _event):
        self._drag = None
        self._guides = ((), ())
        self.update()

    def wheelEvent(self, event):
        index, _handle = self._hit(event.position())
        if index < 0:
            return
        self.select(index)
        tile = self._model.layout().tiles[index]
        steps = event.angleDelta().y() / 120
        zoom = min(composite.MAX_ZOOM, max(1.0, tile.zoom * _ZOOM_STEP ** steps))
        self._replace(index, replace(tile, zoom=zoom))
        event.accept()

    def keyPressEvent(self, event):
        tiles = self._model.layout().tiles
        moves = {Qt.Key.Key_Left: (-1, 0), Qt.Key.Key_Right: (1, 0), Qt.Key.Key_Up: (0, -1), Qt.Key.Key_Down: (0, 1)}
        if 0 <= self.selected < len(tiles) and event.key() in moves:
            w, h = self._model.size()
            step = _NUDGE_PX * (10 if event.modifiers() & Qt.KeyboardModifier.ShiftModifier else 1)
            mx, my = moves[event.key()]
            tile = tiles[self.selected]
            self._replace(self.selected, composite.moved(tile, tile.x + mx * step / w, tile.y + my * step / h))
            return
        super().keyPressEvent(event)

    def select(self, index: int):
        if index != self.selected:
            self.selected = index
            self.picked.emit(index)
            self.update()

    def _replace(self, index: int, tile: Tile):
        layout = self._model.layout()
        tiles = list(layout.tiles)
        if 0 <= index < len(tiles) and tiles[index] != tile:
            tiles[index] = tile
            self._model.set_layout(replace(layout, tiles=tuple(tiles)))
            self.edited.emit()
            self.refresh()


class _ColourButton(QPushButton):
    """A button showing a colour, which asks for another when clicked."""

    chosen = pyqtSignal(str)

    def __init__(self, title: str, parent=None):
        super().__init__(parent)
        self._title = title
        self._colour = "#000000"
        self.setFixedWidth(ui_px(64))
        self.setToolTip(title)
        self.clicked.connect(self._ask)

    def set_colour(self, colour: str):
        self._colour = colour
        self.setStyleSheet(f"QPushButton {{ background: {colour}; }}")
        self.setAccessibleName(f"{self._title}: {colour}")

    def _ask(self):
        picked = QColorDialog.getColor(QColor(self._colour), self.window(), self._title)
        if picked.isValid():
            self.set_colour(picked.name())
            self.chosen.emit(picked.name())


class CompositeEditor(PlacedDialog):
    """The editor window. model is the plugin: layout(), set_layout(), frames(), streams(), size() and the saved
    layouts (layouts(), names(), pick(), save_layout(), rename_layout(), delete_layout(), saved_match())."""

    def __init__(self, model, parent=None):
        super().__init__(parent)
        self._model = model
        self.setWindowTitle(tr("Edit picture"))
        self.setModal(False)
        lay = dialog_layout(self)
        dialog_header(lay, tr("Edit picture"), tr("Drag a stream to move it and its corners to resize it. Shift keeps "
                                                   "its shape and Alt stops it snapping."))
        body = QHBoxLayout()
        body.setSpacing(16)
        self.view = PictureView(model, self)
        self.view.picked.connect(lambda _i: self._show_tile())
        self.view.edited.connect(self._show_tile)
        body.addWidget(self.view, 1)
        side = QWidget()
        side_lay = QVBoxLayout(side)
        side_lay.setContentsMargins(0, 0, 8, 0)
        side_lay.setSpacing(10)
        self._build_layouts(side_lay)
        self._build_streams(side_lay)
        self._build_tile(side_lay)
        self._build_background(side_lay)
        side_lay.addStretch(1)
        scroll = QScrollArea()
        scroll.setWidgetResizable(True)
        scroll.setFrameShape(QScrollArea.Shape.NoFrame)
        scroll.setHorizontalScrollBarPolicy(Qt.ScrollBarPolicy.ScrollBarAlwaysOff)
        scroll.setWidget(side)
        scroll.setFixedWidth(ui_px(372))
        body.addWidget(scroll)
        lay.addLayout(body, 1)
        close = QPushButton(tr("Close"))
        close.clicked.connect(self.close)
        dialog_buttons(lay, close)
        self.reload()
        self._side = side

    def fit_before_show(self):
        # As tall as the side column needs, so nothing is scrolled away unless the screen is too small
        screen = self.screen().availableGeometry() if self.screen() is not None else None
        height = max(ui_px(680), self._side.sizeHint().height() + ui_px(170))
        if screen is not None:
            height = min(height, screen.height() - ui_px(40))
        self.resize(ui_px(1180), height)

    # ── Saved layouts ─────────────────────────────────────────────────────

    def _build_layouts(self, lay: QVBoxLayout):
        add_section_heading(lay, tr("Layout"), "bookmark")
        self._layout_combo = NoScrollComboBox()
        self._layout_combo.setSizeAdjustPolicy(QComboBox.SizeAdjustPolicy.AdjustToMinimumContentsLengthWithIcon)
        self._layout_combo.setMinimumContentsLength(12)
        self._layout_combo.setPlaceholderText(tr("Not saved"))
        self._layout_combo.activated.connect(lambda i: self._pick(self._layout_combo.itemText(i)))
        lay.addWidget(self._layout_combo)
        self._save_btn = QPushButton(tr("Save as…"))
        self._save_btn.clicked.connect(self._ask_save)
        self._rename_btn = QPushButton(tr("Rename…"))
        self._rename_btn.clicked.connect(self._ask_rename)
        self._delete_btn = QPushButton(tr("Delete"))
        self._delete_btn.clicked.connect(self._delete)
        lay.addLayout(button_row(self._save_btn, self._rename_btn, self._delete_btn))
        starts = []
        for text, make in ((tr("Side by side"), composite.side_by_side), (tr("Big and small"), composite.big_and_small),
                           (tr("Grid"), composite.grid)):
            btn = QPushButton(text)
            btn.setToolTip(tr("Start again with the running streams laid out like this"))
            btn.clicked.connect(lambda _=False, m=make: self._start_from(m))
            starts.append(btn)
        self._start_btns = starts
        lay.addWidget(self._flow(*starts))

    def _pick(self, name: str):
        self._model.pick(name)
        self.view.select(-1)
        self.reload()

    def _unused_name(self) -> str:
        n = len(self._model.names()) + 1
        while tr("Layout {n}", n=n) in self._model.names():
            n += 1
        return tr("Layout {n}", n=n)

    def _ask_save(self):
        current = self._model.layout().name
        name, ok = QInputDialog.getText(self, tr("Save layout"), tr("Name:"), QLineEdit.EchoMode.Normal,
                                        current or self._unused_name())
        name = name.strip()
        if not ok or not name:
            return
        if name in self._model.names() and name != current:
            answer = QMessageBox.question(self, tr("Replace layout"),
                                          tr('There\'s already a layout called "{name}". Replace it?', name=name))
            if answer != QMessageBox.StandardButton.Yes:
                return
        self._model.save_layout(name)
        self.reload()

    def _ask_rename(self):
        old = self._model.saved_match() or self._model.layout().name
        if old not in self._model.names():
            return
        name, ok = QInputDialog.getText(self, tr("Rename layout"), tr("Name:"), QLineEdit.EchoMode.Normal, old)
        if ok and name.strip():
            self._model.rename_layout(old, name)
            self.reload()

    def _delete(self):
        name = self._model.saved_match() or self._model.layout().name
        if name in self._model.names():
            self._model.delete_layout(name)
            self.reload()

    def _start_from(self, make):
        w, h = self._model.size()
        tiles = make(self._model.streams(), w / h)
        layout = self._model.layout()
        self._model.set_layout(replace(layout, tiles=tiles))
        self.view.select(-1)
        self.reload()

    # ── Which streams are in it ───────────────────────────────────────────

    def _build_streams(self, lay: QVBoxLayout):
        add_section_heading(lay, tr("Streams"), "video")
        self._streams_box = QVBoxLayout()
        self._streams_box.setSpacing(4)
        lay.addLayout(self._streams_box)
        self._no_streams = wrapped_note(tr("Start two or more streams to put them together. The + next to Stop adds "
                                           "one."))
        lay.addWidget(self._no_streams)
        self._stream_checks: list = []

    def _show_streams(self):
        for check in self._stream_checks:
            self._streams_box.removeWidget(check)
            check.hide()
            check.deleteLater()
        self._stream_checks = []
        layout = self._model.layout()
        entries = [(sid, name) for sid, name, _aspect in self._model.streams()]
        known = {sid for sid, _name in entries}
        entries += [(t.source_id, t.name or t.source_id) for t in layout.tiles if t.source_id not in known]
        inside = {t.source_id for t in layout.tiles}
        for sid, name in entries:
            check = QCheckBox(name if sid in known else tr("{name} (not streaming)", name=name))
            check.setChecked(sid in inside)
            check.toggled.connect(lambda on, s=sid, n=name: self._put(s, n, on))
            self._streams_box.addWidget(check)
            self._stream_checks.append(check)
        self._no_streams.setVisible(len(known) < 2)

    def _put(self, source_id: str, name: str, on: bool):
        layout = self._model.layout()
        tiles = [t for t in layout.tiles if t.source_id != source_id]
        if on and len(tiles) < composite.MAX_TILES:
            w, h = self._model.size()
            aspect = next((a for sid, _n, a in self._model.streams() if sid == source_id), w / h)
            box = composite.fit_box(aspect, (0.3, 0.3, 0.4, 0.4), w / h)
            tiles.append(Tile(source_id, name, *box, radius=16))
        self._model.set_layout(replace(layout, tiles=tuple(tiles)))
        self.view.select(len(tiles) - 1 if on else -1)
        self.reload()

    # ── The picked stream ─────────────────────────────────────────────────

    def _build_tile(self, lay: QVBoxLayout):
        add_section_heading(lay, tr("Picked stream"), "crop")
        self._tile_box = QWidget()
        box = QVBoxLayout(self._tile_box)
        box.setContentsMargins(0, 0, 0, 0)
        box.setSpacing(6)
        self._tile_name = QLabel()
        box.addWidget(self._tile_name)
        self._zoom = NoScrollSlider(Qt.Orientation.Horizontal)
        self._zoom.setRange(100, round(composite.MAX_ZOOM * 100))
        self._zoom.set_default(100)
        self._zoom.setToolTip(tr("Scroll over a stream to zoom in and drag it with the right mouse button to move "
                                 "what's shown"))
        self._zoom_lbl = value_label()
        self._zoom.valueChanged.connect(lambda v: self._edit_tile(zoom=v / 100))
        box.addLayout(self._labelled(tr("Zoom"), slider_row(self._zoom, self._zoom_lbl)))
        self._border = NoScrollSlider(Qt.Orientation.Horizontal)
        self._border.setRange(0, composite.MAX_BORDER)
        self._border.set_default(0)
        self._border_lbl = value_label()
        self._border.valueChanged.connect(lambda v: self._edit_tile(border=v))
        box.addLayout(self._labelled(tr("Border"), slider_row(self._border, self._border_lbl)))
        self._border_colour = _ColourButton(tr("Border colour"))
        self._border_colour.chosen.connect(lambda c: self._edit_tile(border_colour=c))
        box.addLayout(self._labelled(tr("Colour"), self._border_colour))
        self._radius = NoScrollSlider(Qt.Orientation.Horizontal)
        self._radius.setRange(0, composite.MAX_RADIUS)
        self._radius.set_default(0)
        self._radius_lbl = value_label()
        self._radius.valueChanged.connect(lambda v: self._edit_tile(radius=v))
        box.addLayout(self._labelled(tr("Corners"), slider_row(self._radius, self._radius_lbl)))
        self._fit_btn = QPushButton(tr("Fit its shape"))
        self._fit_btn.setToolTip(tr("Make the box the stream's own shape, so none of it is cut off"))
        self._fit_btn.clicked.connect(self._fit_shape)
        self._back_btn = QPushButton(tr("Send back"))
        self._back_btn.clicked.connect(lambda: self._restack(-1))
        self._front_btn = QPushButton(tr("Bring forward"))
        self._front_btn.clicked.connect(lambda: self._restack(1))
        box.addWidget(self._flow(self._fit_btn, self._back_btn, self._front_btn))
        lay.addWidget(self._tile_box)
        self._pick_hint = wrapped_note(tr("Click a stream in the picture to change it."))
        lay.addWidget(self._pick_hint)

    @staticmethod
    def _flow(*buttons) -> QWidget:
        """Buttons of one width side by side, wrapping onto another row when a translation needs the room."""
        holder = QWidget()
        flow = FlowLayout(holder, spacing=8, uniform=True)
        for btn in buttons:
            flow.addWidget(btn)
        return holder

    @staticmethod
    def _labelled(text: str, control) -> QHBoxLayout:
        row = QHBoxLayout()
        row.setSpacing(8)
        label = QLabel(text)
        label.setObjectName("dim")
        label.setFixedWidth(ui_px(64))
        row.addWidget(label)
        if isinstance(control, QHBoxLayout):
            row.addLayout(control, 1)
        else:
            row.addWidget(control)
            row.addStretch(1)
        return row

    def _picked(self) -> Optional[Tile]:
        tiles = self._model.layout().tiles
        return tiles[self.view.selected] if 0 <= self.view.selected < len(tiles) else None

    def _show_tile(self):
        tile = self._picked()
        self._tile_box.setVisible(tile is not None)
        self._pick_hint.setVisible(tile is None)
        if tile is None:
            return
        self._tile_name.setText(tile.name or tile.source_id)
        for slider, value in ((self._zoom, round(tile.zoom * 100)), (self._border, tile.border),
                              (self._radius, tile.radius)):
            slider.blockSignals(True)
            slider.setValue(value)
            slider.blockSignals(False)
        self._zoom_lbl.setText(f"{round(tile.zoom * 100)} %")
        self._border_lbl.setText(f"{tile.border} px")
        self._radius_lbl.setText(f"{tile.radius} px")
        self._border_colour.set_colour(tile.border_colour)
        tiles = self._model.layout().tiles
        self._fit_btn.setEnabled(any(sid == tile.source_id for sid, _n, _a in self._model.streams()))
        self._back_btn.setEnabled(self.view.selected > 0)
        self._front_btn.setEnabled(self.view.selected < len(tiles) - 1)

    def _edit_tile(self, **changes):
        tile = self._picked()
        if tile is None:
            return
        layout = self._model.layout()
        tiles = list(layout.tiles)
        tiles[self.view.selected] = replace(tile, **changes)
        self._model.set_layout(replace(layout, tiles=tuple(tiles)))
        self._show_tile()
        self.view.refresh()

    def _fit_shape(self):
        tile = self._picked()
        aspect = next((a for sid, _n, a in self._model.streams() if tile is not None and sid == tile.source_id), None)
        if aspect is None:
            return
        w, h = self._model.size()
        x, y, bw, bh = composite.fit_box(aspect, (tile.x, tile.y, tile.w, tile.h), w / h)
        self._edit_tile(x=x, y=y, w=bw, h=bh, zoom=1.0, cx=0.5, cy=0.5)

    def _restack(self, step: int):
        layout = self._model.layout()
        index = self.view.selected
        tiles = composite.raised(layout.tiles, index, step)
        self._model.set_layout(replace(layout, tiles=tiles))
        self.view.selected = min(max(0, index + step), len(tiles) - 1)
        self._show_tile()
        self.view.refresh()

    # ── Background ────────────────────────────────────────────────────────

    def _build_background(self, lay: QVBoxLayout):
        add_section_heading(lay, tr("Background"), "image")
        self._bg_colour = _ColourButton(tr("Background colour"))
        self._bg_colour.chosen.connect(lambda c: self._edit_layout(background=c))
        self._picture_btn = QPushButton(tr("Picture…"))
        self._picture_btn.clicked.connect(self._ask_picture)
        self._no_picture_btn = QPushButton(tr("No picture"))
        self._no_picture_btn.clicked.connect(lambda: self._edit_layout(picture=""))
        row = QHBoxLayout()
        row.setSpacing(8)
        row.addWidget(self._bg_colour)
        row.addLayout(button_row(self._picture_btn, self._no_picture_btn), 1)
        lay.addLayout(row)

    def _ask_picture(self):
        path, _filter = QFileDialog.getOpenFileName(self, tr("Background picture"), "", tr(_PICTURES))
        if path:
            self._edit_layout(picture=path)

    def _edit_layout(self, **changes):
        self._model.set_layout(replace(self._model.layout(), **changes))
        self.reload()

    # ── Everything ────────────────────────────────────────────────────────

    def reload(self):
        """Bring every control up to date with the picture (a preset or a shortcut may have changed it)."""
        names = self._model.names()
        self._layout_combo.blockSignals(True)
        self._layout_combo.clear()
        self._layout_combo.addItems(names)
        match = self._model.saved_match()
        self._layout_combo.setCurrentIndex(names.index(match) if match else -1)
        self._layout_combo.blockSignals(False)
        self._layout_combo.setEnabled(bool(names))
        self._rename_btn.setEnabled(bool(match))
        self._delete_btn.setEnabled(bool(match))
        has_streams = bool(self._model.streams())
        for btn in self._start_btns:
            btn.setEnabled(has_streams)
        layout: Layout = self._model.layout()
        if self.view.selected >= len(layout.tiles):
            self.view.select(-1)
        self._show_streams()
        self._show_tile()
        self._bg_colour.set_colour(layout.background)
        self._no_picture_btn.setEnabled(bool(layout.picture))
        self.view.refresh()
