"""QR code reader: Wi-Fi codes, what goes to the clipboard, and reading a code off a frame without holding it up."""

import time

import cv2
import numpy as np
import pytest
from PyQt6.QtGui import QGuiApplication

from telescope.overlays import PreviewOverlays
from telescope.plugin import EventBus
from telescope.plugins.qr_reader import QrReaderPlugin, Steady, Wifi, is_link, parse_wifi, plausible, read_code
from telescope.view import View


def test_wifi_codes_are_read_with_their_escapes():
    assert parse_wifi("WIFI:T:WPA;S:Home;P:hunter2;;") == Wifi("Home", "hunter2", "WPA", False)
    assert parse_wifi(r"WIFI:S:My\;Net\:5G;T:WPA;P:pa\\ss\,word;H:true;;") == Wifi("My;Net:5G", "pa\\ss,word", "WPA",
                                                                                   True)
    assert parse_wifi("wifi:T:nopass;S:Cafe;;") == Wifi("Cafe", "", "nopass", False)
    assert parse_wifi("WIFI:T:WPA;P:nameless;;") is None
    assert parse_wifi("https://example.com") is None


def test_what_goes_to_the_clipboard():
    password, line, _note = QrReaderPlugin.copy("WIFI:T:WPA;S:Home;P:hunter2;;")
    assert password == "hunter2" and "Home" in line and "hunter2" not in line  # the card never shows the password
    name, line, _note = QrReaderPlugin.copy("WIFI:T:nopass;S:Cafe;;")
    assert name == "Cafe" and "no password" in line
    link, line, note = QrReaderPlugin.copy("https://example.com/a")
    assert link == note == "https://example.com/a" and line.find("example.com") >= 0
    assert QrReaderPlugin.copy("just some text")[0] == "just some text"
    assert is_link("HTTPS://x.y") and not is_link("ftp://x.y")


def _code_frame(text, w=640, h=360):
    code = cv2.QRCodeEncoder.create().encode(text)
    code = cv2.resize(code, (code.shape[1] * 6, code.shape[0] * 6), interpolation=cv2.INTER_NEAREST)
    code = cv2.copyMakeBorder(code, 20, 20, 20, 20, cv2.BORDER_CONSTANT, value=255)
    frame = np.full((h, w, 3), 90, np.uint8)
    frame[40:40 + code.shape[0], 300:300 + code.shape[1]] = cv2.cvtColor(code, cv2.COLOR_GRAY2BGR)
    return frame


@pytest.mark.parametrize("scale", [4, 8, 14])
def test_codes_are_read_from_arm_s_length_to_right_up_close(scale):
    code = cv2.QRCodeEncoder.create().encode("https://example.com")
    code = cv2.resize(code, (code.shape[1] * scale, code.shape[0] * scale), interpolation=cv2.INTER_NEAREST)
    grey = np.full((720, 1280), 90, np.uint8)
    grey[10:10 + code.shape[0], 400:400 + code.shape[1]] = code
    text, corners = read_code(cv2.GaussianBlur(grey, (0, 0), 1.0), (cv2.QRCodeDetectorAruco(), cv2.QRCodeDetector()))
    assert text == "https://example.com"
    assert corners.shape == (4, 2) and 0.25 < corners[:, 0].min() < 0.45


_QUAD = np.array([[0.4, 0.2], [0.6, 0.2], [0.6, 0.55], [0.4, 0.55]], np.float32)


def test_a_quad_has_to_look_like_a_code():
    assert plausible([[100, 100], [200, 100], [200, 200], [100, 200]])
    assert plausible([[100, 100], [200, 110], [190, 190], [110, 200]])  # held at an angle
    assert not plausible([[100, 100], [200, 100], [120, 120], [100, 200]])  # caved in
    assert not plausible([[100, 100], [200, 200], [200, 100], [100, 200]])  # crossed over
    assert not plausible([[100, 100], [400, 100], [400, 112], [100, 112]])  # a sliver
    assert not plausible([[100, 100], [108, 100], [108, 108], [100, 108]])  # a speck
    assert not plausible([[100, 100], [200, 100], [200, 200]])


def test_one_look_at_a_stray_quad_never_counts():
    steady = Steady(looks=3)
    assert steady.see("", _QUAD) is None  # seen but not read
    assert steady.see("noise", _QUAD) is None
    assert steady.see("", None) is None
    assert steady.see("noise", _QUAD) is None and not steady.confirmed  # a miss in between starts it over
    assert steady.see("other", _QUAD) is None and steady.see("noise", _QUAD + 0.5) is None  # changed or jumped


def test_a_steady_code_counts_after_a_few_looks_and_rides_out_a_miss():
    steady = Steady(looks=3, grace=1)
    assert steady.see("hi", _QUAD) is None and steady.pending
    assert steady.see("hi", _QUAD + 0.01) is None
    text, corners = steady.see("hi", _QUAD + 0.02)
    assert text == "hi" and np.allclose(corners, _QUAD + 0.02) and not steady.pending
    assert steady.see("", None) == ("hi", corners)  # one missed look keeps it
    assert steady.see("hi", _QUAD)[0] == "hi"
    assert steady.see("", None) is not None and steady.see("", None) is None  # two in a row lets it go


