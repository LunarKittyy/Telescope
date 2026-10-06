from types import SimpleNamespace

import pytest

import telescope.app as app_module
from telescope.plugin import EventBus
from telescope.plugins.microphone import MicrophonePlugin

from test_app import _Connection, _Signal, window  # noqa: F401 (the fixture)


class _Host:
    def __init__(self):
        self.saves = 0
        self.main, self.shown = None, None  # (id, name) the mic is on; the id the panels show

    def schedule_save(self):
        self.saves += 1

    def main_stream(self):
        return self.main

    def focused_source_id(self):
        return self.shown

    def is_streaming_from(self, _source_id):
        return True

    def stream_count(self):
        return 0


class _Backend:
    pick_name = "Telescope Microphone"

    def __init__(self, problem=None, prepare_err=""):
        self._problem = problem
        self._prepare_err = prepare_err
        self.prepared = 0
        self.torn_down = 0

    def problem(self):
        return self._problem

    def prepare(self):
        self.prepared += 1
        return self._prepare_err

    def open_sink(self):
        return None

    def teardown(self):
        self.torn_down += 1


class _Worker:
    made = []

    def __init__(self, url, auth, open_sink, on_status):
        self.url, self.auth, self.on_status = url, auth, on_status
        self.started = self.stopped = False
        self.muted = False
        self.gain = 1.0
        self.limit = True
        self.level = (0.0, 0.0, False)
        _Worker.made.append(self)

    def take_level(self):
        return self.level

    def start(self):
        self.started = True

    def stop(self):
        self.stopped = True


class _Ctrl:
    base = "http://10.0.0.5:8080/v1"
    auth = "tok"


_PANELS = []


def _plugin(backend):
    _Worker.made = []
    p = MicrophonePlugin(backend=backend, worker_cls=_Worker, run_job=lambda fn: fn())
    p.setup(_Host(), EventBus())
    _PANELS.append(p.create_panel())
    return p


def test_the_card_is_greyed_out_until_a_phone_is_paired(qapp):
    bus = EventBus()
    p = MicrophonePlugin(backend=_Backend(), worker_cls=_Worker, run_job=lambda fn: fn())
    p.setup(_Host(), bus)
    panel = p.create_panel()
    _PANELS.append(panel)
    assert not panel.isEnabled()
    bus.phones_changed.emit(1)
    assert panel.isEnabled()


def test_on_follows_the_stream(qapp):
    backend = _Backend()
    p = _plugin(backend)
    p._toggle.setChecked(True)
    assert p._status.text() == "Starts with the stream."
    p.on_stream_start("url", _Ctrl())
    w = _Worker.made[-1]
    assert w.started and w.url == "http://10.0.0.5:8080/v1/audio" and w.auth == "tok"
    w.on_status("ok", "")
    p._on_worker_status("ok", "")
    assert p._status_row.isHidden()  # the meter shows it's working
    p.on_stream_stop()
    assert w.stopped
    assert p.get_config() == {"enabled": True, "gain_db": 0}


def test_audio_follows_the_stream_onto_another_route(qapp):
    p = _plugin(_Backend())
    p._toggle.setChecked(True)
    p.on_stream_start("url", _Ctrl())
    first = _Worker.made[-1]

    p.on_stream_start("url", _Ctrl())  # reconnected over the same route: keep listening
    assert _Worker.made == [first] and not first.stopped

    usb = _Ctrl()
    usb.base = "http://127.0.0.1:41000/v1"
    p.on_stream_start("url", usb)
    assert first.stopped
    assert _Worker.made[-1].url == "http://127.0.0.1:41000/v1/audio" and _Worker.made[-1].started


def test_audio_follows_a_route_change_on_the_same_control_client(qapp):
    p = _plugin(_Backend())
    p._toggle.setChecked(True)
    ctrl = _Ctrl()
    ctrl.base = "http://127.0.0.1:41000/v1"
    p.on_stream_start("url", ctrl)
    usb = _Worker.made[-1]

    ctrl.base = "http://10.0.0.5:8080/v1"  # retargeted onto Wi-Fi, still the same client
    p.on_stream_start("url", ctrl)
    assert usb.stopped
    assert _Worker.made[-1].url == "http://10.0.0.5:8080/v1/audio" and _Worker.made[-1].started


