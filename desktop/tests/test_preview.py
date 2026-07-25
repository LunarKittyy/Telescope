import numpy as np
from PyQt6.QtCore import QEvent
from PyQt6.QtWidgets import QWidget

from telescope.plugin import EventBus
from telescope.plugins.preview import PreviewPlugin, _HostFilter, _PopoutWindow


class _Host(QWidget):
    """A window stand-in - the preview asks the host whether a stream is
    running to pick its placeholder text."""

    streaming = False

    def is_streaming(self):
        return self.streaming


def _plugin(qapp):
    host = _Host()
    plugin = PreviewPlugin()
    plugin.setup(host, EventBus())
    panel = plugin.create_panel()
    return plugin, host, panel


def test_host_filter_emits_on_hide_only(qapp):
    filt = _HostFilter()
    seen = []
    filt.hidden.connect(lambda: seen.append(True))

    assert filt.eventFilter(None, QEvent(QEvent.Type.Show)) is False
    assert seen == []
    assert filt.eventFilter(None, QEvent(QEvent.Type.Hide)) is False
    assert seen == [True]


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
    assert plugin._preview_lbl.text() == "Not streaming"


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
    assert plugin._preview_lbl.text() == "Not streaming"
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


def test_host_hide_closes_embedded_preview(qapp):
    plugin, _host, _panel = _plugin(qapp)
    plugin._toggle()
    plugin._on_host_hidden()
    assert plugin._active is False


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