def test_a_moving_code_stays_outlined():
    steady = Steady(looks=3)
    for i in range(12):
        found = steady.see("hi", _QUAD + [0.04 * i, 0.0])
        assert (found is None) == (i < 2)
    assert np.allclose(found[1], _QUAD + [0.44, 0.0])
    assert steady.see("hi", _QUAD - 0.3)[0] == "hi"  # a fast jump of the same code once it's confirmed


def test_no_code_is_none():
    assert read_code(np.full((90, 160), 90, np.uint8), (cv2.QRCodeDetectorAruco(), cv2.QRCodeDetector())) is None


class _Host:
    def __init__(self):
        self.overlays = PreviewOverlays(View)
        self.notified, self.refreshed, self.saves = [], 0, 0

    def add_preview_layer(self, owner, layer):
        self.overlays.add_layer(owner, layer)

    def remove_preview_layer(self, owner):
        self.overlays.remove_layer(owner)

    def refresh_preview(self):
        self.refreshed += 1

    def send_notification(self, title, body, urgent=True):
        self.notified.append((title, body, urgent))

    def schedule_save(self):
        self.saves += 1


@pytest.fixture
def reader(qt_widgets):
    host, bus = _Host(), EventBus()
    plugin = QrReaderPlugin()
    plugin.setup(host, bus)
    qt_widgets(plugin.create_panel())
    bus.phones_changed.emit(1)
    plugin.set_config({})
    return plugin, host


def _wait_for(qapp, done, seconds=5):
    deadline = time.monotonic() + seconds
    while not done() and time.monotonic() < deadline:
        qapp.processEvents()
        time.sleep(0.01)


def test_a_code_in_the_picture_goes_to_the_clipboard_once(reader, qapp):
    plugin, host = reader
    frame = _code_frame("WIFI:T:WPA;S:Telescope;P:hunter22;;")
    assert plugin.process_frame(frame) is frame and not plugin._busy.is_set()  # off: nothing looked at
    plugin._switch.setChecked(True)
    assert host.overlays.layers() == [plugin._layer]
    QGuiApplication.clipboard().setText("")
    assert plugin.process_frame(frame) is frame  # handed off: the frame goes on unchanged straight away
    _wait_for(qapp, lambda: not plugin._busy.is_set())
    qapp.processEvents()
    assert plugin._layer.corners is None and plugin._steady.pending  # one look isn't enough to outline it
    _wait_for(qapp, lambda: plugin.process_frame(frame) is frame and host.notified)
    assert QGuiApplication.clipboard().text() == "hunter22" and plugin._copy_btn.isVisibleTo(plugin._status.parentWidget())
    assert host.notified == [("QR code copied", "Copied the password for Telescope", False)]
    assert plugin._layer.corners is not None and host.refreshed
    QGuiApplication.clipboard().setText("something else")
    plugin._looked_at = 0
    plugin.process_frame(frame)
    _wait_for(qapp, lambda: not plugin._busy.is_set())
    qapp.processEvents()
    assert QGuiApplication.clipboard().text() == "something else"  # still in view: not copied over and over


def _read(plugin, text):
    for _ in range(3):
        plugin._on_found(text, _QUAD)


def test_a_stray_quad_is_never_outlined(reader):
    plugin, host = reader
    plugin.set_config({"on": True})
    for _ in range(5):
        plugin._on_found("", _QUAD)
        plugin._on_found("", None)
    assert plugin._layer.corners is None and host.notified == []


def test_notifications_can_be_switched_off(reader, qapp):
    plugin, host = reader
    plugin.set_config({"on": True, "notify": False})
    _read(plugin, "https://example.com")
    _wait_for(qapp, lambda: plugin._status.text().find("example.com") >= 0, 2)
    assert host.notified == [] and plugin._status.text().find("example.com") >= 0
    assert plugin.get_config() == {"on": True, "notify": False}


def test_switching_off_takes_the_outline_off_the_preview(reader):
    plugin, host = reader
    plugin.set_config({"on": True})
    plugin._switch.setChecked(False)
    assert host.overlays.layers() == []
    _read(plugin, "https://example.com")
    assert host.notified == []  # a result that comes in after switching off is dropped


class _DeafClipboard:
    """A clipboard that ignores a copy from the background, as Wayland does, until a click gives the window focus."""

    def __init__(self):
        self.held, self.focused = "old", False

    def setText(self, text):
        if self.focused:
            self.held = text

    def text(self):
        return self.held


def test_a_copy_wayland_ignores_says_to_click_copy(reader, qapp, monkeypatch):
    plugin, host = reader
    deaf = _DeafClipboard()
    monkeypatch.setattr(QGuiApplication, "clipboard", staticmethod(lambda: deaf))
    plugin.set_config({"on": True})
    _read(plugin, "https://example.com")
    _wait_for(qapp, lambda: host.notified)
    assert deaf.held == "old" and "Click Copy" in plugin._status.text()
    assert host.notified[-1][0] == "QR code found"
    deaf.focused = True
    plugin._copy_btn.click()
    assert deaf.held == "https://example.com" and plugin._status.text().find("example.com") >= 0


def test_loading_settings_switches_on_once(reader):
    plugin, host = reader
    host.saves = 0
    plugin.set_config({"on": True})
    assert host.saves == 1 and host.overlays.layers() == [plugin._layer]
