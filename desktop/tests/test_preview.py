import numpy as np
from PyQt6.QtCore import QEvent
from PyQt6.QtWidgets import QSizePolicy, QWidget

from telescope.plugin import EventBus
from telescope.plugins.preview import _IDLE_TEXT, PreviewPlugin, _HostFilter, _PopoutWindow


class _Host(QWidget):
    """Window stand-in; preview checks is_streaming() for placeholder text."""

    streaming = False

    def is_streaming(self):
        return self.streaming


def _plugin(qapp):
    host = _Host()
    plugin = PreviewPlugin()
    plugin.setup(host, EventBus())
    panel = plugin.create_panel()
    return plugin, host, panel


def test_host_filter_reports_hide_and_show(qapp):
    filt = _HostFilter()
    seen = []
    filt.visibility_changed.connect(seen.append)

    assert filt.eventFilter(None, QEvent(QEvent.Type.Hide)) is False
    assert filt.eventFilter(None, QEvent(QEvent.Type.Show)) is False
    assert seen == [False, True]


def test_preview_starts_active_and_toggles_off_and_back_on(qapp):
    plugin, _host, _panel = _plugin(qapp)

    # The stage is the centre of the window now, so it starts on.
    assert plugin._active is True
    assert plugin._toggle_btn.text() == "Hide"

    plugin._toggle()
    assert plugin._active is False
    assert plugin._toggle_btn.text() == "Show"
    assert plugin._preview_lbl.text() == "Preview hidden"

    plugin._toggle()
    assert plugin._active is True
    assert plugin._toggle_btn.text() == "Hide"
    assert plugin._preview_lbl.text() == _IDLE_TEXT


def test_preview_placeholder_says_waiting_while_a_stream_is_up(qapp):
    plugin, host, _panel = _plugin(qapp)
    host.streaming = True

    plugin._toggle()
    plugin._toggle()

    assert plugin._preview_lbl.text() == "Waiting for the first frame…"


def test_stream_start_and_stop_swap_the_placeholder(qapp):
    plugin, _host, _panel = _plugin(qapp)

    plugin.on_stream_start("http://phone/", None)
    assert plugin._preview_lbl.text() == "Waiting for the first frame…"

    plugin.on_stream_stop()
    assert plugin._preview_lbl.text() == _IDLE_TEXT
    assert plugin._preview_lbl.pixmap().isNull()


def test_process_frame_is_zero_copy_when_inactive_or_busy(qapp):
    plugin, _host, _panel = _plugin(qapp)
    frame = np.zeros((20, 30, 3), dtype=np.uint8)

    plugin._active = False
    assert plugin.process_frame(frame) is frame
    plugin._active = True
    plugin._busy = True
    assert plugin.process_frame(frame) is frame


def test_card_preview_downscales_large_frame_before_signal(qapp):
    plugin, _host, _panel = _plugin(qapp)
    plugin._active = True
    seen = []
    plugin._sig.frame.connect(lambda frame: seen.append(frame.copy()))
    frame = np.zeros((600, 1200, 3), dtype=np.uint8)

    returned = plugin.process_frame(frame)

    assert returned is frame
    assert seen[-1].shape == (480, 960, 3)
    assert plugin._busy is False


def test_card_preview_copies_small_frame_and_popout_keeps_full_resolution(qapp):
    plugin, _host, _panel = _plugin(qapp)
    seen = []
    plugin._sig.frame.connect(lambda frame: seen.append(frame))
    frame = np.zeros((100, 200, 3), dtype=np.uint8)

    plugin._active = True
    plugin.process_frame(frame)
    assert seen[-1].shape == frame.shape
    assert not np.shares_memory(seen[-1], frame)

    plugin._active = False
    plugin._popout_active = True
    plugin.process_frame(frame)
    assert seen[-1].shape == frame.shape


def test_open_and_close_popout_disables_then_restores_toggle(qapp):
    plugin, _host, _panel = _plugin(qapp)
    plugin._active = True

    plugin._open_popout()

    assert plugin._active is False
    assert plugin._popout_active is True
    assert plugin._popout is not None
    assert not plugin._toggle_btn.isEnabled()

    plugin._popout.close()
    qapp.processEvents()
    assert plugin._popout is None
    assert plugin._popout_active is False
    assert plugin._toggle_btn.isEnabled()


def test_second_popout_request_reuses_visible_window(qapp, monkeypatch):
    plugin, _host, _panel = _plugin(qapp)
    plugin._open_popout()
    existing = plugin._popout
    raised = []
    monkeypatch.setattr(existing, "raise_", lambda: raised.append("raise"))
    monkeypatch.setattr(existing, "activateWindow", lambda: raised.append("activate"))

    plugin._open_popout()

    assert plugin._popout is existing
    assert raised == ["raise", "activate"]
    existing.close()


