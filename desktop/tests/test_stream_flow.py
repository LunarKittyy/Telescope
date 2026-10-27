"""StreamFlow on its own: starting, stopping and moving the panels between streams, with a stand-in for the window."""

from types import SimpleNamespace

import numpy as np
import pytest

from telescope import packets
from telescope.plugin import TelescopePlugin
from telescope.stream_flow import StreamFlow


class _Signal:
    def __init__(self, log, name):
        self._log, self._name = log, name

    def emit(self, *args):
        self._log.append((self._name, *args))


class _Bus:
    def __init__(self, log):
        for name in ("stream_started", "stream_stopped", "stream_start_failed", "streams_changed", "stream_behind",
                     "stream_lost", "stream_connected", "device_changed", "idle_outputs", "camera_on_changed"):
            setattr(self, name, _Signal(log, name))


class _Plugin(TelescopePlugin):
    def __init__(self, name, log, follows_focus=True):
        self.name, self._log, self.follows_focus = name, log, follows_focus

    def __getattribute__(self, attr):
        if attr.startswith("on_") and attr != "on_plugins_ready":
            return lambda *_a: object.__getattribute__(self, "_log").append((self.name, attr))
        return object.__getattribute__(self, attr)


class _Worker:
    def __init__(self, live):
        self.live, self.parked, self.stopped = live, False, False
        self.packets = packets.Tap()

    def set_pipeline(self, steps):
        self.live = steps == ["live"]


class _Source:
    def __init__(self, sid, wakes=False):
        self.id, self.name, self.wakes = sid, sid, wakes
        self.session = None
        self.slot = 0
        self.keep_slot = None
        self.camera_off = self.camera_off_auto = False
        self.camera_toggle = None
        self.recovering = self.connected = self.behind = self.arrival_slow = False
        self.settling_reports = 0
        self.auto_canvas = None
        self.url, self.auth = f"http://{sid}/", None
        self.status = ("Not streaming", "dim")
        self.woken = self.cancelled = False

    def redirect(self):
        return None

    def prepare(self, _interactive):
        return True

    def waiting(self):
        return False

    def wake(self):
        self.woken = True

    def control_client(self):
        return SimpleNamespace(close=lambda: None)

    def stream_params(self):
        return 1280, 720, 30

    def started(self, _session_id):
        pass

    def stopped(self, _remote_stop, active):
        pass

    def refresh_state(self, _session_id):
        pass

    def end_recovery(self):
        self.recovering = False

    def forget_stream_status(self):
        pass

    def cancel_wait(self):
        self.cancelled = True

    def start_lost_clock(self):
        pass

    def lost(self):
        pass


class _Host:
    """The window, as far as StreamFlow sees it: it notes what it's asked and keeps a profile per source."""

    def __init__(self, plugins, sources, log):
        self.bus = _Bus(log)
        self._plugins, self._sources, self.log = plugins, sources, log
        self.selected = next(iter(sources))
        self.problems, self.saved, self.applied = [], [], []
        self.packet_makers, self.keyframes = {}, []
        self.ended = []
        self.connection = SimpleNamespace(selected_device=self.selected, select=self._select,
                                          show_selected=lambda _sid: None)

    def _select(self, sid):
        self.selected = self.connection.selected_device = sid

    def plugins(self):
        return self._plugins

    def plugin(self, name):
        return self.connection if name == "connection" else None

    def call(self, plugins, hook, *_args):
        for p in plugins:
            getattr(p, hook)()

    def source_by_id(self, sid):
        return self._sources[sid]

    def plugin_source(self, sid):
        return None

    def selected_source(self):
        return self._sources[self.selected]

    def later(self, fn):
        fn()

    def start_problem(self, title, _text):
        self.problems.append(title)

    def save_profile(self, sid, plugins):
        self.saved.append((sid, [p.name for p in plugins]))

    def apply_device_profile(self, sid, plugins):
        self.applied.append((sid, [p.name for p in plugins]))

    def canvas_size(self):
        return 1920, 1080

    def new_worker(self, _source, _url, _auth, _params, pipeline, _canvas):
        return _Worker(pipeline == ["live"])

    def end_worker(self, worker):
        worker.stopped = True
        self.ended.append(worker)

    def packet_steps(self):
        return dict(self.packet_makers)

    def ask_keyframe(self, source):
        self.keyframes.append(source.id)

    def set_behind(self, behind, source):
        source.behind = behind

    def __getattr__(self, name):  # the widgets: nothing to check here
        return lambda *_a, **_k: None


