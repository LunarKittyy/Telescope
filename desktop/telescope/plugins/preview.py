import cv2
import numpy as np
from PyQt6.QtCore import Qt, QEvent, QLineF, QObject, QPoint, QRectF, QSize, QTimer, QVariantAnimation, pyqtSignal
from PyQt6.QtGui import QColor, QFont, QFontMetricsF, QIcon, QImage, QPainter, QPen, QPixmap
from PyQt6.QtWidgets import (
    QFrame, QHBoxLayout, QLabel, QPushButton, QSizePolicy, QVBoxLayout, QWidget,
)

from telescope.widgets.common import set_button_icon
from telescope.widgets.line_icons import line_icon
from telescope import pose, theme, vision
from telescope.plugin import TelescopePlugin
from telescope.shortcuts import button_action
from telescope.view import shown_to_view, view_to_shown
from telescope.widgets.common import create_vector_icon, set_ui_role, ui_px
from telescope.i18n import N_, tr


# Translators: the preview area doesn't wrap lines, so keep the line breaks and each line about as long as this one
_IDLE_TEXT    = N_("Not streaming\n\nPress Start streaming and the phone's camera comes up on its own.")
_IDLE_BROWSER_TEXT = N_("Not streaming\n\nPress Start streaming and the browser's camera comes up once it connects.")
_IDLE_MIC_TEXT = N_("Not streaming\n\nStart mic only streams the phone's mic with its camera off.")
_WAITING_TEXT = N_("Waiting for the first frame\u2026")
_CAMERA_OFF_TEXT = N_("Camera off\n\nThe phone's mic is still streaming.\nStop streaming ends it.")
# Broken by hand like the rest: the stage doesn't wrap, and these are its longest lines
_CAMERA_OFF_AUTO_TEXT = N_("Camera off\n\nNo app was using the camera, so it turned off.\nThe phone's mic is still "
                           "streaming, and the camera\ncomes back when an app opens it.")


class _Sig(QObject):
    frame = pyqtSignal(object, int)  # the frame, and the stream it's from (PreviewPlugin._stream_gen)


_MARKER = 44  # the square drawn where you clicked, in px
_DRAG_START = 4  # px the mouse has to move before a press is a drag, not a click
_WHEEL_STEP = 1.1  # zoom factor per wheel notch
_BOXES_HOLD_MS = 1000  # the lens boxes stay this long after the framing last moved, then fade
_BOXES_FADE_MS = 300
_PLACEHOLDER_ICON = 52  # px, the camera-off icon over the stage's text
_SKELETON_OWNER = "Preview skeleton"  # what asks vision for people while the skeleton is on


def skeleton(people, map_point, aspect: float) -> list:
    """[(x0, y0, x1, y1, sure, out)] lines to draw for people, in the frame as shown (0..1): sure when the model is
    sure of both ends, out for an arm Auto framing would count as held out."""
    lines = []
    for person in people:
        out_sides = pose.hands_out([person], aspect)
        for a, b in pose.BONES:
            pa, pb = person.point(a, pose.ARM_SEEN), person.point(b, pose.ARM_SEEN)
            if pa is None or pb is None:
                continue
            sure = person.point(a, pose.SEEN) is not None and person.point(b, pose.SEEN) is not None
            side, part = b.split("_", 1) if "_" in b else ("", b)
            out = side in out_sides and part in ("elbow", "wrist", "index", "pinky", "thumb")  # that arm and hand
            lines.append((*map_point(*pa), *map_point(*pb), sure, out))
    return lines