def test_host_hide_pauses_the_card_without_turning_it_off(qapp):
    # Hiding to the tray used to flip the preview to "hidden" for good.
    plugin, _host, _panel = _plugin(qapp)
    assert plugin._active is True
    sent = []
    plugin._sig.frame.connect(sent.append)
    frame = np.zeros((4, 4, 3), dtype=np.uint8)

    plugin._on_host_visibility(False)
    plugin.process_frame(frame)
    assert sent == []
    assert plugin._active is True

    plugin._on_host_visibility(True)
    plugin.process_frame(frame)
    assert len(sent) == 1


def test_on_frame_updates_card_pixmap_and_clears_busy(qapp):
    plugin, _host, panel = _plugin(qapp)
    panel.resize(500, 300)
    plugin._active = True
    plugin._preview_lbl.resize(480, 180)
    plugin._busy = True

    plugin._on_frame(np.full((90, 160, 3), 128, dtype=np.uint8))

    assert not plugin._preview_lbl.pixmap().isNull()
    assert plugin._busy is False


def test_popout_set_frame_and_resize_guards(qapp):
    window = _PopoutWindow()
    frame = np.full((90, 160, 3), 128, dtype=np.uint8)
    from PyQt6.QtGui import QImage, QPixmap
    image = QImage(frame.data, 160, 90, 160 * 3, QImage.Format.Format_RGB888).copy()
    pixmap = QPixmap.fromImage(image)

    window.resize(640, 400)
    window.set_frame(pixmap, 16 / 9)
    assert window._aspect == 16 / 9
    assert not window._lbl.pixmap().isNull()

    window._aspect = 0
    window.resize(500, 300)
    assert window.size().width() == 500


def test_a_large_frame_does_not_pin_the_column_open(qapp):
    """Large pixmap doesn't pin preview column width open (QLabel minimum size issue)."""
    plugin, _host, panel = _plugin(qapp)
    plugin._preview_lbl.resize(900, 500)

    plugin.process_frame(np.zeros((1080, 1920, 3), dtype=np.uint8))
    qapp.processEvents()

    assert plugin._preview_lbl.minimumWidth() == 1
    assert plugin._preview_lbl.sizePolicy().horizontalPolicy() == \
        QSizePolicy.Policy.Ignored
    assert panel.minimumSizeHint().width() < 400


def test_a_click_on_the_frame_is_a_point_in_it_and_the_bars_are_ignored(qapp):
    from PyQt6.QtCore import QPoint, QPointF, Qt
    from PyQt6.QtGui import QMouseEvent, QPixmap
    plugin, _host, _panel = _plugin(qapp)
    picked = []
    plugin._bus.focus_point_picked.connect(lambda u, v: picked.append((round(u, 2), round(v, 2))))
    lbl = plugin._preview_lbl
    lbl.resize(400, 300)
    pm = QPixmap(400, 200)  # letterboxed: 50 px bars above and below
    lbl.setPixmap(pm)
    assert lbl.frame_point(QPoint(100, 150)) == (0.25, 0.5)
    assert lbl.frame_point(QPoint(100, 20)) is None

    def click(x, y):
        pos = QPointF(x, y)
        for kind, handler in ((QEvent.Type.MouseButtonPress, lbl.mousePressEvent),
                              (QEvent.Type.MouseButtonRelease, lbl.mouseReleaseEvent)):
            handler(QMouseEvent(kind, pos, lbl.mapToGlobal(pos), Qt.MouseButton.LeftButton,
                                Qt.MouseButton.LeftButton, Qt.KeyboardModifier.NoModifier))

    click(100, 150)
    assert picked == []  # not while the lens can't focus on a point
    plugin._bus.focus_point_available.emit(True)
    assert lbl.cursor().shape() == Qt.CursorShape.CrossCursor
    click(100, 150)
    click(100, 20)
    assert picked == [(0.25, 0.5)]
    assert not lbl._marker.isHidden()


def _mouse(lbl, kind, x, y):
    from PyQt6.QtCore import QPointF, Qt
    from PyQt6.QtGui import QMouseEvent
    pos = QPointF(x, y)
    buttons = Qt.MouseButton.NoButton if kind == QEvent.Type.MouseButtonRelease else Qt.MouseButton.LeftButton
    event = QMouseEvent(kind, pos, lbl.mapToGlobal(pos), Qt.MouseButton.LeftButton, buttons,
                        Qt.KeyboardModifier.NoModifier)
    {QEvent.Type.MouseButtonPress: lbl.mousePressEvent, QEvent.Type.MouseMove: lbl.mouseMoveEvent,
     QEvent.Type.MouseButtonRelease: lbl.mouseReleaseEvent}[kind](event)