def test_switching_off_mid_stream_stops_and_removes_the_virtual_mic(qapp):
    backend = _Backend()
    p = _plugin(backend)
    p.on_stream_start("url", _Ctrl())
    assert _Worker.made == []  # off: nothing listens
    p._toggle.setChecked(True)
    w = _Worker.made[-1]
    p._toggle.setChecked(False)
    assert w.stopped and backend.torn_down == 1
    assert p._status_row.isHidden()


def test_a_missing_piece_is_shown_with_its_fix_and_nothing_starts(qapp):
    backend = _Backend(problem=("Needs VB-Audio Virtual Cable (free).", ("Get VB-Cable", "https://vb-audio.com/Cable/")))
    p = _plugin(backend)
    p.set_config({"enabled": True})
    p.on_stream_start("url", _Ctrl())
    assert _Worker.made == []
    assert "VB-Audio" in p._status.text()
    assert p._action_btn.text() == "Get VB-Cable" and not p._action_row.isHidden()


def test_phone_refusal_is_shown(qapp):
    p = _plugin(_Backend())
    p.set_config({"enabled": True})
    p.on_stream_start("url", _Ctrl())
    p._on_worker_status("err", "Allow the microphone in Telescope on the phone")
    assert p._status.text() == "Allow the microphone in Telescope on the phone."


def test_setup_failure_is_shown(qapp):
    p = _plugin(_Backend(prepare_err="Couldn't create the virtual microphone: no pulse"))
    p.set_config({"enabled": True})
    p.on_stream_start("url", _Ctrl())
    assert _Worker.made == [] and "no pulse" in p._status.text()


def test_shutdown_stops_and_tears_down(qapp):
    backend = _Backend()
    p = _plugin(backend)
    p.set_config({"enabled": True})
    p.on_stream_start("url", _Ctrl())
    p.shutdown()
    assert _Worker.made[-1].stopped and backend.torn_down == 1


def test_setup_runs_off_the_ui_thread_and_a_late_result_is_ignored(qapp):
    jobs = []
    backend = _Backend()
    _Worker.made = []
    p = MicrophonePlugin(backend=backend, worker_cls=_Worker, run_job=jobs.append)
    p.setup(_Host(), EventBus())
    _PANELS.append(p.create_panel())
    p.set_config({"enabled": True})
    p.on_stream_start("url", _Ctrl())
    assert backend.prepared == 0 and p._status.text() == "Setting up…"  # queued, not run here
    p.on_stream_stop()  # the stream ends before setup finishes
    jobs.pop(0)()  # setup finishes late
    assert _Worker.made == [] and backend.prepared == 1
    p.on_stream_start("url", _Ctrl())
    jobs.pop(0)()
    assert _Worker.made[-1].started


def test_switching_off_queues_teardown_after_the_stop(qapp):
    jobs = []
    backend = _Backend()
    _Worker.made = []
    p = MicrophonePlugin(backend=backend, worker_cls=_Worker, run_job=jobs.append)
    p.setup(_Host(), EventBus())
    _PANELS.append(p.create_panel())
    p.on_stream_start("url", _Ctrl())
    p._toggle.setChecked(True)
    jobs.pop(0)()
    w = _Worker.made[-1]
    p._toggle.setChecked(False)
    assert not w.stopped and backend.torn_down == 0  # nothing ran on the UI thread
    for job in jobs:
        job()
    assert w.stopped and backend.torn_down == 1


def test_mute_writes_silence_without_dropping_the_mic_and_isnt_saved(qapp):
    p = _plugin(_Backend())
    assert p._mute_btn.isHidden() and p._level_row.isHidden()  # off: nothing to mute
    p._toggle.setChecked(True)
    assert not p._mute_btn.isHidden() and not p._level_row.isHidden()
    p._mute_btn.setChecked(True)  # before the stream: the worker starts muted
    p.on_stream_start("url", _Ctrl())
    w = _Worker.made[-1]
    assert w.muted and not w.stopped
    assert p._mute_btn.text() == "Unmute" and p._readout.text() == "Muted"
    p._mute_btn.setChecked(False)
    assert not w.muted and p._mute_btn.text() == "Mute"
    assert p.get_config() == {"enabled": True, "gain_db": 0}


