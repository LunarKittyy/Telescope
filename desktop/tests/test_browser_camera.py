"""Browser camera as a stream source: the picker entry, the host's start and stop, and the plugin's card."""

import time
from types import SimpleNamespace

import pytest
from PyQt6.QtCore import QCoreApplication
from PyQt6.QtWidgets import QWidget

import telescope.app as app_module
import telescope.plugins.browser_camera as browser_module
from telescope.browser_server import BrowserControl, BrowserFeed, BrowserReader, ServerStats
from telescope.plugin import EventBus
from telescope.plugins.browser_camera import (
    HINT_AFTER_S, REMEMBER_DAYS, SOURCE_ID, BrowserCameraPlugin, waiting_hint,
)
from telescope.widgets.common import create_card, dim_until_paired

from test_app import _Connection, _Plugin, _Signal, window  # noqa: F401 (the fixture)
from test_connection import _add, plugin_env  # noqa: F401 (the fixture)


# ── Phone picker ─────────────────────────────────────────────────────────────

def _picker(plugin):
    combo = plugin._phone_combo
    return [combo.itemText(i) for i in range(combo.count()) if combo.itemData(i)]


def test_sources_are_listed_after_the_phones_and_can_be_picked(plugin_env):
    plugin, host, _panel = plugin_env
    picked = []
    plugin._bus.source_selected.connect(picked.append)
    _add(plugin)
    plugin._bus.stream_sources_changed.emit([(SOURCE_ID, "Browser camera")])
    assert _picker(plugin) == ["Pixel", "Browser camera"]

    plugin._phone_combo.setCurrentIndex(plugin._phone_combo.findData(SOURCE_ID))
    assert plugin.selected_device == SOURCE_ID  # its settings are kept apart from the phone's
    assert host.switches[-1] == ("id-a", SOURCE_ID)
    assert picked[-1] == SOURCE_ID
    assert "no phone needed" in plugin._status_lbl.fullText()

    plugin._phone_combo.setCurrentIndex(plugin._phone_combo.findData("id-a"))
    assert picked[-1] == ""


def test_a_saved_source_selection_is_restored(plugin_env):
    plugin, _host, _panel = plugin_env
    plugin._bus.stream_sources_changed.emit([(SOURCE_ID, "Browser camera")])
    _add(plugin)
    cfg = plugin.get_config()
    cfg["selected_phone"] = SOURCE_ID
    plugin.set_config(cfg)
    assert plugin.selected_device == SOURCE_ID


def test_phone_only_cards_hide_while_a_source_is_picked(qapp):
    bus = EventBus()
    holder = QWidget()
    camera, mic = create_card(holder), create_card(holder)
    dim_until_paired(camera, bus, phone_only=True)
    dim_until_paired(mic, bus)
    assert not mic.isEnabled()  # nothing paired yet

    bus.source_selected.emit(SOURCE_ID)
    assert camera.isHidden()
    assert mic.isEnabled() and not mic.isHidden()  # the browser has a mic too

    bus.source_selected.emit("")
    assert not camera.isHidden()
    assert not mic.isEnabled()


# ── Host ─────────────────────────────────────────────────────────────────────

class _Source:
    id, name, url = SOURCE_ID, "Browser camera", "browser:"

    def __init__(self, ready=True):
        self.ready = ready
        self.ctrl = SimpleNamespace(close=lambda: None)
        self.prepared = 0

    def prepare(self, interactive):
        self.prepared += 1
        return self.ready

    def open_reader(self):
        return "reader"

    def control_client(self):
        return self.ctrl

    def fps(self):
        return 24


class _Worker:
    def __init__(self, **kwargs):
        self.kwargs = kwargs
        self.status, self.reconnected, self.vcam_opened = _Signal(), _Signal(), _Signal()
        self.auth = None

    def start(self):
        pass

    def request_stop(self):
        pass

    def wait(self, _ms):
        return True


def _source_window(window, monkeypatch, source):  # noqa: F811
    conn = _Connection(selected=SOURCE_ID)
    conn.ensure_virtual_camera = lambda interactive=True: True
    other = _Plugin("transforms")
    window.register_plugin(conn)
    window.register_plugin(other)
    window.add_stream_source(source)
    monkeypatch.setattr(app_module, "StreamWorker", _Worker)
    threads = []
    monkeypatch.setattr(app_module.threading, "Thread",
                        lambda target, args=(), daemon=False: SimpleNamespace(start=lambda: threads.append(target)))
    return conn, other, threads