def _framed(qapp):
    from PyQt6.QtGui import QPixmap
    plugin, host, panel = _plugin(qapp)
    lbl = plugin._preview_lbl
    lbl.resize(400, 300)
    lbl.setPixmap(QPixmap(400, 200))  # 50 px bars above and below
    return plugin, lbl


def test_dragging_the_preview_pans_instead_of_focusing(qapp):
    from PyQt6.QtCore import Qt
    plugin, lbl = _framed(qapp)
    picked, dragged = [], []
    plugin._bus.focus_point_picked.connect(lambda u, v: picked.append((u, v)))
    plugin._bus.view_dragged.connect(lambda du, dv: dragged.append((round(du, 3), round(dv, 3))))
    plugin._bus.focus_point_available.emit(True)
    plugin._bus.view_pannable.emit(True)
    assert lbl.cursor().shape() == Qt.CursorShape.OpenHandCursor

    _mouse(lbl, QEvent.Type.MouseButtonPress, 100, 150)
    _mouse(lbl, QEvent.Type.MouseMove, 102, 150)  # a wobble, still a click
    assert dragged == []
    _mouse(lbl, QEvent.Type.MouseMove, 140, 170)
    assert lbl.cursor().shape() == Qt.CursorShape.ClosedHandCursor
    _mouse(lbl, QEvent.Type.MouseMove, 180, 170)
    _mouse(lbl, QEvent.Type.MouseButtonRelease, 180, 170)
    assert dragged == [(0.1, 0.1), (0.1, 0.0)]  # from the press, as fractions of the 400x200 frame
    assert picked == []
    assert lbl.cursor().shape() == Qt.CursorShape.OpenHandCursor

    _mouse(lbl, QEvent.Type.MouseButtonPress, 100, 150)
    _mouse(lbl, QEvent.Type.MouseButtonRelease, 101, 150)
    assert picked == [(0.25, 0.5)]  # a click still focuses, where it was pressed


def test_the_wheel_over_the_frame_zooms_around_the_mouse(qapp):
    from PyQt6.QtCore import QPoint, QPointF, Qt
    from PyQt6.QtGui import QWheelEvent
    plugin, lbl = _framed(qapp)
    seen = []
    plugin._bus.view_scrolled.connect(lambda f, u, v: seen.append((round(f, 4), u, v)))

    def wheel(x, y, dy):
        pos = QPointF(x, y)
        lbl.wheelEvent(QWheelEvent(pos, lbl.mapToGlobal(pos), QPoint(), QPoint(0, dy), Qt.MouseButton.NoButton,
                                   Qt.KeyboardModifier.NoModifier, Qt.ScrollPhase.NoScrollPhase, False))

    wheel(100, 150, 120)
    wheel(100, 150, -240)
    wheel(100, 20, 120)  # in the bars: nothing
    assert seen == [(1.1, 0.25, 0.5), (round(1 / 1.21, 4), 0.25, 0.5)]


def test_lens_boxes_show_for_a_moment_after_a_move_then_fade(qapp):
    plugin, lbl = _framed(qapp)
    boxes = [("3×", 0.33, 0.33, 0.67, 0.67)]
    plugin._bus.lens_boxes.emit(boxes, False)
    assert lbl._boxes == boxes and lbl._boxes_opacity == 0.0  # known, but nothing moved
    assert plugin._lenses_btn.isEnabled()
    plugin._bus.lens_boxes.emit(boxes, True)
    assert lbl._boxes_opacity == 1.0
    lbl.grab()  # paints them
    plugin._boxes_hold.timeout.emit()
    plugin._boxes_fade.setCurrentTime(plugin._boxes_fade.duration())
    assert lbl._boxes_opacity == 0.0

    plugin._lenses_btn.setChecked(True)
    assert lbl._boxes_opacity == 1.0  # always on
    assert plugin.get_config() == {"lens_boxes": True}
    plugin.set_config({"lens_boxes": False})
    assert not plugin._lenses_btn.isChecked() and lbl._boxes_opacity == 0.0

    plugin._bus.lens_boxes.emit([], False)
    assert not plugin._lenses_btn.isEnabled()


def test_the_popout_pans_zooms_and_shows_boxes_too(qapp):
    plugin, lbl = _framed(qapp)
    boxes = [("3×", 0.33, 0.33, 0.67, 0.67)]
    plugin._bus.view_pannable.emit(True)
    plugin._bus.lens_boxes.emit(boxes, True)
    plugin._open_popout()
    try:
        pop = plugin._popout._lbl
        assert pop._pannable and pop._boxes == boxes and pop._boxes_opacity == 1.0
        dragged = []
        plugin._bus.view_dragged.connect(lambda du, dv: dragged.append(du))
        pop.dragged.emit(0.2, 0.0)
        assert dragged == [0.2]
    finally:
        plugin._popout.close()
