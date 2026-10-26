"""The editor for Several cameras in one picture: a live preview to drag, resize and crop the streams on, and the
controls for borders, corners, the background and saved layouts. Every change goes out at once."""

import math
import os
from dataclasses import replace
from typing import Optional

from PyQt6.QtCore import QPointF, QRectF, Qt, QTimer, pyqtSignal
from PyQt6.QtGui import QColor, QFontMetricsF, QImage, QPainter, QPainterPath, QPen
from PyQt6.QtWidgets import (
    QColorDialog, QComboBox, QFileDialog, QHBoxLayout, QInputDialog, QLabel, QLineEdit, QMessageBox, QPushButton,
    QScrollArea, QSizePolicy, QVBoxLayout, QWidget,
)

from telescope import composite, theme
from telescope.composite import Compositor, Layout, Tile
from telescope.widgets.common import (
    ElidingLabel, NoScrollComboBox, NoScrollSlider, PlacedDialog, Switch, add_card_header, button_row, card_action,
    card_layout, control_row, create_card, dialog_buttons, dialog_header, dialog_layout, icon_button, set_ui_role,
    slider_row, ui_px, value_label, wrapped_note,
)
from telescope.i18n import N_, tr

_PICTURES = N_("Pictures (*.png *.jpg *.jpeg *.webp *.bmp)")
_HANDLES = ("tl", "t", "tr", "r", "br", "b", "bl", "l")
_HANDLE_PX = 9  # how far from a handle a click still grabs it
_CORNER_PX = 10  # a corner handle's size
_STAGE_PAD = 14  # room around the picture inside the dark stage, so handles on its edge aren't cut off
_PREVIEW_MS = 66  # about 15 pictures a second is plenty to line things up by
_PREVIEW_MAX_W = 1280
_NUDGE_PX = 1  # arrow keys move the picked stream this many pixels of the picture (Shift: ten times as far)
_ZOOM_STEP = 1.1
_SIDE_W = 384
_LABEL_W = 72
_HUE = "stream"
_MODES = {"move": N_("Move"), "scale": N_("Scale"), "stretch": N_("Stretch"), "crop": N_("Crop"), "pan": N_("Pan")}