def test_start_streams_from_the_source_without_waking_a_phone(window, monkeypatch):  # noqa: F811
    source = _Source()
    conn, other, threads = _source_window(window, monkeypatch, source)
    window._start()

    worker = window._worker
    assert conn.wakes == 0
    assert worker.kwargs["open_reader"]() == "reader"
    assert worker.kwargs["fps"] == 24 and worker.kwargs["width"] is None
    assert window._ctrl is source.ctrl
    assert other.started == [("browser:", source.ctrl)]
    assert threads == []  # no phone state to fetch

    worker.status.emit("reconnecting", "Stream dropped - reconnecting")
    assert window.state_fetches == []  # nowhere else to look for it

    window._stop()
    assert conn.remote_stops == 0
    assert window._worker is None


def test_a_source_that_isnt_ready_builds_nothing(window, monkeypatch):  # noqa: F811
    source = _Source(ready=False)
    _source_window(window, monkeypatch, source)
    failed = []
    window._bus.stream_start_failed.connect(lambda: failed.append(1))
    window._start()
    assert source.prepared == 1
    assert window._worker is None and failed == [1]


def test_sources_are_announced_on_the_bus(window):  # noqa: F811
    seen = []
    window._bus.stream_sources_changed.connect(seen.append)
    window.add_stream_source(_Source())
    assert seen == [[(SOURCE_ID, "Browser camera")]]


# ── The plugin ───────────────────────────────────────────────────────────────

class _Host:
    def __init__(self):
        self.sources, self.issues, self.starts, self.outputs = {}, {}, [], []
        self.streams = []  # ids, in start order
        self.starting = False
        self.saves = 0
        self.settings = set()  # ids with settings saved
        self.forgotten = []
        self.stopped = []  # stop_stream calls, waiting tiles included

    def add_stream_source(self, source):
        self.sources[source.id] = source

    def remove_stream_source(self, source_id):
        self.sources.pop(source_id, None)
        if source_id in self.streams:
            self.streams.remove(source_id)

    def stream_source(self, source_id):
        self.starts.append(source_id)
        self.streams.append(source_id)

    def stop_stream(self, source_id=None):
        self.stopped.append(source_id)
        if source_id in self.streams:
            self.streams.remove(source_id)

    def stream_output(self, source_id):
        return f"Camera {self.streams.index(source_id) + 1}" if source_id in self.streams else ""

    def stream_count(self):
        return len(self.streams)

    def has_device_settings(self, name):
        return name in self.settings

    def forget_device_settings(self, name):
        self.forgotten.append(name)
        self.settings.discard(name)

    def show_issue(self, key, issue):
        self.issues[key] = issue

    def clear_issue(self, key=None):
        self.issues.pop(key, None)

    def start_stream(self, interactive=True):
        pass

    def start_again(self, source_id=None, before=None):
        return lambda: None

    def is_streaming(self):
        return bool(self.streams)

    def is_streaming_from(self, source_id):
        return source_id in self.streams

    def is_starting(self):
        return self.starting

    def update_stream_output(self, **kwargs):
        self.outputs.append(kwargs)

    def schedule_save(self):
        self.saves += 1


class _Server:
    instances = []

    def __init__(self, hub, cert, key, on_change=None):
        self.hub, self.on_change = hub, on_change
        self.started = self.stopped = False
        self.token = "t1"
        self.port = 8767
        self.stats = ServerStats(8767, 8767)
        _Server.instances.append(self)

    def start(self):
        self.started = True

    def stop(self):
        self.stopped = True

    def url_for(self, host):
        return f"https://{host}:{self.port}/#{self.token}"

    def new_token(self):
        self.token = "t2"


@pytest.fixture
def browser(qapp, tmp_path):
    _Server.instances.clear()
    host, bus = _Host(), EventBus()
    addrs = [SimpleNamespace(ip="192.168.1.20"), SimpleNamespace(ip="100.64.0.3")]
    plugin = BrowserCameraPlugin(server_cls=_Server, cert_folder=tmp_path, addresses=lambda: addrs)
    plugin.setup(host, bus)
    holder = QWidget()
    card = plugin.create_panel()
    card.setParent(holder)
    QCoreApplication.processEvents()  # the card hides itself once it's placed
    yield plugin, host, bus, card
    plugin.shutdown()


def _arrive(plugin, bid, device=""):
    """A browser connects (and says what it runs on), as the server's thread would report it."""
    feed = plugin.hub.join(bid)
    gen = feed.connect()
    if device:
        feed.note_hello(gen, device, "", "h264")
    plugin._on_server_changed()
    return feed, gen


def _changed(host, bus):
    bus.streams_changed.emit(len(host.streams))


