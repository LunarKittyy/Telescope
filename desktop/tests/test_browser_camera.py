"""Browser camera as a stream source: the picker entry, the host's start and stop, and the plugin's card."""

from types import SimpleNamespace

import pytest
from PyQt6.QtCore import QCoreApplication
from PyQt6.QtWidgets import QWidget

import telescope.app as app_module
import telescope.plugins.browser_camera as browser_module
from telescope.browser_server import BrowserControl, BrowserFeed, BrowserReader, ServerStats
from telescope.plugin import EventBus
from telescope.plugins.browser_camera import HINT_AFTER_S, SOURCE_ID, BrowserCameraPlugin, waiting_hint
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
    assert worker.kwargs["open_reader"] == source.open_reader
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
        self.sources, self.issues, self.starts, self.outputs = [], {}, [], []
        self.streaming = self.starting = False
        self.saves = 0

    def add_stream_source(self, source):
        self.sources.append(source)

    def show_issue(self, key, issue):
        self.issues[key] = issue

    def start_stream(self, interactive=True):
        self.starts.append(interactive)

    def is_streaming(self):
        return self.streaming

    def is_starting(self):
        return self.starting

    def update_stream_output(self, **kwargs):
        self.outputs.append(kwargs)

    def schedule_save(self):
        self.saves += 1


class _Server:
    instances = []

    def __init__(self, feed, cert, key, on_change=None):
        self.feed, self.on_change = feed, on_change
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


def test_offers_a_source_that_reads_the_browser(browser):
    plugin, host, _bus, _card = browser
    source = host.sources[0]
    assert source.id == SOURCE_ID
    assert isinstance(source.open_reader(), BrowserReader)
    ctrl = source.control_client()
    assert isinstance(ctrl, BrowserControl) and ctrl.get_state() is None
    assert source.fps() == 30


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


def test_a_browser_connecting_starts_the_stream_once(browser):
    plugin, host, bus, _card = browser
    bus.source_selected.emit(SOURCE_ID)
    gen = plugin.feed.connect()
    plugin._on_server_changed()
    assert host.starts == [False]  # nobody at the keyboard: no password prompts
    assert plugin._status_lbl.fullText().startswith("●")

    host.streaming = True
    plugin._on_server_changed()  # a hello from the same browser
    assert host.starts == [False]

    plugin.feed.disconnect(gen)
    plugin._on_server_changed()
    host.streaming = False  # the user stopped it meanwhile
    plugin.feed.connect()
    plugin._on_server_changed()
    assert host.starts == [False, False]


def test_capture_settings_go_to_the_page_and_the_virtual_camera(browser):
    plugin, host, bus, _card = browser
    bus.source_selected.emit(SOURCE_ID)
    host.streaming = True
    plugin._fps_combo.setCurrentIndex(plugin._fps_combo.findData(15))
    plugin._size_combo.setCurrentIndex(plugin._size_combo.findData("1080p"))
    assert host.outputs == [{"fps": 15}]
    _seq, cfg = plugin.feed.page_config()
    assert cfg == {"width": 1920, "height": 1080, "fps": 15, "audio": False, "h264": plugin.feed.h264}
    assert plugin.get_config() == {"size": "1080p", "fps": 15, "address": "192.168.1.20"}


def test_config_falls_back_on_bad_values(browser):
    plugin, *_ = browser
    plugin.set_config({"size": "8k", "fps": 120, "address": 5})
    assert plugin.get_config() == {"size": "720p", "fps": 30, "address": ""}


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
    plugin.feed.connect()
    plugin.feed.note_hello(plugin.feed._gen, "Safari on iPhone", "", "jpeg", "No hardware H.264 encoder")
    diag = plugin.diagnostics()
    assert diag["Browser camera"] == "browser connected, sending jpeg (No hardware H.264 encoder)"
    assert diag["Browser camera port"] == "8767"
    assert diag["Browser camera reached"].startswith("1 connections, 1 certificate refusals, 0 page loads")
    assert "iPhone" not in str(diag)

    stats.port, stats.fell_back = 41234, True
    assert plugin.diagnostics()["Browser camera port"] == "41234 (8767 was taken)"


def test_waiting_hint_points_at_the_step_that_failed():
    stats = ServerStats(8767, 8767)
    t0 = stats.started
    assert waiting_hint(stats, False, t0 + 5) == ""  # still scanning, probably
    assert "firewall allows port 8767" in waiting_hint(stats, False, t0 + HINT_AFTER_S)
    assert waiting_hint(stats, True, t0 + HINT_AFTER_S) == ""
    stats.note("connections")
    assert waiting_hint(stats, False, t0 + HINT_AFTER_S) == ""
    stats.note("tls_failed")
    assert "certificate" in waiting_hint(stats, False, t0 + 1)
    stats.note("pages")
    assert waiting_hint(stats, False, t0 + HINT_AFTER_S) == ""  # on the page now


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
    host.clear_issue = lambda key: host.issues.pop(key, None)
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