def test_meter_follows_the_worker_and_clears_when_the_stream_stops(qapp):
    p = _plugin(_Backend())
    p.set_config({"enabled": True})
    p.on_stream_start("url", _Ctrl())
    assert p._meter_timer.isActive()
    _Worker.made[-1].level = (0.5, 0.2, False)
    p._tick_meter()
    assert p._readout.text() == "-6 dB"
    p.on_stream_stop()
    assert not p._meter_timer.isActive() and p._readout.text() == ""


def test_gain_reaches_the_worker_and_is_saved_per_phone(qapp):
    p = _plugin(_Backend())
    p.set_config({"enabled": True, "gain_db": 6})
    assert p._gain_spin.text() == "+6 dB" and not p._gain_row.isHidden()
    p.on_stream_start("url", _Ctrl())
    w = _Worker.made[-1]
    assert w.gain == pytest.approx(1.995, abs=0.001)
    p._gain_slider.setValue(-30)  # past the end: held at the minimum
    assert w.gain == pytest.approx(10 ** (-24 / 20)) and p._gain_spin.text() == "-24 dB"
    assert p.get_config() == {"enabled": True, "gain_db": -24}
    p.set_config({"enabled": True, "gain_db": "junk"})
    assert w.gain == 1.0 and p._gain_spin.text() == "0 dB"


def test_typing_a_gain_moves_the_slider(qapp):
    p = _plugin(_Backend())
    p.set_config({"enabled": True})
    p._gain_spin.setValue(9)  # what Enter does after typing +9
    assert p._gain_slider.value() == 9 and p.get_config()["gain_db"] == 9


def test_max_gain_from_advanced_sets_the_range_and_pulls_a_higher_gain_down(qapp):
    bus = EventBus()
    _Worker.made = []
    p = MicrophonePlugin(backend=_Backend(), worker_cls=_Worker, run_job=lambda fn: fn())
    p.setup(_Host(), bus)
    bus.max_gain_changed.emit(48)  # Setup's config loads before the card is built
    _PANELS.append(p.create_panel())
    p.set_config({"enabled": True, "gain_db": 40})
    assert p._gain_slider.maximum() == 48 and p._gain_spin.maximum() == 48 and p.get_config()["gain_db"] == 40
    bus.max_gain_changed.emit(12)
    assert p._gain_slider.maximum() == 12 and p.get_config()["gain_db"] == 12 and p._gain_spin.value() == 12


def test_tray_mute_follows_the_card_both_ways(qapp):
    p = _plugin(_Backend())
    [tray] = p.create_tray_actions()
    assert not tray.isVisible()  # mic off: nothing to mute
    p.set_config({"enabled": True})
    assert tray.isVisible() and not tray.isChecked()
    tray.trigger()
    assert p._mute_btn.isChecked() and p._mute_btn.text() == "Unmute"
    p._mute_btn.setChecked(False)
    assert not tray.isChecked()


def test_limiter_switch_from_advanced_reaches_the_worker(qapp):
    bus = EventBus()
    _Worker.made = []
    p = MicrophonePlugin(backend=_Backend(), worker_cls=_Worker, run_job=lambda fn: fn())
    p.setup(_Host(), bus)
    bus.limiter_changed.emit(False)
    _PANELS.append(p.create_panel())
    p.set_config({"enabled": True})
    p.on_stream_start("url", _Ctrl())
    w = _Worker.made[-1]
    assert w.limit is False
    bus.limiter_changed.emit(True)
    assert w.limit is True
    w.level = (0.89, 0.3, True)
    p._tick_meter()
    assert p._meter.limiting() and not p._meter.clipping()


# ── Several streams: one mic, on the stream whose card has it on ─────────────