def test_offers_a_picker_entry_that_waits_for_a_browser(browser):
    plugin, host, bus, _card = browser
    launcher = host.sources[SOURCE_ID]
    assert launcher.name == "Browser camera" and launcher.redirect() is None
    bus.source_selected.emit(SOURCE_ID)
    assert not launcher.prepare(False)  # nothing to stream yet: the first browser to connect starts
    assert host.issues["start"].title == "Waiting for a browser"


def test_picking_it_runs_the_server_and_shows_the_code(browser):
    plugin, _host, bus, card = browser
    assert card.isHidden()

    bus.source_selected.emit(SOURCE_ID)
    server = _Server.instances[-1]
    assert server.started
    assert not card.isHidden()
    assert plugin._url_lbl.fullText() == "https://192.168.1.20:8767/#t1"
    assert plugin._qr is not None
    assert not plugin._addr_row.isHidden()  # two addresses to choose from

    plugin.new_link()
    assert plugin._url_lbl.fullText().endswith("#t2")

    bus.source_selected.emit("some-phone")
    assert server.stopped and card.isHidden()


def test_each_browser_streams_by_itself_to_a_camera_of_its_own(browser):
    plugin, host, bus, _card = browser
    bus.source_selected.emit(SOURCE_ID)
    pixel, _ = _arrive(plugin, "pixel-aaaaaaaa", "Chrome on Android")
    assert host.starts == ["browser:pixel-aaaaaaaa"]  # like opening the app on a phone
    assert host.sources["browser:pixel-aaaaaaaa"].name == "Chrome on Android"
    assert host.issues.get("start") is None
    _changed(host, bus)
    assert pixel.page_config()[1]["camera"] == "Camera 1"

    iphone, _ = _arrive(plugin, "iphone-bbbbbbbb", "Safari on iPhone")
    assert host.starts == ["browser:pixel-aaaaaaaa", "browser:iphone-bbbbbbbb"]
    _changed(host, bus)
    assert iphone.page_config()[1]["camera"] == "Camera 2"
    assert plugin._list_lbl.text() == "Chrome on Android: Camera 1\nSafari on iPhone: Camera 2"
    assert not plugin._list_row.isHidden()


def test_a_stopped_browser_waits_until_it_connects_again(browser):
    plugin, host, bus, _card = browser
    bus.source_selected.emit(SOURCE_ID)
    feed, gen = _arrive(plugin, "pixel-aaaaaaaa")
    host.stop_stream("browser:pixel-aaaaaaaa")  # the x on its tile
    _changed(host, bus)
    assert host.starts == ["browser:pixel-aaaaaaaa"]
    assert host.sources[SOURCE_ID].redirect() == "browser:pixel-aaaaaaaa"  # Start streams it again

    feed.disconnect(gen)
    plugin._on_server_changed()
    assert "browser:pixel-aaaaaaaa" not in host.sources  # gone from the picker
    _arrive(plugin, "pixel-aaaaaaaa")
    assert host.starts == ["browser:pixel-aaaaaaaa"] * 2


def test_a_streaming_browser_that_drops_stays_until_it_comes_back(browser):
    plugin, host, bus, _card = browser
    bus.source_selected.emit(SOURCE_ID)
    feed, gen = _arrive(plugin, "pixel-aaaaaaaa")
    feed.disconnect(gen)
    plugin._on_server_changed()
    assert "browser:pixel-aaaaaaaa" in host.sources and plugin.hub.feed("pixel-aaaaaaaa") is feed
    _arrive(plugin, "pixel-aaaaaaaa")
    assert host.starts == ["browser:pixel-aaaaaaaa"]  # still streaming: nothing to start


def test_no_room_means_no_feed(browser):
    plugin, host, bus, _card = browser
    bus.source_selected.emit(SOURCE_ID)
    host.streams = ["phone-1", "phone-2", "phone-3"]
    _changed(host, bus)
    assert plugin.hub.join("pixel-aaaaaaaa") is not None  # the last camera
    assert plugin.hub.join("iphone-bbbbbbbb") is None


def test_the_server_stays_up_while_a_browser_streams_behind_another(browser):
    plugin, host, bus, card = browser
    bus.source_selected.emit(SOURCE_ID)
    server = _Server.instances[-1]
    _arrive(plugin, "pixel-aaaaaaaa")

    bus.source_selected.emit("some-phone")  # the panels moved to a phone's tile
    assert not server.stopped and card.isHidden()

    host.streams.clear()
    _changed(host, bus)  # its stream stopped
    assert server.stopped and "browser:pixel-aaaaaaaa" not in host.sources