@pytest.fixture
def setup(monkeypatch):
    log = []
    camera, mic = _Plugin("camera", log), _Plugin("mic", log, follows_focus=False)
    sources = {"a": _Source("a"), "b": _Source("b"), "phone": _Source("phone", wakes=True)}
    host = _Host([camera, mic], sources, log)
    flow = StreamFlow(host)
    monkeypatch.setattr(flow, "pipeline", lambda live=True: ["live"] if live else ["frozen"])
    monkeypatch.setattr("telescope.stream_flow.vcam.slot_ready", lambda _slot: True)
    return flow, host, sources, log


def _hooks(log):
    hooks = [e for e in log if not e[0].startswith(("stream", "device", "idle"))]
    log.clear()
    return hooks


def test_a_source_that_needs_no_waking_starts_right_away(setup):
    flow, host, sources, log = setup
    flow.start()
    a = sources["a"]
    assert flow.sessions.streams == [a] and flow.sessions.focus is a and flow.sessions.main is a
    assert a.session.worker.live and a.slot == 0
    assert ("stream_started", a.url) in log
    assert _hooks(log) == [("camera", "on_stream_starting"), ("mic", "on_stream_starting"),
                           ("camera", "on_stream_start"), ("mic", "on_stream_start")]


def test_a_phone_wakes_first_and_a_failed_wake_says_so(setup):
    flow, host, sources, log = setup
    host._select("phone")
    flow.start()
    phone = sources["phone"]
    assert phone.woken and flow.sessions.waking and flow.sessions.wake_source is phone and not flow.sessions.streams
    flow.wake_done(phone, False, "The phone didn't answer")
    assert not flow.sessions.waking and host.problems == ["Couldn't start the phone's camera"]
    assert ("stream_start_failed",) in log and not flow.sessions.streams
    flow.start()
    flow.wake_done(phone, True, "")
    assert flow.sessions.streams == [phone]


def _two_streams(flow, host, log):
    flow.start()
    flow.add_stream("b")
    log.clear()
    host.saved.clear()
    host.applied.clear()


def test_a_second_stream_gets_its_own_camera_and_the_panels(setup):
    flow, host, sources, log = setup
    flow.start()
    log.clear()
    flow.add_stream("b")
    a, b = sources["a"], sources["b"]
    assert flow.sessions.streams == [a, b] and flow.sessions.focus is b and flow.sessions.main is a
    assert b.slot == 1 and flow.slot_memory == {"b": 1}
    assert not a.session.worker.live and b.session.worker.live
    assert ("camera", "on_focus_lost") in log and ("mic", "on_focus_lost") not in log
    assert ("camera", "on_stream_start") in log and ("mic", "on_stream_start") not in log  # the mic stays on a


def test_clicking_a_tile_moves_the_panels_and_not_the_mic(setup):
    flow, host, sources, log = setup
    _two_streams(flow, host, log)
    a, b = sources["a"], sources["b"]
    flow.focus_stream("a")
    assert flow.sessions.focus is a and flow.sessions.main is a
    assert _hooks(log) == [("camera", "on_focus_lost"), ("camera", "on_focus_gained")]
    assert host.saved == [("b", ["camera"])] and host.applied == [("a", ["camera"])]
    assert a.session.worker.live and not b.session.worker.live
    assert flow.sessions.streams == [a, b]  # nothing stopped


def test_stopping_the_shown_stream_moves_the_panels_to_one_left(setup):
    flow, host, sources, log = setup
    _two_streams(flow, host, log)
    a, b = sources["a"], sources["b"]
    worker = b.session.worker
    flow.stop_stream()
    assert worker.stopped and flow.sessions.streams == [a] and flow.sessions.focus is a
    assert _hooks(log) == [("camera", "on_stream_stop"), ("camera", "on_focus_gained")]
    assert ("stream_stopped",) not in log  # a still streams


def test_stopping_the_mics_stream_moves_the_mic_to_the_next(setup):
    flow, host, sources, log = setup
    _two_streams(flow, host, log)
    b = sources["b"]
    flow.stop_stream("a")
    assert flow.sessions.streams == [b] and flow.sessions.main is b and flow.sessions.focus is b
    assert _hooks(log) == [("mic", "on_stream_stop"), ("mic", "on_focus_gained")]
    assert host.applied == [("b", ["mic"])]