def _fit_within(frame: np.ndarray, size) -> np.ndarray:
    """frame scaled down to fit inside size (w, h), keeping its shape (never up); always a new array, since it crosses
    threads."""
    h, w = frame.shape[:2]
    if size is None:
        return frame.copy()
    scale = min(size[0] / w, size[1] / h, 1.0)  # never bigger than the stream: the UI thread scales the pixmap up
    new_w, new_h = max(1, round(w * scale)), max(1, round(h * scale))
    if (new_w, new_h) == (w, h):
        return frame.copy()
    # INTER_AREA is slow at uneven scales, so shrink by a whole factor with it first and finish with INTER_LINEAR
    step = int(1 / scale)
    if step >= 2:
        frame = cv2.resize(frame, (w // step, h // step), interpolation=cv2.INTER_AREA)
    return cv2.resize(frame, (new_w, new_h), interpolation=cv2.INTER_LINEAR)


def _show(lbl: QLabel, pixmap: QPixmap):
    """Put pixmap on lbl, letterboxed. The worker already sized it for lbl; only a resize since then needs a scale."""
    w, h = lbl.width(), lbl.height()
    pw, ph = pixmap.width(), pixmap.height()
    if not (pw <= w and ph <= h and (w - pw <= 1 or h - ph <= 1)):
        pixmap = pixmap.scaled(w, h, Qt.AspectRatioMode.KeepAspectRatio, Qt.TransformationMode.SmoothTransformation)
    lbl.setPixmap(pixmap)


class FrameLabel(QLabel):
    """A QLabel showing a letterboxed frame. Reports clicks as a point in the frame (0..1), drags and
    wheel turns for panning and zooming, and draws the lens boxes over the frame (never into it)."""

    picked = pyqtSignal(float, float)
    dragged = pyqtSignal(float, float)            # moved by (du, dv), fractions of the frame
    scrolled = pyqtSignal(float, float, float)    # zoom factor, and the (u, v) under the mouse

    def __init__(self, parent=None):
        super().__init__(parent)
        self._pickable = False
        self._pannable = False
        self._press = None      # where the left button went down, while it's down on the frame
        self._last = None       # the last position a drag was reported from
        self._dragging = False
        self._boxes: list = []  # [(label, x0, y0, x1, y1)] in the frame, 0..1
        self._boxes_opacity = 0.0
        self._skeleton: list = []  # skeleton() lines
        self._placeholder_icon: QIcon | None = None  # drawn above the text while there's no frame
        self._overlays = None  # the host's PreviewOverlays: plugins' layers, and the pick under way
        self._pick_press = False  # the left button went down on a pick, so moves and the release are the pick's
        self._marker = QFrame(self)
        self._marker.setObjectName("focus_marker")
        self._marker.setAttribute(Qt.WidgetAttribute.WA_TransparentForMouseEvents)
        self._marker.resize(_MARKER, _MARKER)
        self._marker.hide()
        self._marker_timer = QTimer(self)
        self._marker_timer.setSingleShot(True)
        self._marker_timer.timeout.connect(self._marker.hide)

    def set_pickable(self, on: bool):
        self._pickable = on
        self._update_cursor()

    def set_pannable(self, on: bool):
        self._pannable = on
        self._update_cursor()

    def set_boxes(self, boxes: list, opacity: float):
        if boxes != self._boxes or opacity != self._boxes_opacity:
            self._boxes, self._boxes_opacity = boxes, opacity
            self.update()

    def set_skeleton(self, lines: list):
        if lines != self._skeleton:
            self._skeleton = lines
            self.update()

    def set_overlays(self, overlays):
        self._overlays = overlays
        overlays.changed.connect(self.update)
        overlays.pick_changed.connect(self._on_pick_changed)
        self._on_pick_changed(overlays.picking())

    def _picking(self) -> bool:
        return self._overlays is not None and bool(self._overlays.picking())

    def _on_pick_changed(self, owner: str):
        self._pick_press = False
        self.setMouseTracking(bool(owner))  # the cursor shows what a press would grab
        self.setFocusPolicy(Qt.FocusPolicy.ClickFocus if owner else Qt.FocusPolicy.NoFocus)
        if owner and self.isVisible():
            self.setFocus()
        self._update_cursor()

    def _frame_size(self) -> tuple:
        rect = self.frame_rect()
        return (rect.width(), rect.height()) if rect is not None else (1.0, 1.0)

    def _frame_uv(self, pos: QPoint):
        """Where pos falls against the shown frame, (u, v), outside 0..1 too; None without a frame."""
        rect = self.frame_rect()
        if rect is None:
            return None
        return (pos.x() - rect.x()) / rect.width(), (pos.y() - rect.y()) / rect.height()

    def set_placeholder_icon(self, icon: QIcon | None):
        self._placeholder_icon = icon
        self.update()

    def _update_cursor(self, pick_shape=None):
        if pick_shape is not None:
            shape = pick_shape
        elif self._picking() and self._overlays.cursor(0.5, 0.5, self._frame_size()) == Qt.CursorShape.CrossCursor:
            shape = Qt.CursorShape.CrossCursor  # a point or an area: anywhere on the picture
        elif self._dragging:
            shape = Qt.CursorShape.ClosedHandCursor
        elif self._pannable:
            shape = Qt.CursorShape.OpenHandCursor
        elif self._pickable:
            shape = Qt.CursorShape.CrossCursor
        else:
            shape = Qt.CursorShape.ArrowCursor
        self.setCursor(shape)

    def frame_rect(self):
        """Where the frame sits inside the letterbox, in widget px; None without one."""
        pm = self.pixmap()
        if pm is None or pm.isNull():
            return None
        pw, ph = pm.width() / pm.devicePixelRatio(), pm.height() / pm.devicePixelRatio()
        return QRectF((self.width() - pw) / 2, (self.height() - ph) / 2, pw, ph)

    def frame_point(self, pos: QPoint):
        """Where pos falls in the shown frame, as (u, v) in 0..1, or None in the letterbox bars."""
        rect = self.frame_rect()
        if rect is None:
            return None
        u, v = (pos.x() - rect.x()) / rect.width(), (pos.y() - rect.y()) / rect.height()
        if not (0.0 <= u <= 1.0 and 0.0 <= v <= 1.0):
            return None
        return u, v

    def mousePressEvent(self, event):
        pos = event.position().toPoint()
        if self._picking():
            if event.button() == Qt.MouseButton.RightButton:
                self._overlays.cancel_pick()
                return
            point = self.frame_point(pos)
            if (event.button() == Qt.MouseButton.LeftButton and point is not None
                    and self._overlays.press(*point, self._frame_size())):
                self._pick_press = True
                self.setFocus()
                return
        if (event.button() != Qt.MouseButton.LeftButton or not (self._pickable or self._pannable)
                or self.frame_point(pos) is None):
            super().mousePressEvent(event)
            return
        self._press = self._last = pos
        self._dragging = False

    def mouseMoveEvent(self, event):
        uv = self._frame_uv(event.position().toPoint())
        if self._pick_press and uv is not None:
            self._overlays.move(*uv, self._frame_size())
            self._update_cursor(self._overlays.cursor(*uv, self._frame_size()))
            return
        if self._picking() and self._press is None and uv is not None:
            self._update_cursor(self._overlays.cursor(*uv, self._frame_size()))
        if self._press is None or not self._pannable:
            super().mouseMoveEvent(event)
            return
        pos = event.position().toPoint()
        if not self._dragging:
            if (pos - self._press).manhattanLength() < ui_px(_DRAG_START):
                return
            self._dragging = True
            self._update_cursor()
        rect = self.frame_rect()
        if rect is not None:
            self.dragged.emit((pos.x() - self._last.x()) / rect.width(), (pos.y() - self._last.y()) / rect.height())
        self._last = pos

    def mouseReleaseEvent(self, event):
        if self._pick_press and event.button() == Qt.MouseButton.LeftButton:
            self._pick_press = False
            uv = self._frame_uv(event.position().toPoint()) or (0.5, 0.5)
            self._overlays.release(*uv)
            return
        if self._press is None or event.button() != Qt.MouseButton.LeftButton:
            super().mouseReleaseEvent(event)
            return
        press, was_drag = self._press, self._dragging
        self._press = self._last = None
        self._dragging = False
        self._update_cursor()
        point = self.frame_point(press)
        if was_drag or not self._pickable or point is None or self._picking():
            return
        self._marker.move(press.x() - _MARKER // 2, press.y() - _MARKER // 2)
        self._marker.show()
        self._marker.raise_()
        self._marker_timer.start(1000)
        self.picked.emit(*point)

    def keyPressEvent(self, event):
        if self._overlays is not None and self._overlays.key(event.key()):
            return
        super().keyPressEvent(event)

    def wheelEvent(self, event):
        notches = event.angleDelta().y() / 120
        point = self.frame_point(event.position().toPoint())
        if not notches or point is None:
            super().wheelEvent(event)
            return
        self.scrolled.emit(_WHEEL_STEP ** notches, *point)

    def paintEvent(self, event):
        super().paintEvent(event)
        rect = self.frame_rect()
        if rect is None and self._placeholder_icon is not None and self.text():
            self._paint_placeholder_icon()
        warped = self._overlays is not None and self._overlays.warping()  # drawn where the picture no longer is
        if rect is not None and self._skeleton and not warped:
            self._paint_skeleton(rect)
        if rect is not None and self._overlays is not None:
            p = QPainter(self)
            p.setClipRect(rect)
            self._overlays.paint(p, rect)
            p.end()
        if rect is None or not self._boxes or self._boxes_opacity <= 0.0 or warped:
            return
        p = QPainter(self)
        p.setClipRect(rect)
        p.setOpacity(self._boxes_opacity)
        font = QFont(self.font())
        font.setPointSizeF(8.0)
        font.setBold(True)
        p.setFont(font)
        fm = QFontMetricsF(font)
        halo, line = ui_px(4), ui_px(2)
        for label, x0, y0, x1, y1 in self._boxes:
            eps = 1e-3
            if x0 <= eps and y0 <= eps and x1 >= 1 - eps and y1 >= 1 - eps:
                continue  # the whole frame is inside it (exactly on the lens, or past it): no edge to show
            # Edges on whole pixels, so an even-width line covers whole pixels and stays crisp.
            box = QRectF(round(rect.x() + x0 * rect.width()), round(rect.y() + y0 * rect.height()), 0, 0)
            box.setRight(round(rect.x() + x1 * rect.width()))
            box.setBottom(round(rect.y() + y1 * rect.height()))
            p.setBrush(Qt.BrushStyle.NoBrush)
            p.setPen(QPen(QColor(0, 0, 0, 120), halo))
            p.drawRect(box)
            p.setPen(QPen(QColor(255, 255, 255, 235), line))
            p.drawRect(box)
            # The label in the box's top-left corner, or the corner of the part of it still in view.
            seen = box.intersected(rect)
            pad = ui_px(4)
            tag = QRectF(seen.x() + line + pad, seen.y() + line + pad,
                         fm.horizontalAdvance(label) + 2 * pad, fm.height() + pad)
            if tag.right() + pad > seen.right() or tag.bottom() + pad > seen.bottom():
                continue  # only a sliver of the box is in view: a label there would stick out of it
            p.setPen(Qt.PenStyle.NoPen)
            p.setBrush(QColor(0, 0, 0, 150))
            p.drawRoundedRect(tag, ui_px(3), ui_px(3))
            p.setPen(QColor(255, 255, 255))
            p.drawText(tag, Qt.AlignmentFlag.AlignCenter, label)


    def _paint_skeleton(self, rect: QRectF):
        p = QPainter(self)
        p.setRenderHint(QPainter.RenderHint.Antialiasing)
        p.setClipRect(rect)
        bone, out = QColor(theme.hue("vision")), QColor(theme.WARN)
        for halo in (True, False):
            for x0, y0, x1, y1, sure, held_out in self._skeleton:
                if halo:
                    color, width = QColor(0, 0, 0, 110), ui_px(5)
                else:
                    color, width = QColor(out if held_out else bone), ui_px(3 if held_out else 2)
                    color.setAlpha(235 if sure else 110)
                pen = QPen(color, width)
                pen.setCapStyle(Qt.PenCapStyle.RoundCap)
                p.setPen(pen)
                p.drawLine(QLineF(rect.x() + x0 * rect.width(), rect.y() + y0 * rect.height(),
                                  rect.x() + x1 * rect.width(), rect.y() + y1 * rect.height()))
        p.end()

    def _paint_placeholder_icon(self):
        """The icon centred just above the (centred) placeholder text."""
        size, gap = ui_px(_PLACEHOLDER_ICON), ui_px(14)
        text_h = self.fontMetrics().lineSpacing() * (self.text().count("\n") + 1)
        top = (self.height() - text_h) / 2 - gap - size
        if top < 0:
            return  # too short a stage: the text alone
        p = QPainter(self)
        p.drawPixmap(round((self.width() - size) / 2), round(top), self._placeholder_icon.pixmap(QSize(size, size)))
        p.end()


class _PopoutWindow(QWidget):
    """Floating preview window that enforces the stream's aspect ratio on resize."""

    closed = pyqtSignal()

    def __init__(self, parent=None):
        super().__init__(parent, Qt.WindowType.Window)
        self.setWindowTitle(tr("Telescope - Video Preview"))
        self.setMinimumSize(320, 180)
        self._aspect: float = 16 / 9
        self._adjusting = False

        self._lbl = FrameLabel()
        self._lbl.setAlignment(Qt.AlignmentFlag.AlignCenter)
        self._lbl.setSizePolicy(QSizePolicy.Policy.Expanding, QSizePolicy.Policy.Expanding)
        self._lbl.setStyleSheet("background: #000;")

        lay = QVBoxLayout(self)
        lay.setContentsMargins(0, 0, 0, 0)
        lay.addWidget(self._lbl)

    def set_frame(self, pixmap: QPixmap, aspect: float):
        self._aspect = aspect
        _show(self._lbl, pixmap)

    def resizeEvent(self, event):
        super().resizeEvent(event)
        if self._adjusting or self._aspect <= 0:
            return
        self._adjusting = True
        target_h = round(self.width() / self._aspect)
        if abs(target_h - self.height()) > 4:
            self.resize(self.width(), target_h)
        self._adjusting = False

    def closeEvent(self, event):
        self.closed.emit()
        super().closeEvent(event)


class _HostFilter(QObject):
    """Event filter installed on the main window to detect hide/show."""
    visibility_changed = pyqtSignal(bool)

    def eventFilter(self, obj, event):
        if event.type() == QEvent.Type.Hide:
            self.visibility_changed.emit(False)
        elif event.type() == QEvent.Type.Show:
            self.visibility_changed.emit(True)
        return False


class PreviewPlugin(TelescopePlugin):
    name = "preview"
    title = N_("Preview")
    about = N_("The live picture in the middle of the window, and the camera on/off button under it.")
    category = "preview"
    icon = "eye"
    when_off = N_("Without it the window doesn't show the picture but apps using the virtual camera still get it.")
    panel_region = "center"
    sees_final_frame = True  # shows what apps get, added plugins included

    # Max width for in-window preview sent across thread, until the stage's size is known.
    _CARD_MAX_W = 960

    def setup(self, host, bus):
        self._host   = host
        # On by default (toggle is escape hatch for decode savings).
        self._active = True
        self._popout: _PopoutWindow | None = None
        # Flag; process_frame() runs on stream thread and must never touch self._popout (not thread-safe).
        self._popout_active = False
        self._hid_for_popout = False  # opening the popout hid the card, so closing it shows the card again
        self._streaming = False  # a frame queued just before Stop arrives after it and is dropped
        self._stream_gen = 0  # each stream the panels show, so one queued from the last one is dropped too
        self._busy   = False
        # Where each view last showed a frame, in px (UI thread writes, stream thread reads): frames are scaled to it
        # on the stream thread, so the UI thread only has to put them up.
        self._card_size: tuple | None = None
        self._popout_size: tuple | None = None
        # Skip decoding for the card while the window is in the tray, without flipping the user's Hide/Show choice.
        self._host_visible = True
        self._sig    = _Sig()
        self._sig.frame.connect(self._on_frame)

        self._host_filter = _HostFilter()
        self._host_filter.visibility_changed.connect(self._on_host_visibility)
        host.installEventFilter(self._host_filter)
        bus.setup_needed.connect(self._on_setup_needed)
        self._bus = bus
        self._overlays = host.preview_overlays()  # what plugins draw on the preview, and their picks
        bus.preview_pick_changed.connect(self._on_pick_changed)
        self._pickable = False
        self._pannable = False
        bus.focus_point_available.connect(self._on_focus_point_available)
        # Lens boxes: shown for a moment whenever the framing moves, or all the time with the Lenses button.
        self._boxes: list = []
        self._boxes_opacity = 0.0
        self._boxes_hold = QTimer()
        self._boxes_hold.setSingleShot(True)
        self._boxes_hold.setInterval(_BOXES_HOLD_MS)
        self._boxes_fade = QVariantAnimation()
        self._boxes_fade.setStartValue(1.0)
        self._boxes_fade.setEndValue(0.0)
        self._boxes_fade.setDuration(_BOXES_FADE_MS)
        self._boxes_fade.valueChanged.connect(self._set_boxes_opacity)
        self._boxes_hold.timeout.connect(self._boxes_fade.start)
        bus.lens_boxes.connect(self._on_lens_boxes)
        bus.view_pannable.connect(self._on_pannable)
        self._camera_on = True
        self._phone_picked = True  # another source (Browser camera) has its own idle text
        bus.source_selected.connect(self._on_source_selected)
        bus.camera_on_changed.connect(self._on_camera_changed)
        for signal in (bus.mic_changed, bus.stream_started, bus.stream_stopped, bus.phone_state_updated):
            signal.connect(self._show_camera_btn)
        # The skeleton: the people vision finds, drawn over the frame while the Skeleton button is on
        self._people: list = []
        bus.people_seen.connect(self._on_people)
        bus.view_changed.connect(lambda _view: self._push_skeleton())
        bus.stream_stopped.connect(lambda: self._on_people("", [], force=True))
        bus.vision_state_changed.connect(lambda _state: self._show_skeleton_btn())

    def create_panel(self) -> QWidget:
        """Video stage: letterboxed frame area with toolbar beneath (centre column, no chrome)."""
        stage = QFrame()
        stage.setObjectName("preview_stage")
        stage.setSizePolicy(QSizePolicy.Policy.Expanding, QSizePolicy.Policy.Expanding)
        lay = QVBoxLayout(stage)
        lay.setContentsMargins(1, 1, 1, 1)
        lay.setSpacing(0)

        self._preview_lbl = FrameLabel()
        self._wire(self._preview_lbl)
        self._overlays.attach()
        self._preview_lbl.setObjectName("preview_surface")
        self._preview_lbl.setAlignment(Qt.AlignmentFlag.AlignCenter)
        self._preview_lbl.setMinimumHeight(240)
        self._preview_lbl.setSizePolicy(
            QSizePolicy.Policy.Ignored, QSizePolicy.Policy.Ignored)  # QLabel min hint ignored; frames scaled on arrival.
        self._preview_lbl.setMinimumWidth(1)
        self._preview_lbl.setText(tr(_IDLE_TEXT))
        lay.addWidget(self._preview_lbl, 1)

        toolbar = QWidget()
        toolbar.setObjectName("preview_toolbar")
        tb_lay = QHBoxLayout(toolbar)
        tb_lay.setContentsMargins(12, 9, 12, 10)
        tb_lay.setSpacing(8)

        self._toggle_btn = QPushButton(tr("Hide"))
        self._toggle_btn.setMinimumWidth(ui_px(78))
        set_ui_role(self._toggle_btn, "quiet")
        self._toggle_btn.setToolTip(
            tr("Stop drawing this preview. The camera output doesn't change.")
        )
        self._toggle_btn.clicked.connect(self._toggle)
        set_button_icon(self._toggle_btn, line_icon("eye_off", theme.TEXT_DIM))
        tb_lay.addWidget(self._toggle_btn)

        # The phone's camera, like a call app's: off leaves the stream to the mic. Icon only, like the lens toggle
        self._camera_btn = QPushButton()
        self._camera_btn.setFixedWidth(ui_px(36))
        self._camera_btn.setAccessibleName(tr("Phone camera"))
        set_ui_role(self._camera_btn, "quiet")
        self._camera_btn.setIconSize(QSize(16, 16))
        self._camera_btn.clicked.connect(lambda: self._host.set_camera_on(not self._host.is_camera_on()))
        tb_lay.addWidget(self._camera_btn)
        self._show_camera_btn()

        tb_lay.addStretch()

        self._skeleton_btn = QPushButton()
        self._skeleton_btn.setFixedWidth(ui_px(36))
        self._skeleton_btn.setAccessibleName(tr("Skeleton"))
        set_ui_role(self._skeleton_btn, "quiet")
        icon = QIcon()
        for color, state in ((theme.TEXT_DIM, QIcon.State.Off), (theme.ACCENT_SOFT, QIcon.State.On)):
            icon.addPixmap(line_icon("skeleton", color).pixmap(QSize(32, 32)), QIcon.Mode.Normal, state)
        self._skeleton_btn.setIcon(icon)
        self._skeleton_btn.setIconSize(QSize(16, 16))
        self._skeleton_btn.setCheckable(True)
        self._skeleton_btn.setToolTip(tr("Draw the skeleton MediaPipe finds. Doesn't show in the camera output."))
        self._skeleton_btn.toggled.connect(self._on_skeleton)
        tb_lay.addWidget(self._skeleton_btn)
        self._show_skeleton_btn()

        # Icon only, at a fixed width: the stage is the narrowest column, and no style may widen it.
        self._lenses_btn = QPushButton()
        self._lenses_btn.setFixedWidth(ui_px(36))
        self._lenses_btn.setAccessibleName(tr("Lens outlines"))
        set_ui_role(self._lenses_btn, "quiet")
        # With no text, the icon alone has to show whether it's on, off or unavailable.
        icon = QIcon()
        for color, mode, state in ((theme.TEXT_DIM, QIcon.Mode.Normal, QIcon.State.Off),
                                   (theme.ACCENT_SOFT, QIcon.Mode.Normal, QIcon.State.On),
                                   (theme.TEXT_DISABLED, QIcon.Mode.Disabled, QIcon.State.Off),
                                   (theme.TEXT_DISABLED, QIcon.Mode.Disabled, QIcon.State.On)):
            icon.addPixmap(create_vector_icon("lenses", color).pixmap(QSize(32, 32)), mode, state)
        self._lenses_btn.setIcon(icon)
        self._lenses_btn.setIconSize(QSize(16, 16))
        self._lenses_btn.setCheckable(True)
        self._lenses_btn.setEnabled(False)
        self._lenses_btn.setToolTip(
            tr("Keep showing what the phone's longer lenses see. Doesn't show in the camera output.")
        )
        self._lenses_btn.toggled.connect(self._push_boxes)
        tb_lay.addWidget(self._lenses_btn)

        self._popout_btn = QPushButton(tr("Pop out"))
        self._popout_btn.setMinimumWidth(ui_px(92))
        set_ui_role(self._popout_btn, "quiet")
        set_button_icon(self._popout_btn, line_icon("pop_out", theme.TEXT_DIM))
        self._popout_btn.clicked.connect(self._open_popout)
        tb_lay.addWidget(self._popout_btn)

        lay.addWidget(toolbar)

        self._stage = stage
        return stage

    def _on_setup_needed(self, needed: bool):
        # The first-run checklist takes the stage; there's nothing to preview before setup anyway.
        self._stage.setVisible(not needed)

    def create_actions(self) -> list:
        g = tr("Preview")
        return [
            button_action("preview.show_hide", tr("Show or hide the preview"), g, self._toggle_btn),
            button_action("preview.lens_outlines", tr("Lens outlines"), g, self._lenses_btn),
            button_action("preview.skeleton", tr("Skeleton"), g, self._skeleton_btn),
            button_action("preview.pop_out", tr("Pop out the preview"), g, self._popout_btn),
        ]

    def _toggle(self):
        self._active = not self._active
        self._toggle_btn.setText(tr("Hide") if self._active else tr("Show"))
        set_button_icon(self._toggle_btn, line_icon("eye_off" if self._active else "eye", theme.TEXT_DIM))
        if not self._active:
            self._preview_lbl.setPixmap(QPixmap())
            self._preview_lbl.setText(tr("Preview hidden"))
        else:
            self._preview_lbl.setText(self._idle_text())

    def _on_source_selected(self, sid: str):
        self._phone_picked = not sid
        if self._active and not self._host.is_streaming():
            self._preview_lbl.setText(self._idle_text())

    def _idle_text(self) -> str:
        """What the stage says with no frame to show."""
        if not self._host.is_streaming():
            if not self._host.is_camera_on():
                return tr(_IDLE_MIC_TEXT)
            return tr(_IDLE_TEXT) if self._phone_picked else tr(_IDLE_BROWSER_TEXT)
        if not self._host.is_camera_on():
            return tr(_CAMERA_OFF_AUTO_TEXT) if self._host.is_camera_off_auto() else tr(_CAMERA_OFF_TEXT)
        return tr(_WAITING_TEXT)

    def _clear(self):
        if self._popout is not None:
            self._popout._lbl.setPixmap(QPixmap())
        self._preview_lbl.setPixmap(QPixmap())
        self._preview_lbl.setText(self._idle_text() if self._active else tr("Preview hidden"))
        camera_off = self._host.is_streaming() and not self._host.is_camera_on()
        set_ui_role(self._preview_lbl, "camera_off" if camera_off else "")
        self._preview_lbl.set_placeholder_icon(create_vector_icon("camera_off", theme.ACCENT) if camera_off else None)

    def on_stream_start(self, stream_url: str, ctrl):
        self._streaming = True
        self._stream_gen += 1
        self._busy = False  # a frame lost on its way to the UI mustn't freeze the preview for good
        if self._active and self._preview_lbl.pixmap().isNull() and self._host.is_camera_on():
            self._preview_lbl.setText(tr(_WAITING_TEXT))  # with the camera off, on_camera_off follows and says so

    def on_stream_stop(self):
        self._streaming = False
        self._clear()

    def on_camera_off(self):
        self._clear()

    def on_camera_on(self):
        self._busy = False
        self._clear()

    def _on_camera_changed(self, on: bool):
        self._camera_on = on
        self._show_camera_btn()
        if self._preview_lbl.pixmap().isNull():
            self._clear()  # the idle text says what Start does

    def _show_camera_btn(self, *_):
        on = self._host.is_camera_on()
        can, why = self._host.can_turn_camera_off()
        self._camera_btn.setIcon(create_vector_icon("camera" if on else "camera_off",
                                                    theme.TEXT_DIM if on else theme.ACCENT))
        self._camera_btn.setEnabled(not on or can)
        if not on:
            tip = tr("Turn the phone's camera back on.")
        elif not can:
            tip = why
        elif self._host.is_streaming():
            tip = tr("Turn the phone's camera off. Its mic keeps streaming.")
        else:
            tip = tr("Start with the phone's camera off, streaming just its mic.")
        self._camera_btn.setToolTip(tip)
        self._camera_btn.setProperty("camera_off", not on)
        self._camera_btn.style().unpolish(self._camera_btn)
        self._camera_btn.style().polish(self._camera_btn)

    def _open_popout(self):
        if self._popout and self._popout.isVisible():
            self._popout.raise_()
            self._popout.activateWindow()
            return
        if self._active:
            self._toggle()
            self._hid_for_popout = True
        self._toggle_btn.setEnabled(False)

        self._popout = _PopoutWindow(None)
        self._popout.closed.connect(self._on_popout_closed)
        self._wire(self._popout._lbl)
        self._popout._lbl.set_pickable(self._pickable)
        self._popout._lbl.set_pannable(self._pannable)
        self._push_boxes()
        self._popout.resize(640, 360)
        self._popout.show()
        self._popout_size = None
        self._popout_active = True

    def _on_popout_closed(self):
        self._popout = None
        self._popout_active = False
        self._toggle_btn.setEnabled(True)
        if self._hid_for_popout and not self._active:
            self._toggle()
        self._hid_for_popout = False

    def _on_focus_point_available(self, available: bool):
        self._pickable = available
        self._preview_lbl.set_pickable(available)
        if self._popout is not None:
            self._popout._lbl.set_pickable(available)

    def _wire(self, lbl: FrameLabel):
        lbl.set_overlays(self._overlays)
        lbl.picked.connect(self._on_picked)
        lbl.dragged.connect(self._on_dragged)
        lbl.scrolled.connect(lambda factor, u, v: self._bus.view_scrolled.emit(factor, *self._unwarp(u, v)))

    def _unwarp(self, u: float, v: float) -> tuple:
        """A point on the preview back to the frame as shown with the view, past a plugin's warp (Desk mode)."""
        x, y = self._overlays.unwarp(u, v)
        return min(max(x, 0.0), 1.0), min(max(y, 0.0), 1.0)

    def _on_picked(self, u: float, v: float):
        self._bus.focus_point_picked.emit(*self._unwarp(u, v))

    def _on_dragged(self, du: float, dv: float):
        if not self._overlays.warping():
            self._bus.view_dragged.emit(du, dv)
            return
        (x0, y0), (x1, y1) = self._overlays.unwarp(0.5, 0.5), self._overlays.unwarp(0.5 + du, 0.5 + dv)
        self._bus.view_dragged.emit(x1 - x0, y1 - y0)

    def _labels(self):
        return [self._preview_lbl] + ([self._popout._lbl] if self._popout is not None else [])

    def _on_pick_changed(self, owner: str):
        if owner and not self._active and not self._popout_active:
            self._toggle()  # a pick needs the picture to pick on

    def _on_pannable(self, pannable: bool):
        self._pannable = pannable
        for lbl in self._labels():
            lbl.set_pannable(pannable)

    def _on_lens_boxes(self, boxes: list, moved: bool):
        self._boxes = boxes
        self._lenses_btn.setEnabled(bool(boxes))
        if moved and boxes:
            self._boxes_fade.stop()
            self._boxes_opacity = 1.0
            self._boxes_hold.start()
        self._push_boxes()

    def _set_boxes_opacity(self, opacity):
        self._boxes_opacity = float(opacity)
        self._push_boxes()

    def _push_boxes(self, *_):
        opacity = 1.0 if self._lenses_btn.isChecked() else self._boxes_opacity
        for lbl in self._labels():
            lbl.set_boxes(self._boxes, opacity)

    def _show_skeleton_btn(self):
        """Only with the vision download there; MediaPipe itself loads when the button goes on."""
        if not hasattr(self, "_skeleton_btn"):
            return
        there = self._host.vision_state() not in (vision.MISSING, vision.FAILED)
        self._skeleton_btn.setVisible(there)
        if not there and self._skeleton_btn.isChecked():
            self._skeleton_btn.setChecked(False)

    def _on_skeleton(self, on: bool):
        self._host.want_people(_SKELETON_OWNER, on)
        if not on:
            self._people = []
        self._push_skeleton()
        self._host.schedule_save()

    def _on_people(self, source_id: str, people: list, force: bool = False):
        if force or (self._skeleton_btn.isChecked() and source_id == (self._host.focused_source_id() or "")):
            self._people = people
            self._push_skeleton()

    def _push_skeleton(self):
        lines = []
        if self._people:
            view = self._host.view()
            flips = (view.flip_h, view.flip_v, view.rotation)

            def map_point(x, y):
                return view_to_shown(*shown_to_view(x, y, 1.0, 0.0, 0.0, *flips), view.zoom, view.pan_x,
                                     view.pan_y, *flips)
            lines = skeleton(self._people, map_point, self._host.view_aspect() or 16 / 9)
        for lbl in self._labels():
            lbl.set_skeleton(lines)

    def get_config(self) -> dict:
        return {"lens_boxes": self._lenses_btn.isChecked(), "skeleton": self._skeleton_btn.isChecked()}

    def set_config(self, cfg: dict):
        self._lenses_btn.setChecked(cfg.get("lens_boxes", False) is True)
        self._skeleton_btn.blockSignals(True)
        self._skeleton_btn.setChecked(cfg.get("skeleton", False) is True and not self._skeleton_btn.isHidden())
        self._skeleton_btn.blockSignals(False)
        self._host.want_people(_SKELETON_OWNER, self._skeleton_btn.isChecked())
        self._push_skeleton()

    def _on_host_visibility(self, visible: bool):
        self._host_visible = visible

    # ── Worker thread ─────────────────────────────────────────────────────────

    def process_frame(self, frame: np.ndarray) -> np.ndarray:
        popout_open = self._popout_active
        card_wanted = self._active and self._host_visible
        if not (card_wanted or popout_open) or self._busy:
            return frame
        self._busy = True
        try:
            if popout_open:
                size = self._popout_size
            else:
                h, w = frame.shape[:2]
                size = self._card_size or (min(w, self._CARD_MAX_W), h)
            self._sig.frame.emit(_fit_within(frame, size), self._stream_gen)
        except Exception:
            self._busy = False
            raise
        return frame

    # ── UI thread ─────────────────────────────────────────────────────────────

    def _on_frame(self, frame: np.ndarray, stream_gen: int | None = None):
        try:
            if stream_gen is not None and stream_gen != self._stream_gen:
                return  # the panels have moved on to another stream since
            if self._streaming and self._host.is_camera_on():  # a frame from just before the camera went off
                self._show_frame(self._overlays.tint(frame))  # here, not in process_frame: it adds no lag to the output
        finally:
            self._busy = False

    def _show_frame(self, frame: np.ndarray):
        h, w = frame.shape[:2]
        img = QImage(frame.data, w, h, w * 3, QImage.Format.Format_BGR888).copy()
        px = QPixmap.fromImage(img)

        if self._popout and self._popout.isVisible():
            self._popout.set_frame(px, w / h)
            lbl = self._popout._lbl
            self._popout_size = (lbl.width(), lbl.height())
        elif self._active:
            _show(self._preview_lbl, px)
            self._card_size = (self._preview_lbl.width(), self._preview_lbl.height())