def test_adding_a_browser_next_to_a_phone_shows_the_code_until_one_joins(browser):
    plugin, host, bus, card = browser
    host.streams.append("some-phone")
    bus.source_selected.emit(SOURCE_ID)  # + put the panels on it: its own page, with the code
    assert not host.sources[SOURCE_ID].prepare(True)
    server = _Server.instances[-1]
    assert server.started and not card.isHidden() and plugin._qr is not None
    bus.source_selected.emit("some-phone")  # the phone's tile clicked: its settings, without the code
    _changed(host, bus)
    assert not server.stopped and card.isHidden()  # still waiting

    assert launcher_waits(host) and "start" not in host.issues  # its tile says so, not a banner

    _arrive(plugin, "pixel-aaaaaaaa")
    assert host.starts == ["browser:pixel-aaaaaaaa"] and card.isHidden()
    assert not launcher_waits(host) and host.stopped == [SOURCE_ID]  # the waiting tile went


def launcher_waits(host):
    return host.sources[SOURCE_ID].waiting()


def test_closing_the_waiting_tile_or_the_banner_hides_the_code(browser):
    plugin, host, bus, card = browser
    host.streams.append("some-phone")
    launcher = host.sources[SOURCE_ID]
    launcher.prepare(True)
    launcher.cancel_wait()  # the tile's X
    assert card.isHidden() and _Server.instances[-1].stopped and not launcher.waiting()

    host.streams.clear()
    launcher.prepare(True)
    host.issues["start"].actions[0].callback()  # Cancel, with nothing else streaming
    assert card.isHidden() and _Server.instances[-1].stopped and "start" not in host.issues

    launcher.prepare(True)
    host.issues["start"].on_dismiss()  # the banner's X
    assert card.isHidden() and _Server.instances[-1].stopped


def test_the_card_shows_the_browser_the_panels_show(browser):
    plugin, host, bus, card = browser
    bus.source_selected.emit(SOURCE_ID)
    feed, gen = _arrive(plugin, "pixel-aaaaaaaa")
    feed.note_hello(gen, "Firefox on Android", "Not allowed", "jpeg", "No hardware H.264 encoder")
    plugin._on_server_changed()
    bus.source_selected.emit("browser:pixel-aaaaaaaa")
    assert not card.isHidden()
    assert plugin._status_lbl.fullText() == "● Firefox on Android, JPEG"
    assert "No hardware" in plugin._status_lbl.toolTip()
    assert plugin._mic_lbl.text() == "No microphone: Not allowed"


def test_capture_settings_go_to_every_page_and_stream(browser):
    plugin, host, bus, _card = browser
    bus.source_selected.emit(SOURCE_ID)
    feed, _ = _arrive(plugin, "pixel-aaaaaaaa")
    _arrive(plugin, "iphone-bbbbbbbb")
    bus.source_selected.emit("browser:pixel-aaaaaaaa")
    plugin._fps_combo.setCurrentIndex(plugin._fps_combo.findData(15))
    plugin._size_combo.setCurrentIndex(plugin._size_combo.findData("1080p"))
    assert host.outputs == [{"fps": 15, "source_id": "browser:pixel-aaaaaaaa"},
                            {"fps": 15, "source_id": "browser:iphone-bbbbbbbb"}]
    _seq, cfg = feed.page_config()
    assert (cfg["width"], cfg["height"], cfg["fps"]) == (1920, 1080, 15)
    later, _ = _arrive(plugin, "iphone-bbbbbbbb")
    assert later.page_config()[1]["fps"] == 15
    cfg = plugin.get_config()
    assert (cfg["size"], cfg["fps"], cfg["address"]) == ("1080p", 15, "192.168.1.20")


def test_config_falls_back_on_bad_values(browser):
    plugin, *_ = browser
    plugin.set_config({"size": "8k", "fps": 120, "address": 5, "known": {"x": {"name": 3}}})
    assert plugin.get_config() == {"size": "720p", "fps": 30, "address": "", "known": {}}