class PictureView(QWidget):
    """The live preview on a dark stage, with the tiles' outlines on top to grab."""

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
        self._hover = (-1, "")  # tile and handle under the pointer
        self._drag: Optional[dict] = None
        self._mode = ""  # what the drag going on does, shown in the corner
        self._guides: tuple = ((), ())  # snap lines to draw while dragging, in picture fractions
        self.setMinimumSize(ui_px(520), ui_px(320))
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
        """Where the picture sits in the widget, as big as its shape allows inside the stage."""
        w, h = self._model.size()
        pad = ui_px(_STAGE_PAD)
        r = QRectF(self.rect()).adjusted(pad, pad, -pad, -pad)
        scale = min(r.width() / w, r.height() / h)
        aw, ah = round(w * scale), round(h * scale)
        return QRectF(round(r.x() + (r.width() - aw) / 2), round(r.y() + (r.height() - ah) / 2), aw, ah)

    def _to_picture(self, pos: QPointF) -> tuple:
        a = self.area()
        return (pos.x() - a.x()) / a.width(), (pos.y() - a.y()) / a.height()

    def _tile_rect(self, tile: Tile) -> QRectF:
        a = self.area()
        return QRectF(a.x() + tile.x * a.width(), a.y() + tile.y * a.height(), tile.w * a.width(), tile.h * a.height())

    @staticmethod
    def _handle_points(rect: QRectF) -> dict:
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

    def _source_aspect(self, source_id: str) -> Optional[float]:
        """The stream's width over height, from its newest frame or what the model says; None while it's off."""
        frame = self._model.frames().get(source_id)
        if frame is not None:
            fh, fw = frame[0].shape[:2]
            return fw / fh
        return next((a for sid, _n, a in self._model.streams() if sid == source_id), None)

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
        stage = QRectF(self.rect()).adjusted(0.5, 0.5, -0.5, -0.5)
        p.setPen(QPen(QColor(theme.BORDER), 1))
        p.setBrush(QColor(theme.SURFACE_SUNK))
        p.drawRoundedRect(stage, 10, 10)
        a = self.area()
        if self._image is not None:
            p.drawImage(a, self._image)
        p.setBrush(Qt.BrushStyle.NoBrush)
        p.setPen(QPen(QColor(theme.BORDER_STRONG), 1))
        p.drawRect(a.adjusted(-0.5, -0.5, 0.5, 0.5))
        accent = QColor(theme.hue(_HUE))
        live = set(self._model.frames())
        tiles = self._model.layout().tiles
        for i, tile in enumerate(tiles):
            rect = self._tile_rect(tile)
            if tile.source_id not in live:
                self._paint_missing(p, rect, tile)
            if i == self._hover[0] and i != self.selected and self._drag is None:
                faint = QColor(accent)
                faint.setAlphaF(0.55)
                p.setPen(QPen(faint, 1.5))
                p.setBrush(Qt.BrushStyle.NoBrush)
                p.drawRect(rect)
        guide = QColor(theme.ACCENT_SOFT)
        p.setPen(QPen(guide, 1, Qt.PenStyle.DashLine))
        for x in self._guides[0]:
            p.drawLine(QPointF(a.x() + x * a.width(), a.top()), QPointF(a.x() + x * a.width(), a.bottom()))
        for y in self._guides[1]:
            p.drawLine(QPointF(a.left(), a.y() + y * a.height()), QPointF(a.right(), a.y() + y * a.height()))
        if 0 <= self.selected < len(tiles):
            self._paint_selected(p, self._tile_rect(tiles[self.selected]), tiles[self.selected], accent)
        if self._mode:
            self._paint_chip(p, QPointF(a.x() + ui_px(8), a.y() + ui_px(8)), self._mode, accent)
        p.end()

    def _paint_missing(self, p: QPainter, rect: QRectF, tile: Tile):
        """A stream that isn't running: a dashed, faintly filled box with its name."""
        fill = QColor(theme.SURFACE_RAISE)
        fill.setAlphaF(0.75)
        p.setBrush(fill)
        p.setPen(QPen(QColor(theme.TEXT_FAINT), 1, Qt.PenStyle.DashLine))
        p.drawRect(rect)
        p.setPen(QColor(theme.TEXT_DIM))
        p.drawText(rect.adjusted(6, 6, -6, -6), Qt.AlignmentFlag.AlignCenter | Qt.TextFlag.TextWordWrap,
                   tr("{name} isn't streaming", name=tile.name or tile.source_id))

    def _paint_selected(self, p: QPainter, rect: QRectF, tile: Tile, accent: QColor):
        """The picked tile's outline and handles: a dark line under the accent so they show on any picture."""
        p.setBrush(Qt.BrushStyle.NoBrush)
        shade = QColor(theme.SURFACE_SUNK)
        shade.setAlphaF(0.7)
        p.setPen(QPen(shade, 3.5))
        p.drawRect(rect)
        p.setPen(QPen(accent, 1.5))
        p.drawRect(rect)
        corner = ui_px(_CORNER_PX)
        for name, point in self._handle_points(rect).items():
            hot = self._hover == (self.selected, name) or (self._drag is not None and self._drag["handle"] == name)
            grow = 2 if hot else 0
            if len(name) == 2:
                box = QRectF(point.x() - corner / 2 - grow / 2, point.y() - corner / 2 - grow / 2, corner + grow,
                             corner + grow)
                radius = box.width() / 2
            else:
                long, short = corner * 1.6 + grow, corner * 0.6 + grow / 2
                bw, bh = (long, short) if name in ("t", "b") else (short, long)
                box = QRectF(point.x() - bw / 2, point.y() - bh / 2, bw, bh)
                radius = short / 2
            p.setPen(QPen(QColor(theme.SURFACE_SUNK), 1.5))
            p.setBrush(QColor("#ffffff") if hot else accent)
            p.drawRoundedRect(box, radius, radius)
        label = tile.name or tile.source_id
        if label and rect.height() > ui_px(40):
            self._paint_chip(p, QPointF(rect.left() + ui_px(6), rect.top() + ui_px(6)), label, None, rect.width() - 12)

    def _paint_chip(self, p: QPainter, at: QPointF, text: str, accent: Optional[QColor], max_w: Optional[float] = None):
        """A small dark pill with text, for the drag's mode and the picked stream's name."""
        fm = QFontMetricsF(p.font())
        pad_x, h = ui_px(8), fm.height() + ui_px(6)
        if max_w is not None:
            text = fm.elidedText(text, Qt.TextElideMode.ElideRight, max(0.0, max_w - 2 * pad_x))
        if not text:
            return
        box = QRectF(at.x(), at.y(), math.ceil(fm.horizontalAdvance(text)) + 2 * pad_x + 2, h)
        back = QColor(theme.SURFACE_SUNK)
        back.setAlphaF(0.82)
        path = QPainterPath()
        path.addRoundedRect(box, h / 2, h / 2)
        p.fillPath(path, back)
        if accent is not None:
            p.setPen(QPen(accent, 1))
            p.setBrush(Qt.BrushStyle.NoBrush)
            p.drawPath(path)
        p.setPen(QColor(theme.ACCENT_SOFT if accent is not None else theme.TEXT))
        p.drawText(box, Qt.AlignmentFlag.AlignCenter, text)

    # ── Mouse and keys ────────────────────────────────────────────────────

    def mousePressEvent(self, event):
        index, handle = self._hit(event.position())
        self.select(index)
        if index < 0:
            return
        tile = self._model.layout().tiles[index]
        kind = "pan" if event.button() == Qt.MouseButton.RightButton else ("resize" if handle else "move")
        self._drag = {"kind": kind, "handle": handle if kind == "resize" else "", "start": self._to_picture(event.position()),
                      "tile": tile, "pos": event.position()}

    def mouseMoveEvent(self, event):
        if self._drag is None:
            hover = self._hit(event.position())
            if hover != self._hover:
                self._hover = hover
                self.update()
            index, handle = hover
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
        mods = event.modifiers()
        snapping = not mods & Qt.KeyboardModifier.ControlModifier  # Ctrl: no snapping, as in OBS
        xs, ys = composite.snap_lines(self._model.layout().tiles, self.selected)
        guides_x, guides_y = [], []
        if d["kind"] == "move":
            mode = "move"
            nx, ny = tile.x + dx, tile.y + dy
            if snapping:
                sx, gx = composite.snap((nx, nx + tile.w / 2, nx + tile.w), xs)
                sy, gy = composite.snap((ny, ny + tile.h / 2, ny + tile.h), ys)
                nx, ny = nx + sx, ny + sy
                guides_x, guides_y = [g for g in (gx,) if g is not None], [g for g in (gy,) if g is not None]
            new = composite.moved(tile, nx, ny)
        elif d["kind"] == "resize":
            handle = d["handle"]
            # OBS's keys: Alt crops the side being dragged, Shift lets the shape go, a plain drag keeps it
            mode = ("crop" if mods & Qt.KeyboardModifier.AltModifier
                    else "stretch" if mods & Qt.KeyboardModifier.ShiftModifier else "scale")
            if snapping:
                ex = (tile.x + dx) if "l" in handle else (tile.x + tile.w + dx) if "r" in handle else None
                ey = (tile.y + dy) if "t" in handle else (tile.y + tile.h + dy) if "b" in handle else None
                if ex is not None:
                    dx += composite.snap((ex,), xs)[0]
                if ey is not None:
                    dy += composite.snap((ey,), ys)[0]
            w, h = self._model.size()
            new = composite.dragged(tile, handle, dx, dy, mode, w / h, self._source_aspect(tile.source_id))
            if snapping:
                guides_x = [g for g in xs if any(abs(g - e) < 1e-6 for e in (new.x, new.x + new.w))]
                guides_y = [g for g in ys if any(abs(g - e) < 1e-6 for e in (new.y, new.y + new.h))]
        else:
            mode = "pan"
            new = self._panned(tile, event.position() - d["pos"])
        text = tr(_MODES[mode])
        self._mode = text if snapping or mode == "pan" else tr("{mode}, no snapping", mode=text)
        self._guides = (tuple(guides_x), tuple(guides_y))
        self._replace(self.selected, new)
        self.update()

    def _panned(self, tile: Tile, delta: QPointF) -> Tile:
        """tile with what it shows moved along with a right-drag of delta (widget pixels), as if dragging the stream."""
        frame = self._model.frames().get(tile.source_id)
        if frame is None:
            return tile
        fh, fw = frame[0].shape[:2]
        rect = self._tile_rect(tile)
        x0, y0, x1, y1 = composite.crop_px(tile, fw, fh, max(1, round(rect.width())), max(1, round(rect.height())))
        rw = max(1.0, (1.0 - tile.crop_l - tile.crop_r) * fw)
        rh = max(1.0, (1.0 - tile.crop_t - tile.crop_b) * fh)
        cx = self._drag["tile"].cx - delta.x() / max(1.0, rect.width()) * (x1 - x0) / rw
        cy = self._drag["tile"].cy - delta.y() / max(1.0, rect.height()) * (y1 - y0) / rh
        return replace(tile, cx=min(1.0, max(0.0, cx)), cy=min(1.0, max(0.0, cy)))

    def mouseReleaseEvent(self, _event):
        self._drag = None
        self._mode = ""
        self._guides = ((), ())
        self.update()

    def leaveEvent(self, event):
        self._hover = (-1, "")
        self.update()
        super().leaveEvent(event)

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
    """A swatch showing a colour, which asks for another when clicked."""

    chosen = pyqtSignal(str)

    def __init__(self, title: str, parent=None):
        super().__init__(parent)
        self._title = title
        self._colour = "#000000"
        self.setFixedSize(ui_px(52), ui_px(30))
        self.setCursor(Qt.CursorShape.PointingHandCursor)
        self.setToolTip(title)
        self.clicked.connect(self._ask)

    def set_colour(self, colour: str):
        self._colour = colour
        self.setStyleSheet(f"QPushButton {{ background: {colour}; border: 1px solid {theme.BORDER_STRONG}; }}"
                           f"QPushButton:hover {{ background: {colour}; border-color: {theme.BORDER_HOVER}; }}")
        self.setAccessibleName(f"{self._title}: {colour}")

    def _ask(self):
        picked = QColorDialog.getColor(QColor(self._colour), self.window(), self._title)
        if picked.isValid():
            self.set_colour(picked.name())
            self.chosen.emit(picked.name())


