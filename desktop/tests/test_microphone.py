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

    def __init__(self, url, token, open_sink, on_status):
        self.url, self.token, self.on_status = url, token, on_status
        self.started = self.stopped = False
        _Worker.made.append(self)

    def start(self):
        self.started = True

    def stop(self):
        self.stopped = True


class _Ctrl:
    base = "http://10.0.0.5:8080/v1"
    token = "tok"


_PANELS = []


def _plugin(backend):
    _Worker.made = []
    p = MicrophonePlugin(backend=backend, worker_cls=_Worker, run_job=lambda fn: fn())
    p.setup(_Host(), EventBus())
    _PANELS.append(p.create_panel())
    return p


def test_on_follows_the_stream(qapp):
    backend = _Backend()
    p = _plugin(backend)
    p._toggle.setChecked(True)
    assert p._status.text() == "Starts with the stream."
    p.on_stream_start("url", _Ctrl())
    w = _Worker.made[-1]
    assert w.started and w.url == "http://10.0.0.5:8080/v1/audio" and w.token == "tok"
    w.on_status("ok", "")
    p._on_worker_status("ok", "")
    assert "Telescope Microphone" in p._status.text()
    p.on_stream_stop()
    assert w.stopped
    assert p.get_config() == {"enabled": True}


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