def test_a_browser_is_remembered_once_it_has_settings_and_can_be_forgotten(browser, monkeypatch):
    plugin, host, bus, _card = browser
    lists = []
    bus.remembered_sources.connect(lists.append)
    bus.source_selected.emit(SOURCE_ID)
    _arrive(plugin, "pixel-aaaaaaaa", "Chrome on Android")
    _arrive(plugin, "guest-cccccccc", "Safari on iPhone")
    host.settings.add("browser:pixel-aaaaaaaa")  # zoomed in on it; the guest changed nothing
    _changed(host, bus)
    assert [(sid, name) for sid, name, _detail in lists[-1]] == [("browser:pixel-aaaaaaaa", "Chrome on Android")]

    plugin2 = BrowserCameraPlugin(server_cls=_Server)
    plugin2.setup(host, EventBus())
    plugin2.set_config(plugin.get_config())
    plugin2._tidy_known()
    assert set(plugin2._known) == {"pixel-aaaaaaaa"}  # the guest left nothing behind

    old = time.time() - (REMEMBER_DAYS + 1) * 86400
    plugin2._known["pixel-aaaaaaaa"]["seen"] = old
    plugin2._tidy_known()
    assert plugin2._known == {} and host.forgotten == ["browser:pixel-aaaaaaaa"]

    host.settings.add("browser:pixel-aaaaaaaa")
    bus.forget_source_requested.emit("browser:pixel-aaaaaaaa")
    assert "browser:pixel-aaaaaaaa" not in host.streams and lists[-1] == []
    assert host.forgotten == ["browser:pixel-aaaaaaaa"] * 2


def test_without_cryptography_it_says_so_and_start_fails(browser, monkeypatch):
    plugin, host, bus, _card = browser
    monkeypatch.setattr(browser_module, "certificate_available", lambda: False)
    bus.source_selected.emit(SOURCE_ID)
    assert "cryptography" in plugin._status_lbl.fullText()
    assert not plugin.prepare()
    assert "cryptography" in host.issues["start"].text


def test_diagnostics_say_how_far_browsers_got_but_never_name_the_device(browser):
    plugin, _host, bus, _card = browser
    bus.source_selected.emit(SOURCE_ID)
    stats = _Server.instances[-1].stats
    stats.note("connections")
    stats.note("tls_failed")
    feed, gen = _arrive(plugin, "pixel-aaaaaaaa")
    feed.note_hello(gen, "Safari on iPhone", "", "jpeg", "No hardware H.264 encoder")
    diag = plugin.diagnostics()
    assert diag["Browser camera"] == "1 browser connected, sending jpeg (No hardware H.264 encoder)"
    assert diag["Browser camera port"] == "8767"
    assert diag["Browser camera reached"].startswith("1 connections, 1 certificate refusals, 0 page loads")
    assert "iPhone" not in str(diag)

    stats.port, stats.fell_back = 41234, True
    assert plugin.diagnostics()["Browser camera port"] == "41234 (8767 was taken)"


def test_waiting_hint_points_at_the_step_that_failed():
    stats = ServerStats(8767, 8767)
    t0 = stats.started
    late = t0 + HINT_AFTER_S + 1  # exactly HINT_AFTER_S can come out a hair short in floats on a long-running clock
    assert waiting_hint(stats, False, t0 + 5) == ""  # still scanning, probably
    assert "firewall allows port 8767" in waiting_hint(stats, False, late)
    assert waiting_hint(stats, True, late) == ""
    stats.note("connections")
    assert waiting_hint(stats, False, late) == ""
    stats.note("tls_failed")
    assert "certificate" in waiting_hint(stats, False, t0 + 1)
    stats.note("pages")
    assert waiting_hint(stats, False, late) == ""  # on the page now


def test_card_shows_the_firewall_hint_once_the_wait_is_long(browser, monkeypatch):
    plugin, _host, bus, _card = browser
    bus.source_selected.emit(SOURCE_ID)
    assert plugin._hint_row.isHidden()
    assert plugin._hint_timer.isActive()
    _Server.instances[-1].stats.started -= HINT_AFTER_S
    plugin._render()  # what the timer does
    assert not plugin._hint_row.isHidden()
    assert "port 8767" in plugin._hint_lbl.text()


def test_feed_drops_frames_from_a_replaced_browser():
    feed = BrowserFeed()
    old = feed.connect()
    new = feed.connect()
    feed.put_frame(old, b"old")
    assert feed.next_frame(new, 0, 0.01) is None
    feed.put_frame(new, b"new")
    assert feed.next_frame(new, 0, 0.01) == (1, "jpeg", b"new")


def test_falling_behind_points_at_its_own_resolution_and_frame_rate(browser):
    plugin, host, bus, _card = browser
    bus.stream_behind.emit(True)
    assert "behind" not in host.issues  # a phone is picked: Stream output says what helps
    bus.source_selected.emit(SOURCE_ID)
    bus.stream_behind.emit(True)
    issue = host.issues["behind"]
    assert issue.title == "Can't keep up" and "Browser camera card" in issue.text and issue.kind == "warn"
    plugin._set_capture(size="480p", fps=15)
    bus.stream_behind.emit(True)
    assert host.issues["behind"].text == "The device's browser can't send any faster."
    bus.stream_behind.emit(False)
    assert "behind" not in host.issues