class _Cam:
    url = "browser:"
    remember_changes_only = False

    def __init__(self, sid, ip):
        self.id, self.name = sid, sid.title()
        self.ctrl = SimpleNamespace(base=f"http://{ip}/v1", auth="tok", send=lambda **_kw: None,
                                    get_state=lambda: None, close=lambda: None)

    def prepare(self, interactive):
        return True

    def open_reader(self):
        return None

    def control_client(self):
        return self.ctrl

    def fps(self):
        return 30


class _StreamWorker:
    def __init__(self, **kwargs):
        self.status, self.reconnected, self.vcam_opened = _Signal(), _Signal(), _Signal()

    def start(self):
        pass

    def request_stop(self):
        pass

    def wait(self, _ms):
        return True

    def set_pipeline(self, _steps):
        pass

    def latest_frame(self):
        return None


@pytest.fixture
def two_cams(window, config_home, monkeypatch):
    monkeypatch.setattr(app_module, "StreamWorker", _StreamWorker)
    monkeypatch.setattr(app_module.vcam, "slot_ready", lambda _slot: True)
    conn = _Connection(selected="front")
    conn.ensure_virtual_camera = lambda interactive=True: True
    _Worker.made = []
    mic = MicrophonePlugin(backend=_Backend(), worker_cls=_Worker, run_job=lambda fn: fn())
    for plugin in (conn, mic):
        window.register_plugin(plugin)
    for cam in (_Cam("front", "10.0.0.5"), _Cam("back", "10.0.0.6")):
        window.add_stream_source(cam)
    window._bus.phones_changed.emit(2)  # the card is greyed out until then
    window._start()
    mic._toggle.setChecked(True)  # on for the first
    window.add_stream("back")  # the panels move to it
    return window, mic, config_home


def _mic_on(config_home, sid):
    cfg = config_home.load_config()
    return cfg.get("devices", {}).get(sid, {}).get("plugin_configs", {}).get("microphone", {}).get("enabled")


def test_another_streams_card_shows_its_mic_off_and_turning_it_on_moves_the_mic(two_cams):
    window, mic, config_home = two_cams
    assert window._focus.id == "back" and window.main_stream() == ("front", "Front")
    assert not mic._toggle.isChecked() and mic._toggle.isEnabled()
    assert mic._gain_row.isHidden() and "On for Front" in mic._status.text()
    assert [w.url for w in _Worker.made if not w.stopped] == ["http://10.0.0.5/v1/audio"]

    mic._toggle.click()
    assert window.main_stream() == ("back", "Back")
    assert mic._toggle.isChecked() and not mic._gain_row.isHidden() and mic._status.text() == "Connecting…"
    assert [w.url for w in _Worker.made if not w.stopped] == ["http://10.0.0.6/v1/audio"]
    assert _mic_on(config_home, "front") is False  # the one it left keeps it off
    assert mic._backend.torn_down == 0  # apps keep the same microphone throughout

    window.focus_stream("front")
    assert not mic._toggle.isChecked() and "On for Back" in mic._status.text()

    window.focus_stream("back")
    mic._toggle.click()  # off on the one that has it: off, and it stays there
    assert window.main_stream() == ("back", "Back") and not mic._enabled
    window.focus_stream("front")
    assert not mic._toggle.isChecked() and mic._status.text() == ""
    mic._toggle.click()  # on again, here
    assert window.main_stream() == ("front", "Front") and mic._enabled
    assert [w.url for w in _Worker.made if not w.stopped] == ["http://10.0.0.5/v1/audio"]


def test_the_mic_stays_on_when_the_stream_with_it_stops(two_cams):
    window, mic, config_home = two_cams
    mic._toggle.click()  # moved to the back camera; the front one keeps it off
    window.stop_stream("back")
    assert window.main_stream() == ("front", "Front") and mic._enabled and mic._toggle.isChecked()
    assert [w.url for w in _Worker.made if not w.stopped] == ["http://10.0.0.5/v1/audio"]


def test_a_card_whose_camera_is_still_starting_cant_take_the_mic(two_cams):
    window, mic, _config = two_cams
    window.stop_stream("back")
    window._move_focus(window._source_by_id("back"))  # as + does, before its stream is up
    assert not mic._toggle.isChecked() and not mic._toggle.isEnabled()