def _swatch_row(button: _ColourButton) -> tuple:
    """[swatch][its hex code], and the label holding the code."""
    row = QHBoxLayout()
    row.setContentsMargins(0, 0, 0, 0)
    row.setSpacing(10)
    row.addWidget(button)
    code = QLabel()
    code.setObjectName("dim")
    row.addWidget(code)
    row.addStretch(1)
    return row, code


class CompositeEditor(PlacedDialog):
    """The editor window. model is the plugin: layout(), set_layout(), frames(), streams(), size() and the saved
    layouts (layouts(), names(), pick(), save_layout(), rename_layout(), delete_layout(), saved_match())."""

    def __init__(self, model, parent=None):
        super().__init__(parent)
        self._model = model
        self.setWindowTitle(tr("Edit picture"))
        self.setModal(False)
        self.setMinimumSize(ui_px(960), ui_px(600))
        lay = dialog_layout(self)
        dialog_header(lay, tr("Edit picture"), tr("Drag a stream to move it and a handle to resize it. Hold Shift to "
                                                   "stretch it, Alt to crop that side and Ctrl to stop it snapping."))
        body = QHBoxLayout()
        body.setSpacing(12)
        body.addWidget(self._build_canvas(), 1)
        side = QWidget()
        side_lay = QVBoxLayout(side)
        side_lay.setContentsMargins(0, 0, ui_px(10), 0)  # room for the scroll bar on a short screen
        side_lay.setSpacing(12)
        side_lay.addWidget(self._build_layouts())
        side_lay.addWidget(self._build_streams())
        side_lay.addWidget(self._build_tile())
        side_lay.addWidget(self._build_background())
        side_lay.addStretch(1)
        scroll = QScrollArea()
        scroll.setWidgetResizable(True)
        scroll.setFrameShape(QScrollArea.Shape.NoFrame)
        scroll.setHorizontalScrollBarPolicy(Qt.ScrollBarPolicy.ScrollBarAlwaysOff)
        scroll.setWidget(side)
        scroll.setFixedWidth(ui_px(_SIDE_W))
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
        height = max(ui_px(700), self._side.sizeHint().height() + ui_px(150))
        width = ui_px(1240)
        if screen is not None:
            height = min(height, screen.height() - ui_px(40))
            width = min(width, screen.width() - ui_px(40))
        self.resize(width, height)

    @staticmethod
    def _card(title: str, icon: str, action: QWidget = None) -> tuple:
        card = create_card()
        lay = card_layout(card)
        header = add_card_header(lay, title, icon, action=action, hue=_HUE)
        return card, lay, header

    # ── The picture ───────────────────────────────────────────────────────

    def _build_canvas(self) -> QWidget:
        starts = QWidget()
        row = QHBoxLayout(starts)
        row.setContentsMargins(0, 0, 0, 0)
        row.setSpacing(6)
        lead = QLabel(tr("Start from"))
        lead.setObjectName("dim")
        row.addWidget(lead)
        self._start_btns = []
        for text, make in ((tr("Side by side"), composite.side_by_side), (tr("Big and small"), composite.big_and_small),
                           (tr("Grid"), composite.grid)):
            btn = QPushButton(text)
            set_ui_role(btn, "quiet")
            btn.setToolTip(tr("Start again with the running streams laid out like this"))
            btn.clicked.connect(lambda _=False, m=make: self._start_from(m))
            row.addWidget(btn)
            self._start_btns.append(btn)
        card, lay, _header = self._card(tr("Picture"), "layout", starts)
        self.view = PictureView(self._model, card)
        self.view.picked.connect(lambda _i: self._show_tile())
        self.view.edited.connect(self._show_tile)
        lay.addWidget(self.view, 1)
        card.setSizePolicy(QSizePolicy.Policy.Expanding, QSizePolicy.Policy.Expanding)
        return card

    def _start_from(self, make):
        w, h = self._model.size()
        tiles = make(self._model.streams(), w / h)
        layout = self._model.layout()
        self._model.set_layout(replace(layout, tiles=tiles))
        self.view.select(-1)
        self.reload()

    # ── Saved layouts ─────────────────────────────────────────────────────

    def _build_layouts(self) -> QWidget:
        self._save_btn = card_action(tr("Save as…"), "bookmark", tr("Keep this picture under a name"))
        self._save_btn.clicked.connect(self._ask_save)
        card, lay, _header = self._card(tr("Layout"), "bookmark", self._save_btn)
        self._layout_combo = NoScrollComboBox()
        self._layout_combo.setSizeAdjustPolicy(QComboBox.SizeAdjustPolicy.AdjustToMinimumContentsLengthWithIcon)
        self._layout_combo.setMinimumContentsLength(8)
        self._layout_combo.setPlaceholderText(tr("Not saved"))
        self._layout_combo.setAccessibleName(tr("Layout"))
        self._layout_combo.activated.connect(lambda i: self._pick(self._layout_combo.itemText(i)))
        self._rename_btn = icon_button("pencil", tr("Rename…"), tr("Rename this layout"))
        self._rename_btn.clicked.connect(self._ask_rename)
        self._delete_btn = icon_button("trash", tr("Delete"), tr("Delete this layout"))
        self._delete_btn.clicked.connect(self._delete)
        row = QHBoxLayout()
        row.setContentsMargins(0, 0, 0, 0)
        row.setSpacing(6)
        row.addWidget(self._layout_combo, 1)
        row.addWidget(self._rename_btn)
        row.addWidget(self._delete_btn)
        lay.addLayout(row)
        return card

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
        if name != current and not self._may_replace(name):
            return
        self._model.save_layout(name)
        self.reload()

    def _may_replace(self, name: str) -> bool:
        """Whether name is free, or there's a layout of that name and the user said to replace it."""
        if name not in self._model.names():
            return True
        answer = QMessageBox.question(self, tr("Replace layout"),
                                      tr('There\'s already a layout called "{name}". Replace it?', name=name))
        return answer == QMessageBox.StandardButton.Yes

    def _ask_rename(self):
        old = self._model.saved_match() or self._model.layout().name
        if old not in self._model.names():
            return
        name, ok = QInputDialog.getText(self, tr("Rename layout"), tr("Name:"), QLineEdit.EchoMode.Normal, old)
        name = name.strip()
        if not ok or not name or name == old or not self._may_replace(name):
            return
        self._model.rename_layout(old, name)
        self.reload()

    def _delete(self):
        name = self._model.saved_match() or self._model.layout().name
        if name in self._model.names():
            self._model.delete_layout(name)
            self.reload()

    # ── Which streams are in it ───────────────────────────────────────────

    def _build_streams(self) -> QWidget:
        card, lay, _header = self._card(tr("Streams"), "video")
        self._streams_box = QVBoxLayout()
        self._streams_box.setContentsMargins(0, 0, 0, 0)
        self._streams_box.setSpacing(0)
        lay.addLayout(self._streams_box)
        self._no_streams = wrapped_note(tr("Start two or more streams to put them together. The + next to Stop adds "
                                           "one."))
        lay.addWidget(self._no_streams)
        self._stream_checks: list = []
        return card

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
            check = Switch(name if sid in known else tr("{name} (not streaming)", name=name))
            check.setProperty("dim_text", sid not in known)
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

    def _build_tile(self) -> QWidget:
        self._fit_btn = card_action(tr("Fit its shape"), "reset",
                                    tr("Make the box the stream's own shape again, with nothing cropped or stretched"))
        self._fit_btn.clicked.connect(self._fit_shape)
        card, lay, _header = self._card(tr("Picked stream"), "crop", self._fit_btn)
        self._tile_name = ElidingLabel()
        self._tile_box = QWidget()
        self._tile_box.setObjectName("card_body")
        box = QVBoxLayout(self._tile_box)
        box.setContentsMargins(0, 0, 0, 0)
        box.setSpacing(2)
        box.addLayout(control_row(tr("Stream"), self._tile_name, _LABEL_W, stretch=True))
        self._zoom = NoScrollSlider(Qt.Orientation.Horizontal)
        self._zoom.setRange(100, round(composite.MAX_ZOOM * 100))
        self._zoom.set_default(100)
        self._zoom.setToolTip(tr("Scroll over a stream to zoom in and drag it with the right mouse button to move "
                                 "what's shown"))
        self._zoom_lbl = value_label()
        self._zoom.valueChanged.connect(lambda v: self._edit_tile(zoom=v / 100))
        box.addLayout(control_row(tr("Zoom"), slider_row(self._zoom, self._zoom_lbl), _LABEL_W, stretch=True))
        self._border = NoScrollSlider(Qt.Orientation.Horizontal)
        self._border.setRange(0, composite.MAX_BORDER)
        self._border.set_default(0)
        self._border_lbl = value_label()
        self._border.valueChanged.connect(lambda v: self._edit_tile(border=v))
        box.addLayout(control_row(tr("Border"), slider_row(self._border, self._border_lbl), _LABEL_W, stretch=True))
        self._border_colour = _ColourButton(tr("Border colour"))
        self._border_colour.chosen.connect(lambda c: self._edit_tile(border_colour=c))
        swatch, self._border_code = _swatch_row(self._border_colour)
        box.addLayout(control_row(tr("Colour"), swatch, _LABEL_W, stretch=True))
        self._radius = NoScrollSlider(Qt.Orientation.Horizontal)
        self._radius.setRange(0, composite.MAX_RADIUS)
        self._radius.set_default(0)
        self._radius_lbl = value_label()
        self._radius.valueChanged.connect(lambda v: self._edit_tile(radius=v))
        box.addLayout(control_row(tr("Corners"), slider_row(self._radius, self._radius_lbl), _LABEL_W, stretch=True))
        self._back_btn = QPushButton(tr("Send back"))
        self._back_btn.clicked.connect(lambda: self._restack(-1))
        self._front_btn = QPushButton(tr("Bring forward"))
        self._front_btn.clicked.connect(lambda: self._restack(1))
        for btn in (self._back_btn, self._front_btn):
            set_ui_role(btn, "quiet")
        box.addLayout(control_row(tr("Order"), button_row(self._back_btn, self._front_btn), _LABEL_W, stretch=True))
        lay.addWidget(self._tile_box)
        self._pick_hint = wrapped_note(tr("Click a stream in the picture to change it."))
        lay.addWidget(self._pick_hint)
        return card

    def _picked(self) -> Optional[Tile]:
        tiles = self._model.layout().tiles
        return tiles[self.view.selected] if 0 <= self.view.selected < len(tiles) else None

    def _show_tile(self):
        tile = self._picked()
        self._tile_box.setVisible(tile is not None)
        self._pick_hint.setVisible(tile is None)
        self._fit_btn.setVisible(tile is not None)
        self._tile_name.setText(tile.name or tile.source_id if tile is not None else "")
        if tile is None:
            return
        for slider, value in ((self._zoom, round(tile.zoom * 100)), (self._border, tile.border),
                              (self._radius, tile.radius)):
            slider.blockSignals(True)
            slider.setValue(value)
            slider.blockSignals(False)
        self._zoom_lbl.setText(f"{round(tile.zoom * 100)} %")
        self._border_lbl.setText(f"{tile.border} px")
        self._radius_lbl.setText(f"{tile.radius} px")
        self._border_colour.set_colour(tile.border_colour)
        self._border_code.setText(tile.border_colour)
        tiles = self._model.layout().tiles
        self._fit_btn.setEnabled(any(sid == tile.source_id for sid, _n, _a in self._model.streams()))
        self._back_btn.setEnabled(self.view.selected > 0)
        self._front_btn.setEnabled(self.view.selected < len(tiles) - 1)

    def _edit_tile(self, **changes):
        tile = self._picked()
        if tile is not None:
            self._set_picked(replace(tile, **changes))

    def _set_picked(self, tile: Tile):
        layout = self._model.layout()
        tiles = list(layout.tiles)
        tiles[self.view.selected] = tile
        self._model.set_layout(replace(layout, tiles=tuple(tiles)))
        self._show_tile()
        self.view.refresh()

    def _fit_shape(self):
        tile = self._picked()
        aspect = next((a for sid, _n, a in self._model.streams() if tile is not None and sid == tile.source_id), None)
        if aspect is None:
            return
        w, h = self._model.size()
        self._set_picked(composite.fitted(tile, aspect, w / h))

    def _restack(self, step: int):
        layout = self._model.layout()
        index = self.view.selected
        tiles = composite.raised(layout.tiles, index, step)
        self._model.set_layout(replace(layout, tiles=tiles))
        self.view.selected = min(max(0, index + step), len(tiles) - 1)
        self._show_tile()
        self.view.refresh()

    # ── Background ────────────────────────────────────────────────────────

    def _build_background(self) -> QWidget:
        card, lay, _header = self._card(tr("Background"), "image")
        self._bg_colour = _ColourButton(tr("Background colour"))
        self._bg_colour.chosen.connect(lambda c: self._edit_layout(background=c))
        swatch, self._bg_code = _swatch_row(self._bg_colour)
        lay.addLayout(control_row(tr("Colour"), swatch, _LABEL_W, stretch=True))
        self._picture_name = ElidingLabel()
        self._picture_name.setObjectName("dim")
        self._picture_btn = QPushButton(tr("Picture…"))
        self._picture_btn.setToolTip(tr("Put a picture behind the streams instead of the colour"))
        self._picture_btn.clicked.connect(self._ask_picture)
        self._no_picture_btn = icon_button("x", tr("No picture"))
        self._no_picture_btn.clicked.connect(lambda: self._edit_layout(picture=""))
        row = QHBoxLayout()
        row.setContentsMargins(0, 0, 0, 0)
        row.setSpacing(6)
        row.addWidget(self._picture_name, 1)
        row.addWidget(self._picture_btn)
        row.addWidget(self._no_picture_btn)
        lay.addLayout(control_row(tr("Picture"), row, _LABEL_W, stretch=True))
        return card

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
        self._bg_code.setText(layout.background)
        self._picture_name.setText(os.path.basename(layout.picture) if layout.picture else tr("None"))
        self._no_picture_btn.setVisible(bool(layout.picture))
        self.view.refresh()
