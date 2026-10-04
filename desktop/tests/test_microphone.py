import pytest

from telescope.plugin import EventBus
from telescope.plugins.microphone import MicrophonePlugin


class _Host:
    def __init__(self):
        self.saves = 0

    def schedule_save(self):
        self.saves += 1


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