def test_a_restart_of_the_mics_stream_keeps_the_mic_waiting_for_it(setup):
    flow, host, sources, log = setup
    _two_streams(flow, host, log)
    a = sources["a"]
    flow.stop_for_restart(a)
    assert flow.sessions.main is a and flow.sessions.streams == [sources["b"]]  # the mic waits for a
    assert _hooks(log) == [("mic", "on_stream_stop")]
    flow.start(source=a)
    assert flow.sessions.main is a and a.slot == 0  # back on its own camera
    assert ("mic", "on_stream_start") in _hooks(log)


def test_making_another_stream_main_moves_only_the_mic(setup):
    flow, host, sources, log = setup
    _two_streams(flow, host, log)
    assert flow.make_main("b") and flow.sessions.main is sources["b"]
    assert _hooks(log) == [("mic", "on_focus_lost"), ("mic", "on_focus_gained")]
    assert host.saved == [("a", ["mic"])] and host.applied == [("b", ["mic"])]
    assert not flow.make_main("nope")


def test_stop_all_stops_everything_and_puts_the_mic_settings_back(setup):
    flow, host, sources, log = setup
    _two_streams(flow, host, log)
    flow.stop_all()
    assert not flow.sessions.streams and flow.sessions.focus is None and flow.sessions.main is None
    assert ("stream_stopped",) in log
    assert host.saved[-1] == ("a", ["mic"]) and host.applied[-1] == ("b", ["mic"])  # idle, the panels show b


def test_a_pending_source_can_be_cancelled_from_its_tile(setup):
    flow, host, sources, log = setup
    flow.start()
    waiting = sources["b"]
    waiting.prepare = lambda _i: False
    waiting.waiting = lambda: True
    flow.add_stream("b")
    assert flow.sessions.pending is waiting and flow.sessions.focus is waiting
    flow.stop_stream("b")
    assert flow.sessions.pending is None and waiting.cancelled and flow.sessions.focus is sources["a"]


def test_a_plugin_that_sees_the_final_frame_gets_it_after_plugins_registered_later():
    order = []

    class Step(TelescopePlugin):
        def __init__(self, name, last=False):
            self.name, self.sees_final_frame = name, last

        def process_frame(self, frame):
            order.append(self.name)
            return frame

    class Host:
        def plugins(self):
            return [Step("transforms"), Step("preview", last=True), Step("added"), Step("added_too")]

    frame = np.zeros((2, 2, 3), np.uint8)
    for step in StreamFlow(Host()).pipeline():
        frame = step(frame)
    assert order == ["transforms", "added", "added_too", "preview"]


def test_frames_go_through_the_stages_in_order_whatever_order_plugins_are_registered_in():
    order = []

    class Step(TelescopePlugin):
        def __init__(self, name, stage):
            self.name, self.frame_stage = name, stage

        def process_frame(self, frame):
            order.append(self.name)
            return frame

    class Host:
        def plugins(self):
            return [Step("tag", "overlay"), Step("grade", "colour"), Step("odd", "nowhere"), Step("blur", "privacy"),
                    Step("key", "background"), Step("view", "view"), Step("more", "effects")]

    frame = np.zeros((2, 2, 3), np.uint8)
    for step in StreamFlow(Host()).pipeline():
        frame = step(frame)
    assert order == ["view", "key", "odd", "more", "grade", "blur", "tag"]  # an unknown stage counts as effects


def test_the_built_in_plugins_frame_order_is_the_one_written_down():
    from telescope.colour import ColourPipeline
    from telescope.plugin import frame_stage_index
    from telescope.plugins import builtin_plugins
    from telescope.view_controller import ViewController
    classes = [ViewController, *(type(p) for p in builtin_plugins()), ColourPipeline]
    steps = [cls for cls in classes if cls.process_frame is not TelescopePlugin.process_frame]
    for cls in steps:
        assert "frame_stage" in vars(cls), f"{cls.__name__} says which frame stage it's in"
    assert [cls.name for cls in sorted(steps, key=frame_stage_index)] == [
        "view", "desk", "qr_reader", "background", "chroma_key", "fun_pack", "colour", "privacy_blur", "name_tag",
        "composite", "wait_screen", "preview"]


def test_packet_steps_follow_the_panels_and_the_stream_left_gets_a_keyframe(setup):
    flow, host, sources, log = setup
    host.packet_makers = {"fun": lambda: lambda unit: []}
    _two_streams(flow, host, log)
    a, b = sources["a"].session.worker, sources["b"].session.worker
    assert b.packets.on and not a.packets.on
    assert host.keyframes == ["a"]  # the panels moved to the new stream, which started with one anyway
    host.keyframes.clear()
    flow.move_focus(sources["a"])
    assert a.packets.on and not b.packets.on
    assert host.keyframes == ["b", "a"]

