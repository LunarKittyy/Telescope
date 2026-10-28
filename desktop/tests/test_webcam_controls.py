"""Webcam picture settings: reading them from V4L2 and showing, setting and keeping them on the Webcam card."""

import sys
from types import SimpleNamespace

import pytest
from PyQt6.QtCore import QCoreApplication
from PyQt6.QtWidgets import QWidget

from telescope import webcam_controls as wc
from telescope.plugin import EventBus
from telescope.plugins.webcam import SOURCE_PREFIX, WebcamPlugin, shown_value, shutter_text
from telescope.webcam_controls import Control
from telescope.webcams import Device, Mode

from test_webcam_plugin import _Host

linux_only = pytest.mark.skipif(sys.platform == "win32", reason="V4L2 is Linux's")
CAM = Device("cam", "HD Webcam", "/dev/video0", (Mode("MJPG", 1280, 720, (30.0,)),))


class _FakeV4L2:
    """Answers the control ioctls the way a webcam with brightness, auto exposure and anti-flicker does."""

    def __init__(self):
        # cid: (type, min, max, step, default, flags, menu)
        self.ctrls = {0x00980900: [1, 0, 255, 1, 128, 0, ()], 0x009A0901: [3, 0, 3, 1, 3, 0, (1, 3)],
                      0x009A0902: [1, 3, 2047, 1, 250, 0x10, ()], 0x00980918: [3, 0, 2, 1, 2, 0, (0, 1, 2)],
                      0x0098091C: [1, 0, 1, 1, 0, 0x4, ()]}  # read-only: left out
        self.values = {0x00980900: 140, 0x009A0901: 3, 0x009A0902: 333, 0x00980918: 2, 0x0098091C: 0}
        self.sets = []

    def __call__(self, fd, request, packed):
        if request == wc.VIDIOC_QUERYCTRL:
            cid = wc._QUERYCTRL.unpack(packed)[0]
            if cid not in self.ctrls:
                return None
            kind, lo, hi, step, default, flags, _menu = self.ctrls[cid]
            return wc._QUERYCTRL.pack(cid, kind, b"x", lo, hi, step, default, flags, 0, 0)
        if request == wc.VIDIOC_QUERYMENU:
            cid, index, _name, _r = wc._QUERYMENU.unpack(packed)
            return packed if index in self.ctrls[cid][6] else None
        cid, value = wc._CONTROL.unpack(packed)
        if request == wc.VIDIOC_G_CTRL:
            return wc._CONTROL.pack(cid, self.values[cid])
        self.sets.append((cid, value))
        self.values[cid] = value
        return packed


@linux_only
def test_v4l2_controls_are_read_under_our_names_and_auto_exposure_is_a_switch():
    fake = _FakeV4L2()
    got = {c.name: c for c in wc.LinuxControls(__file__, ioctl=fake).list()}
    assert set(got) == {"brightness", "exposure_auto", "exposure_time", "anti_flicker"}
    assert got["brightness"] == Control("brightness", "int", 0, 255, 1, 128, 140)
    assert (got["exposure_auto"].kind, got["exposure_auto"].value, got["exposure_auto"].default) == ("bool", 1, 1)
    assert got["exposure_time"].inactive and got["exposure_time"].unit == "100us"
    assert got["anti_flicker"].menu == (0, 1, 2)


@linux_only
def test_setting_auto_exposure_writes_the_cameras_own_auto_mode_or_manual():
    fake = _FakeV4L2()
    controls = wc.LinuxControls(__file__, ioctl=fake)
    controls.list()
    assert controls.set("exposure_auto", 0) and fake.values[0x009A0901] == 1
    assert controls.set("exposure_auto", 1) and fake.values[0x009A0901] == 3
    assert not controls.set("not_a_control", 1)


def test_mains_frequency_comes_from_where_the_computer_is():
    assert wc.mains_hz("US") == 60 and wc.mains_hz("se") == 50 and wc.mains_hz("BR") == 60


def test_auto_switches_are_applied_before_the_values_they_grey_out():
    assert wc.apply_order(["exposure_time", "gain", "exposure_auto"])[0] == "exposure_auto"


def test_values_are_kept_to_the_cameras_range_and_step():
    focus = Control("focus", "int", 0, 250, 5, 0, 0)
    assert (focus.clamp(-3), focus.clamp(12), focus.clamp(999)) == (0, 10, 250)


def test_exposure_shows_as_a_shutter_speed():
    assert shutter_text(1 / 60) == "1/60" and shutter_text(0.5) == "0.5 s" and shutter_text(2) == "2 s"
    assert shown_value(Control("exposure_time", "int", 3, 2047, 1, 166, 166, unit="100us"), 166) == "1/60"
    assert shown_value(Control("exposure_time", "int", -13, -1, 1, -6, -6, unit="log2s"), -6) == "1/64"
    assert shown_value(Control("pan", "int", -36000, 36000, 3600, 0, 0, unit="arcsec"), 7200) == "+2°"
    assert shown_value(Control("zoom", "int", 100, 500, 1, 100, 200, unit="zoom"), 200) == "2.0×"


