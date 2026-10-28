"""The Webcam plugin: webcams in the picker, plugging them in and out, their card and their settings."""

from types import SimpleNamespace

import pytest
from PyQt6.QtCore import QCoreApplication
from PyQt6.QtWidgets import QWidget

from telescope import webcams
from telescope.plugin import EventBus
from telescope.plugins.webcam import SOURCE_PREFIX, WebcamPlugin
from telescope.webcams import Device, Mode

MODES = (Mode("YUYV", 640, 480, (30.0,)), Mode("YUYV", 1920, 1080, (5.0,)),
         Mode("MJPG", 1280, 720, (30.0, 60.0)), Mode("MJPG", 1920, 1080, (30.0,)))
C920 = Device("usb-c920", "HD Pro Webcam C920", "/dev/video0", MODES)
BRIO = Device("usb-brio", "Brio", "/dev/video2", MODES)


class _Host:
    def __init__(self):
        self.sources, self.issues = {}, {}
        self.streams = []
        self.saves = 0
        self.reconnects, self.removed = [], []

    def add_stream_source(self, source):
        self.sources[source.id] = source

    def remove_stream_source(self, source_id):
        self.removed.append(source_id)
        self.sources.pop(source_id, None)
        if source_id in self.streams:
            self.streams.remove(source_id)

    def is_streaming_from(self, source_id):
        return source_id in self.streams

    def reconnect_stream(self, source_id=None):
        self.reconnects.append(source_id)

    def show_issue(self, key, issue):
        self.issues[key] = issue

    def clear_issue(self, key=None, issue=None):
        self.issues.pop(key, None)

    def schedule_save(self):
        self.saves += 1


@pytest.fixture
def cams(qapp):
    host, bus = _Host(), EventBus()
    plugged = [C920]
    plugin = WebcamPlugin(scanner=lambda: list(plugged))
    plugin.setup(host, bus)
    holder = QWidget()
    card = plugin.create_panel()
    card.setParent(holder)
    QCoreApplication.processEvents()  # the first scan, and the card hides itself once it's placed
    yield SimpleNamespace(plugin=plugin, host=host, bus=bus, card=card, plugged=plugged)
    plugin.shutdown()


def _pick(env, device):
    env.bus.source_selected.emit(SOURCE_PREFIX + device.key if device else "")


def test_webcams_are_listed_by_name(cams):
    assert {sid: s.name for sid, s in cams.host.sources.items()} == {"webcam:usb-c920": "HD Pro Webcam C920"}


def test_a_webcam_plugged_in_joins_the_picker_and_one_pulled_out_leaves(cams):
    cams.plugged.append(BRIO)
    cams.plugin.rescan()
    assert "webcam:usb-brio" in cams.host.sources
    cams.plugged.remove(BRIO)
    cams.plugin.rescan()
    assert "webcam:usb-brio" not in cams.host.sources
    assert cams.host.removed == ["webcam:usb-brio"]


def test_a_renamed_webcam_is_offered_again_under_its_new_name(cams):
    cams.plugged.append(Device("usb-c920-2", "HD Pro Webcam C920 (2)", "/dev/video4", MODES))
    cams.plugin.rescan()
    assert cams.host.sources["webcam:usb-c920-2"].name == "HD Pro Webcam C920 (2)"


def test_a_streaming_webcam_that_is_unplugged_waits_for_it_and_carries_on(cams):
    src = cams.host.sources["webcam:usb-c920"]
    cams.host.streams.append(src.id)
    _pick(cams, C920)
    cams.plugged.clear()
    cams.plugin.rescan()
    assert src.id in cams.host.sources  # keeps its place while it streams
    assert cams.plugin._status_lbl.text() == "Not plugged in"
    assert src.open_reader().open() is False  # the stream's next try finds nothing

    cams.plugged.append(C920)
    cams.plugin.rescan()
    assert cams.plugin.device("usb-c920") is C920
    assert "usb-c920" not in cams.plugin._states


def test_an_unplugged_webcam_leaves_once_its_stream_stops(cams):
    src = cams.host.sources["webcam:usb-c920"]
    cams.host.streams.append(src.id)
    cams.plugged.clear()
    cams.plugin.rescan()
    cams.host.streams.clear()
    cams.bus.streams_changed.emit(0)
    assert src.id not in cams.host.sources


def test_start_on_a_webcam_unplugged_since_it_was_picked_says_so(cams):
    src = cams.host.sources["webcam:usb-c920"]
    assert src.prepare(True)
    cams.plugin._devices = {}
    assert not src.prepare(True)
    assert "isn't plugged in" in cams.host.issues["start"].title