class _Backend:
    """A webcam's settings as the card sees them."""

    def __init__(self):
        self.values = {"exposure_auto": 1, "exposure_time": 250, "gain": 0, "anti_flicker": 2, "frame_rate_drop": 1}
        self.sets = []

    def list(self):
        auto = self.values["exposure_auto"]
        return [Control("exposure_auto", "bool", 0, 1, 1, 1, auto),
                Control("exposure_time", "int", 3, 2047, 1, 250, self.values["exposure_time"], inactive=bool(auto),
                        unit="100us"),
                Control("gain", "int", 0, 255, 1, 0, self.values["gain"]),
                Control("anti_flicker", "menu", 0, 2, 1, 2, self.values["anti_flicker"], (0, 1, 2)),
                Control("frame_rate_drop", "bool", 0, 1, 1, 0, self.values["frame_rate_drop"])]

    def set(self, name, value):
        if name == "exposure_time" and self.values["exposure_auto"]:
            return False  # refused while auto exposure is on, as real cameras do
        self.sets.append((name, value))
        self.values[name] = value
        return True


@pytest.fixture
def card(qapp, monkeypatch):
    monkeypatch.setattr(wc, "mains_hz", lambda territory="": 50)
    backend, host, bus = _Backend(), _Host(), EventBus()
    plugin = WebcamPlugin(scanner=lambda: [CAM], controls=lambda device: backend if device is not None else None)
    plugin.setup(host, bus)
    holder = QWidget()
    panel = plugin.create_panel()
    panel.setParent(holder)
    QCoreApplication.processEvents()
    bus.source_selected.emit(SOURCE_PREFIX + CAM.key)
    yield SimpleNamespace(plugin=plugin, backend=backend, host=host)
    plugin.shutdown()


def test_a_webcam_is_only_written_to_once_its_stream_opens_and_gets_anti_flicker_for_the_local_mains(card):
    assert card.backend.sets == []  # plugged in and picked, but not streaming: left as it is
    card.host.streams.append(SOURCE_PREFIX + "cam")
    card.plugin.signals.reported.emit("cam", "ok", "1280×720, MJPEG, 30 fps")
    assert card.backend.sets == [("anti_flicker", 1)]


def test_the_card_shows_only_what_the_camera_offers_and_greys_manual_ones_under_auto(card):
    rows = card.plugin._control_rows
    assert set(rows) == {"exposure_auto", "exposure_time", "gain", "anti_flicker", "frame_rate_drop"}
    assert not rows["exposure_time"][0].isEnabled()
    card.plugin._set_control("exposure_auto", 0)
    assert rows["exposure_time"][0].isEnabled()


def test_picked_settings_are_kept_per_camera_and_put_back_when_it_opens_again(card):
    card.plugin._set_control("exposure_auto", 0)
    card.plugin._set_control("exposure_time", 166)
    card.plugin._set_control("gain", 40)
    assert card.plugin.get_config()["cameras"]["cam"]["controls"] == {"exposure_auto": 0, "exposure_time": 166,
                                                                      "gain": 40}
    card.backend.values.update(exposure_auto=1, exposure_time=250, gain=0)  # unplugged and back: its defaults
    card.backend.sets.clear()
    card.host.streams.append(SOURCE_PREFIX + "cam")
    card.plugin.signals.reported.emit("cam", "ok", "1280×720, MJPEG, 30 fps")
    assert card.backend.sets[0] == ("exposure_auto", 0)
    assert ("exposure_time", 166) in card.backend.sets and ("gain", 40) in card.backend.sets


def test_reset_puts_the_cameras_defaults_back_and_forgets_what_was_picked(card):
    card.plugin._set_control("exposure_auto", 0)
    card.plugin._set_control("gain", 40)
    card.plugin.reset_controls()
    assert card.backend.values["exposure_auto"] == 1 and card.backend.values["gain"] == 0
    assert card.backend.values["anti_flicker"] == 1  # the local mains, not the camera's 60 Hz
    assert card.backend.values["frame_rate_drop"] == 0
    assert "controls" not in card.plugin.get_config()["cameras"]["cam"]


def test_saved_settings_that_arent_numbers_are_ignored(card):
    card.plugin.set_config({"cameras": {"cam": {"controls": {"gain": "lots", "zoom": 2, 3: 4}}}})
    assert card.plugin.get_config()["cameras"]["cam"]["controls"] == {"zoom": 2}


def test_a_camera_whose_settings_cant_be_reached_shows_none(qapp):
    plugin = WebcamPlugin(scanner=lambda: [CAM], controls=lambda device: None)
    plugin.setup(_Host(), EventBus())
    holder = QWidget()
    plugin.create_panel().setParent(holder)
    QCoreApplication.processEvents()
    plugin._bus.source_selected.emit(SOURCE_PREFIX + CAM.key)
    assert plugin._control_rows == {} and plugin._controls_box.isHidden() and plugin._reset_btn.isHidden()
    plugin.shutdown()


def test_a_replugged_webcam_gets_a_fresh_handle_on_its_settings(qapp):
    made, plugged = [], [CAM]

    class Held(_Backend):
        closed = False

        def close(self):
            self.closed = True

    def controls(device):
        made.append(Held())
        return made[-1]
    plugin = WebcamPlugin(scanner=lambda: list(plugged), controls=controls)
    host = _Host()
    plugin.setup(host, EventBus())
    plugin.rescan()
    plugin._read_controls(CAM.key)
    host.streams.append(SOURCE_PREFIX + CAM.key)  # streaming, so it keeps its place while unplugged
    plugged.clear()
    plugin.rescan()
    assert made[0].closed
    plugged.append(CAM)
    plugin.rescan()
    plugin._read_controls(CAM.key)
    assert len(made) == 2 and not made[1].closed
    plugin.shutdown()
    assert made[1].closed