def test_a_busy_webcam_gets_a_banner_naming_the_app_until_it_opens(cams):
    src = cams.host.sources["webcam:usb-c920"]
    cams.host.streams.append(src.id)
    _pick(cams, C920)
    cams.plugin.signals.reported.emit("usb-c920", "busy", "zoom")
    issue = cams.host.issues["webcam:usb-c920"]
    assert "HD Pro Webcam C920" in issue.title and "zoom" in issue.text
    assert cams.plugin._status_lbl.text() == "In use by zoom"

    cams.plugin.signals.reported.emit("usb-c920", "ok", "1280×720, MJPEG, 30 fps")
    assert "webcam:usb-c920" not in cams.host.issues
    assert cams.plugin._status_lbl.text() == "1280×720, MJPEG, 30 fps"


def test_a_busy_webcam_without_a_known_app_still_says_what_to_do(cams):
    cams.host.streams.append("webcam:usb-c920")
    cams.plugin.signals.reported.emit("usb-c920", "busy", "")
    assert "Another app" in cams.host.issues["webcam:usb-c920"].text


def test_the_card_shows_only_while_a_webcam_is_picked(cams):
    assert cams.card.isHidden()
    _pick(cams, C920)
    assert not cams.card.isHidden()
    _pick(cams, None)
    assert cams.card.isHidden()


def test_the_card_offers_the_cameras_sizes_and_the_rates_at_each(cams):
    _pick(cams, C920)
    combo = cams.plugin._size_combo
    assert [combo.itemText(i) for i in range(combo.count())] == ["Auto", "1920×1080", "1280×720", "640×480"]
    fps = cams.plugin._fps_combo
    assert [fps.itemData(i) for i in range(fps.count())] == [60, 30, 5]
    combo.setCurrentIndex(combo.findText("640×480"))
    assert [fps.itemData(i) for i in range(fps.count())] == [30]


def test_settings_are_kept_per_camera_and_used_for_the_capture(cams):
    cams.plugged.append(BRIO)
    cams.plugin.rescan()
    _pick(cams, C920)
    cams.plugin._size_combo.setCurrentIndex(cams.plugin._size_combo.findText("1280×720"))
    cams.plugin._fps_combo.setCurrentIndex(cams.plugin._fps_combo.findData(60))
    assert cams.plugin.choice(C920) == webcams.Choice("MJPG", 1280, 720, 60.0)
    assert cams.host.sources["webcam:usb-c920"].fps() == 60
    assert cams.plugin.choice(BRIO) == webcams.Choice("MJPG", 1920, 1080, 30.0)  # Auto, 30 fps
    assert cams.host.saves >= 2

    cfg = cams.plugin.get_config()
    other = WebcamPlugin(scanner=lambda: [C920])
    other.setup(_Host(), EventBus())
    other.set_config(cfg)
    assert other.choice(C920) == webcams.Choice("MJPG", 1280, 720, 60.0)
    other.shutdown()


def test_changing_a_streaming_webcams_settings_reopens_it(cams):
    cams.host.streams.append("webcam:usb-c920")
    _pick(cams, C920)
    cams.plugin._size_combo.setCurrentIndex(cams.plugin._size_combo.findText("640×480"))
    assert cams.host.reconnects == ["webcam:usb-c920"]


def test_a_saved_size_the_camera_no_longer_offers_stays_shown(cams):
    cams.plugin.set_config({"cameras": {"usb-c920": {"size": [800, 600], "fps": 30}}})
    _pick(cams, C920)
    assert cams.plugin._size_combo.currentText() == "800×600"


def test_bad_config_is_ignored(cams):
    cams.plugin.set_config({"cameras": {"usb-c920": {"size": "big", "fps": -3}, 5: {}, "x": "y"}})
    assert cams.plugin.wanted("usb-c920") == (None, 30)
    cams.plugin.set_config({"cameras": []})
    assert cams.plugin.get_config() == {"cameras": {}}


def test_a_camera_that_lists_no_modes_gets_what_was_picked():
    plugin = WebcamPlugin(scanner=lambda: [])
    plugin.setup(_Host(), EventBus())
    bare = Device("bare", "Capture", 0)
    assert plugin.choice(bare) == webcams.Choice("RAW", 1280, 720, 30.0)
    plugin.shutdown()


def test_the_source_reads_through_the_plugin(cams, monkeypatch):
    src = cams.host.sources["webcam:usb-c920"]
    reader = src.open_reader()
    assert isinstance(reader, webcams.WebcamReader)
    assert src.control_client().audio is False
    assert src.auto_canvas is None
    assert "plugged in" in src.lost_hint


def test_a_failing_scan_keeps_what_was_found(cams):
    cams.plugin._scan = lambda: 1 / 0
    cams.plugin.rescan()
    assert "webcam:usb-c920" in cams.host.sources


def test_diagnostics_list_the_webcams_and_their_streams(cams):
    cams.plugin.signals.reported.emit("usb-c920", "ok", "1280×720, MJPEG, 30 fps")
    diag = cams.plugin.diagnostics()
    assert diag["Webcams"] == "HD Pro Webcam C920 (4 modes)"
    assert "1280×720" in diag["Webcam streams"]
    cams.plugged.clear()
    cams.plugin.rescan()
    assert cams.plugin.diagnostics() == {"Webcams": "none found"}
