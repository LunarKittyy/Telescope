"""Scenes for connection, phones, sources and streams: see translation_scenes.py for how scenes are registered."""

import contextlib

from scripts.translation_scenes import *  # noqa: F401,F403
from scripts.translation_scenes import bubble, capture_notifications, grow, hold_dialogs, main_window, remember, scene, shown, use_scratch_profile  # noqa: F401,E501

WIFI_IP = "192.168.1.20"
USB_SERIAL = "R5CT123ABC"


class Freeze(BaseException):
    """Raised by a stand-in to stop the real code part-way, so the picture shows the state it was in."""


def frozen(call, *args, **kwargs):
    """Runs call and keeps whatever state it reached if a stand-in froze it."""
    try:
        return call(*args, **kwargs)
    except Freeze:
        return None


# ── The window with a phone ──────────────────────────────────────────────────

def mapped_tags(stack):
    """Tags draw their text themselves, so note which translated text each one shows."""
    from unittest import mock
    from telescope.widgets.controls import Tag
    original = Tag.__init__

    def init(self, text, *args, **kwargs):
        original(self, text, *args, **kwargs)
        remember(self, "text", text)

    stack.enter_context(mock.patch.object(Tag, "__init__", init))


def connection_window(stack, phone=True, second=False):
    """The real window with Pixel 8 picked (and Galaxy S23 paired too, with second) and its Connection plugin."""
    mapped_tags(stack)
    win = main_window(stack, phone=phone)
    plugin = next(p for p in win._plugins if p.name == "connection")
    if second:
        config = plugin.get_config()
        plugin.set_config({**config, "phones": [*config["phones"], {"id": "phone-2", "name": "Galaxy S23", "token": "token",
                                                                      "ips": [], "active_ip": None}]})
    return win, plugin


def picture(win, height=700):
    """win three columns wide and as tall as what it shows now needs."""
    import telescope.app as app_module
    grow(win, app_module._WIDTH_THREE_COL, height)
    return win


def with_adb(stack, available=True, windows=False, apk=True):
    """The computer as one with or without adb (and the phone app's file), on Linux or on Windows."""
    from pathlib import Path
    from unittest import mock
    import telescope.phones as phones
    import telescope.plugins.connection as connection
    for module, name, value in ((connection, "adb_available", lambda: available), (connection, "IS_WINDOWS", windows),
                                (phones, "_WINDOWS", windows),
                                (connection, "bundled_apk_path", lambda: Path("telescope.apk") if apk else None)):
        stack.enter_context(mock.patch.object(module, name, value))


# ── Connection card: how the phone is doing ──────────────────────────────────

def resolution(status, route=None, note=None, **fields):
    """A Resolution made when its scene is built (the scenes are listed before any telescope module is imported)."""
    def make():
        from telescope.phones import Resolution, Route
        kind = {"wifi": Route("wifi", WIFI_IP), "usb": Route("usb", "127.0.0.1", USB_SERIAL), None: None}[route]
        return Resolution(status, kind, usb_note=note, **fields)
    return make


def card_states():
    """(id, what it shows, the Resolution to apply, options) for each way the Connection card describes the phone."""
    return (
        ("ready", "ready over Wi-Fi", resolution("ready", "wifi", "no_cable"), {}),
        ("ready-usb", "ready over the cable", resolution("ready", "usb"), {}),
        ("cable-unauthorized", "cable in, USB debugging not allowed", resolution("ready", "wifi", "unauthorized"), {}),
        ("cable-other-phone", "cable in, a different phone", resolution("ready", "wifi", "other_phone"), {}),
        ("cable-app-closed", "cable in, app not open", resolution("ready", "wifi", "app_closed"), {}),
        ("unreachable", "can't reach the phone", resolution("unreachable"), {}),
        ("not-paired", "needs pairing again", resolution("not_paired"), {}),
        ("not-paired-old", "needs pairing again (paired before encryption)", resolution("not_paired", before_tls=True), {}),
        ("local-only", "phone accepts USB only", resolution("local_only"), {}),
        ("local-only-windows", "phone accepts USB only, no adb on Windows", resolution("local_only"),
         {"adb": False, "windows": True}),
        ("usb-unknown", "USB only, phone not answering", resolution("usb_attention"), {"route": "usb"}),
        ("usb-no-adb", "USB only, no adb", resolution("usb_attention", note="no_adb"), {"route": "usb", "adb": False}),
        ("usb-no-adb-windows", "USB only, no adb on Windows", resolution("usb_attention", note="no_adb"),
         {"route": "usb", "adb": False, "windows": True}),
        ("usb-no-cable", "USB only, nothing plugged in", resolution("usb_attention", note="no_cable"), {"route": "usb"}),
        ("usb-unauthorized", "USB only, debugging not allowed", resolution("usb_attention", note="unauthorized"),
         {"route": "usb"}),
        ("usb-other-phone", "USB only, a different phone", resolution("usb_attention", note="other_phone"), {"route": "usb"}),
        ("usb-app-closed", "USB only, app not open", resolution("usb_attention", note="app_closed"), {"route": "usb"}),
        ("phone-outdated", "phone app needs an update", resolution("phone_outdated", "wifi"), {}),
        ("phone-outdated-usb", "phone app needs an update, over the cable", resolution("phone_outdated", "usb"), {}),
        ("desktop-outdated", "this app needs an update", resolution("desktop_outdated", "wifi"), {}),
    )


def card_state(stack, res, route="auto", adb=True, windows=False):
    with_adb(stack, adb, windows)
    win, plugin = connection_window(stack)
    plugin._route_pref = route
    plugin._apply_resolution(res())
    return picture(win)


def register_card_states():
    for id, title, res, options in card_states():
        scene(f"connection-{id}", "states", f"Connection · {title}")(
            lambda stack, res=res, options=options: card_state(stack, res, **options))


register_card_states()


# ── Starting a stream ────────────────────────────────────────────────────────

class Clock:
    """Time that only moves when the code sleeps, so waiting messages say the same seconds every time."""

    def __init__(self):
        self.now = 1000.0

    def monotonic(self):
        return self.now

    def sleep(self, seconds):
        self.now += 2.0


class DirectSignal:
    """A signal that gives what it is sent straight to its slots: a real one turns a translated text into a plain string."""

    def __init__(self):
        self.slots = []

    def connect(self, slot, *_type):
        self.slots.append(slot)

    def disconnect(self, slot=None):
        if slot is None:
            self.slots.clear()
        else:
            self.slots.remove(slot)

    def emit(self, *args):
        for slot in list(self.slots):
            slot(*args)


class Direct:
    """A class attribute in place of a pyqtSignal: each object gets a signal of its own that keeps texts as they were made."""

    def __init__(self, name):
        self.name = "_direct_" + name

    def __get__(self, instance, owner=None):
        return self if instance is None else instance.__dict__.setdefault(self.name, DirectSignal())


class StandInClient:
    """The control client of a stream, with nothing sent to a phone."""

    def __init__(self, *_args, **_kwargs):
        pass

    def __getattr__(self, _name):
        return lambda *_a, **_k: None


def start_flow(stack, script=None, route=None, camera_off=False, patches=(), clock=None, resolution=None, forward="all",
               phone=True, second=False):
    """The window after Start was pressed with a phone that answers as script says (pings, then a Freeze), by the real
    start, wake and banner code. Nothing leaves the computer: the phone's session port and the stream worker are stand-ins."""
    from types import SimpleNamespace
    from unittest import mock
    from PyQt6.QtWidgets import QApplication
    import telescope.app as app_module
    import telescope.plugins.connection as connection
    import telescope.sources as sources
    from telescope.phones import READY, Resolution, Route
    from telescope import vcam
    from telescope.session_client import PingResult, SessionResult
    from telescope.stream import StreamWorker
    for name in ("wake_progress", "wake_done"):  # before the window makes its phone source, which connects them
        stack.enter_context(mock.patch.object(sources.PhoneSource, f"_sig_{name}", Direct(name)))
    win, plugin = connection_window(stack, phone=phone, second=second)
    route = route or Route("wifi", WIFI_IP)
    clock = clock or Clock()
    live = PingResult("paired", streaming=True, busy=False, local_only=False)
    steps = list(script) if script is not None else None  # without a script the phone is always streaming already

    def ping(_client):
        if steps is None:
            return live
        step = steps.pop(0) if steps else Freeze
        if step is Freeze:
            raise Freeze
        return step(clock) if callable(step) else step

    class QuietWorker(StreamWorker):
        status = Direct("status")

        def run(self):
            pass

    answer = resolution or Resolution(READY, route, phone_version="3.0.0")
    plugin._resolver.resolve = lambda _phone, _preference: answer
    # Over a cable: every port opens, or only the stream's (the phone's session port doesn't), or none does
    ports = {"all": lambda remote, serial=None: 41000, "stream": lambda remote, serial=None: 41000 if remote == 8080 else None,
             "none": lambda remote, serial=None: None}[forward]
    for target, name, value in (
            (app_module, "StreamWorker", QuietWorker),
            (sources, "PhoneControlClient", StandInClient),
            (sources.PhoneSource, "_fetch_state_async", lambda self, *a, **k: None),
            (sources.PhoneSource, "_spawn_state_fetch", lambda self, *a, **k: None),
            (sources.PhoneSource, "_spawn_recovery_probe", lambda self, *a, **k: None),
            (sources.PhoneSource, "_spawn_wake",
             lambda self, wake_id, conn, url, auth, target=None, opening=None:
             self._wake_phone(wake_id, conn, url, auth, target, opening)),
            (connection, "adb_forward_auto", ports),
            (connection, "time", SimpleNamespace(monotonic=clock.monotonic, sleep=clock.sleep)),
            (connection.PhoneSessionClient, "ping", ping),
            (connection.PhoneSessionClient, "start", lambda self, opening=None: SessionResult(ok=True)),
            (connection.PhoneSessionClient, "stop", lambda self: SessionResult(ok=True)),
            *patches):
        stack.enter_context(mock.patch.object(target, name, value))
    win._selected_source().camera_off = camera_off
    frozen(win._flow.start)
    if second:  # Galaxy S23 starts next to Pixel 8, on a virtual camera of its own
        stack.enter_context(mock.patch.object(vcam, "slot_ready", lambda _slot: True))
        win._flow.add_stream("phone-2")
    QApplication.processEvents()
    return win


def start_window(stack, **options):
    return picture(start_flow(stack, **options))


def pings():
    """The answers a phone gives while it wakes, so a scene reads as what the phone said."""
    from telescope.session_client import PingResult
    return {
        "idle": PingResult("paired", streaming=False, busy=False, local_only=False),
        "opening": PingResult("paired", streaming=False, busy=True, local_only=False),
        "live": PingResult("paired", streaming=True, busy=False, local_only=False),
        "quiet": PingResult("unreachable"),
        "unpaired": PingResult("not_paired"),
        "theirs": PingResult("paired", streaming=True, busy=False, local_only=False, streaming_for="Office PC"),
        "theirs-starting": PingResult("paired", streaming=False, busy=True, local_only=False, streaming_for="Office PC"),
    }


def timed_out(clock):
    """A phone that only ever says it is opening: the clock runs past the time to wait."""
    clock.now += 100
    return pings()["opening"]


def refused(error, computer=None):
    """The patch that makes the phone turn Start down with error."""
    def patch():
        import telescope.plugins.connection as connection
        from telescope.session_client import SessionResult
        return connection.PhoneSessionClient, "start", lambda self, opening=None: SessionResult(False, error, computer)
    return patch


def unreachable_phone():
    """The patch that makes waking the phone fail with something nobody planned for."""
    def patch():
        import telescope.sources as sources

        def wake(self, wake_id, conn, url, auth, target=None, opening=None):
            conn.ensure_phone_streaming = lambda **_kwargs: (_ for _ in ()).throw(RuntimeError("Connection refused"))
            self._wake_phone(wake_id, conn, url, auth, target, opening)
        return sources.PhoneSource, "_spawn_wake", wake
    return patch


def frozen_wake():
    """The patch that stops a start at the moment the phone is asked to wake."""
    def patch():
        import telescope.sources as sources

        def freeze(self, *_args, **_kwargs):
            raise Freeze
        return sources.PhoneSource, "_spawn_wake", freeze
    return patch


def prepare_fails():
    """The patch that makes getting the phone ready fail with something nobody planned for."""
    def patch():
        import telescope.sources as sources

        def prepare(self, interactive):
            raise RuntimeError("Something unexpected")
        return sources.PhoneSource, "prepare", prepare
    return patch


def wake_failures():
    """(id, what it shows, pings the phone gives, patches, options) for each way the phone can fail to wake."""
    return (
        ("refused-permission", "no camera permission", ["idle"], [refused("no_camera_permission")], {}),
        ("refused-busy", "phone busy", ["idle"], [refused("busy")], {}),
        ("refused-background", "Android won't open the camera", ["idle"], [refused("start_refused")], {}),
        ("refused-unpaired", "phone no longer knows this computer", ["idle"], [refused("not_paired")], {}),
        ("refused-other-computer", "phone streaming to another computer", ["idle"], [refused("busy_other", "Office PC")], {}),
        ("refused-other-unnamed", "phone streaming to a computer with no name", ["idle"], [refused("busy_other")], {}),
        ("refused-unknown", "phone refuses with a code", ["idle"], [refused("storage_full")], {}),
        ("refused-no-code", "phone refuses without a code", ["idle"], [refused(None)], {}),
        ("unpaired", "phone no longer knows this computer (ping)", ["unpaired"], [], {}),
        ("silent", "phone doesn't answer", ["quiet"], [], {}),
        ("other-computer", "phone already streams to another computer", ["theirs"], [], {}),
        ("other-computer-won", "another computer started the phone first", ["idle", "theirs-starting"], [], {}),
        ("lost-contact", "phone stops answering while the camera starts", ["idle", "quiet", "quiet", "quiet"], [], {}),
        ("unpaired-midway", "phone forgets this computer while the camera starts", ["idle", "unpaired"], [], {}),
        ("camera-stopped", "camera stops before it finished starting", ["idle", "opening", "idle"], [], {}),
        ("timeout", "camera takes too long", ["idle", timed_out, "opening"], [], {}),
        ("no-session", "no way to the phone's session port", ["idle"], [], {"route": "usb", "forward": "stream"}),
        ("crashed", "something unexpected while waking", ["idle"], [unreachable_phone()], {}),
    )


def script_of(names):
    """The pings a scene names (or its own steps), ready for start_flow."""
    answers = pings()
    return [answers.get(name, name) if isinstance(name, str) else name for name in names]


def start_scene(id, section_id, title, script=None, patches=(), **options):
    """Registers a scene of the window after Start, with the phone answering as script says."""
    def build(stack):
        from telescope.phones import Route
        route = Route("usb", "127.0.0.1", USB_SERIAL) if options.get("route") == "usb" else None
        plain = {k: v for k, v in options.items() if k != "route"}
        return start_window(stack, script=None if script is None else script_of(script),
                            patches=[patch() for patch in patches], route=route, **plain)
    scene(id, section_id, title)(build)


for _id, _title, _script, _patches, _options in wake_failures():
    start_scene(f"start-{_id}", "states", f"Start · {_title}", _script, _patches, **_options)

for _id, _title, _script in (("opening", "waiting for the camera", ["idle", "idle"]),
                             ("camera-opening", "the camera is opening", ["idle", "opening"]),
                             ("went-quiet", "the phone went quiet for a moment", ["idle", "opening", "quiet"])):
    start_scene(f"start-{_id}", "main", f"Start · {_title}", [*_script, Freeze])

start_scene("start-camera-wake", "main", "Start · asking the phone to start its camera", None, [frozen_wake()])
start_scene("start-mic-wake", "main", "Start · asking the phone to start its mic", None, [frozen_wake()], camera_off=True)
start_scene("start-crashed-prepare", "states", "Start · something went wrong getting ready", None, [prepare_fails()])
start_scene("start-no-phone", "states", "Start · no phone paired yet", None, phone=False)


def start_problem(id, title, make, route=None):
    """Registers Start pressed while the phone can't be used: the banner, with the buttons that fix it."""
    def build(stack):
        from telescope.phones import Route
        kind = Route("usb", "127.0.0.1", USB_SERIAL) if route == "usb" else Route("wifi", WIFI_IP)
        return start_window(stack, resolution=make(), route=kind)
    scene(f"start-problem-{id}", "states", f"Start · {title}")(build)


start_problem("unreachable", "banner when the phone can't be reached", resolution("unreachable"))
start_problem("not-paired", "banner when the phone needs pairing again", resolution("not_paired"))
start_problem("desktop-outdated", "banner when this app needs an update", resolution("desktop_outdated", "wifi"))
start_problem("phone-outdated-usb", "banner when the phone app can be updated over the cable",
              resolution("phone_outdated", "usb"), route="usb")
start_problem("usb-only", "banner when USB only has no cable", resolution("usb_attention", note="no_cable"))


# ── A stream that runs: what the stream worker says ──────────────────────────

class FakeReader:
    """What the worker reads from: opens or not, gives a frame or not, and tells the worker to stop once asked."""

    last_frame_bytes = 0
    last_frame_count = 1
    parallel_decode = False

    def __init__(self, worker, opens=True, frames=True, stop_on=None):
        self.worker, self.opens, self.frames, self.stop_on = worker, opens, frames, stop_on

    def stop(self, when):
        if self.stop_on == when:
            self.worker._stop_flag = True

    def isOpened(self):
        self.stop("open")
        return self.opens

    def read(self):
        self.stop("read")
        return self.frames, (None if not self.frames else 1)

    def read_packet(self):
        self.stop("read")
        return self.frames, b"packet"

    def decode(self, _packet):
        return None

    def release(self):
        pass


class Camera:
    """The virtual camera, open for a moment."""
    device = "/dev/video10"

    def __enter__(self):
        return self

    def __exit__(self, *_exc):
        return False


def worker_scene(stack, part, route=None, **options):
    """The window with a stream running (from the real Start) after the real stream worker did part: each status it
    sends goes to the window as it would from its thread, so the footer, the card and the banners say what they say."""
    win = start_flow(stack, route=route, **options)
    part(win, win._session.worker)
    from PyQt6.QtWidgets import QApplication
    win._stop_reconnecting_animation()
    QApplication.processEvents()
    return picture(win)


def streams(win, worker):
    import numpy as np
    worker._latest = np.zeros((720, 1280, 3), np.uint8)
    worker.parked = True  # the virtual camera loop leaves as soon as it has said it's open
    worker._open_vcam = lambda _w, _h: Camera()
    worker._run_vcam()


def virtual_camera_fails(win, worker):
    import numpy as np
    worker._latest = np.zeros((720, 1280, 3), np.uint8)

    def refuse(_w, _h):
        raise OSError("Device or resource busy")
    worker._open_vcam = refuse
    worker._run_vcam()


def connecting(win, worker):
    worker._stop_flag = True
    worker._run()


def loading_video(win, worker):
    import telescope.h264_reader as h264_reader

    def freeze():
        raise Freeze
    worker.url = worker.url + ".h264"
    worker._stop_flag = False
    from unittest import mock
    with mock.patch.object(h264_reader, "loaded", lambda: False), mock.patch.object(h264_reader, "load_av", freeze):
        frozen(worker._run)


def unreachable_stream(win, worker):
    import threading
    worker._open_cap = lambda: FakeReader(worker, opens=False, stop_on="open")
    worker._restart_vcam = threading.Event()
    worker._restart_vcam.set()  # waiting for the retry is over at once
    worker._run()


def no_first_frame(win, worker):
    import threading
    worker._open_cap = lambda: FakeReader(worker, frames=False, stop_on="read")
    worker._restart_vcam = threading.Event()
    worker._restart_vcam.set()
    worker._run()


def dropped(win, worker):
    import threading
    done = threading.Event()
    done.set()
    worker._reconnect_cap(done)


def reconnected(win, worker):
    import threading
    stop = threading.Event()

    class Back(FakeReader):
        def read_packet(self):
            stop.set()
            return True, b"packet"
    worker._reconnect_cap = lambda _stop: Back(worker)
    worker._read_packets(FakeReader(worker, frames=False), stop)


def crashed(win, worker):
    # The worker says it, then that it ended: only the first is what a person can read
    seen = []
    worker.status.disconnect(win._on_worker_status)
    worker.status.connect(lambda kind, msg: seen.append((kind, msg)))

    def fail():
        raise RuntimeError("Invalid data found when processing input")
    from telescope.stream import StreamWorker
    worker._run = fail
    StreamWorker.run(worker)
    worker.status.slots.clear()
    worker.status.connect(win._on_worker_status)
    kind, msg = seen[0]
    win._on_worker_status(kind, msg, win._selected_source())


STREAM_STATES = (
    ("connecting", "main", "connecting to the phone's video", connecting, {}),
    ("loading-video", "main", "getting the video decoder ready", loading_video, {}),
    ("streaming", "main", "streaming over Wi-Fi", streams, {}),
    ("streaming-usb", "main", "streaming over the cable", streams, {"route": "usb"}),
    ("no-stream", "states", "can't reach the phone's video", unreachable_stream, {}),
    ("no-first-frame", "states", "waiting for the first frame", no_first_frame, {}),
    ("dropped", "states", "the video dropped, reconnecting", dropped, {}),
    ("back", "states", "the video is back after a drop", reconnected, {}),
    ("crashed", "states", "the stream worker hit an error", crashed, {}),
    ("camera-error", "states", "the virtual camera failed", virtual_camera_fails, {}),
)

for _id, _section, _title, _part, _options in STREAM_STATES:
    def _build(stack, part=_part, options=_options):
        from telescope.phones import Route
        route = Route("usb", "127.0.0.1", USB_SERIAL) if options.get("route") == "usb" else None
        return worker_scene(stack, part, route=route)
    scene(f"stream-{_id}", _section, f"Stream · {_title}")(_build)


# ── A phone that says something: banners from the phone's own state ─────────

def source_scene(stack, part, **options):
    """The window with a stream running, after part(window, the phone's source) told it something about the phone."""
    from PyQt6.QtWidgets import QApplication
    win = start_flow(stack, **options)
    part(win, win._selected_source())
    QApplication.processEvents()
    return picture(win)


def camera_taken(win, source):
    source.on_phone_state({"camera_toggle": True, "camera_taken": True})


def very_hot(fps):
    def part(win, source):
        state = {"camera_toggle": True, "heat_width": 1280, "heat_height": 720}
        source.on_phone_state({**state, "heat_fps": 24} if fps else state)
    return part


def cannot_turn_off(win, source):
    source.on_phone_state({"camera_toggle": False})


def stays_off(code):
    def part(win, source):
        state = {"camera_off": True, "camera_error": "Camera is being used by another application"}
        source._on_camera_check(source.session.id, {**state, "camera_error_code": code} if code else state)
    return part


def unreachable_phone_banner(win, source):
    win._flow.begin_recovery(source)
    source._show_unreachable()


def unreachable_browser_banner(win, source):
    browser = win._sources["browser"]
    browser.recovering, browser.session = True, source.session
    browser._show_unreachable()


def recovery(answer, first=False):
    def part(win, source):
        source = win._source_by_id("phone-1") if first else source
        win._flow.begin_recovery(source)
        source._on_recovery_probed(source.session.id, source._recovery_gen, answer())
    return part


def other_computer_took_it():
    from telescope.phones import READY, Resolution, Route
    return Resolution(READY, Route("wifi", WIFI_IP), streaming=True, streaming_for="Office PC")


def stopped_on_the_phone():
    from telescope.phones import READY, Resolution, Route
    return Resolution(READY, Route("wifi", WIFI_IP), streaming=False, busy=False)


def phone_forgot_us():
    from telescope.phones import NOT_PAIRED, Resolution
    return Resolution(NOT_PAIRED)


def phone_only_takes_usb():
    from telescope.phones import LOCAL_ONLY, Resolution
    return Resolution(LOCAL_ONLY)


PHONE_SAYS = (
    ("camera-taken", "another app on the phone has the camera", camera_taken, {}),
    ("hot", "very hot, smaller picture and slower", very_hot(True), {}),
    ("hot-size", "very hot, smaller picture", very_hot(False), {}),
    ("no-camera-switch", "phone can't turn its camera off", cannot_turn_off, {"camera_off": True}),
    ("camera-busy", "camera didn't turn back on, in use", stays_off("camera_in_use"), {}),
    ("camera-error", "camera didn't turn back on, phone's own words", stays_off(None), {}),
    ("gone-phone", "phone can't be reached for a while", unreachable_phone_banner, {}),
    ("gone-browser", "browser can't be reached for a while", unreachable_browser_banner, {}),
    ("took-by-other", "another computer took the phone", recovery(other_computer_took_it), {}),
    ("stopped-on-phone", "the phone stopped streaming", recovery(stopped_on_the_phone), {}),
    ("forgot-us", "phone no longer accepts this computer", recovery(phone_forgot_us), {}),
    ("usb-only-now", "phone now only takes USB", recovery(phone_only_takes_usb), {}),
)

PHONE_SAYS += (
    ("took-by-other-2", "another computer took one of two phones", recovery(other_computer_took_it, first=True), {"second": True}),
    ("stopped-on-phone-2", "one of two phones stopped streaming", recovery(stopped_on_the_phone, first=True), {"second": True}),
)

for _id, _title, _part, _options in PHONE_SAYS:
    scene(f"phone-says-{_id}", "states", f"Stream · {_title}")(
        lambda stack, part=_part, options=_options: source_scene(stack, part, **options))


# ── Connection card: changing the route and plugging in while streaming ──────

def route_choice(stack, now, was, then):
    """Connect via changed mid-stream to a way the phone doesn't answer on: the stream stays and a banner says why."""
    from unittest import mock
    from telescope.phones import UNREACHABLE, Resolution, Route
    route = Route("usb", "127.0.0.1", USB_SERIAL) if now == "usb" else None
    win = start_flow(stack, route=route)
    streams(win, win._session.worker)
    plugin = next(p for p in win._plugins if p.name == "connection")
    plugin._route_pref = was
    probe = lambda probe_id, phone, preference: plugin._on_route_probed(  # noqa: E731
        probe_id, phone.id, preference, Resolution(UNREACHABLE))  # the answer comes at once
    stack.enter_context(mock.patch.object(plugin, "_spawn_route_probe", probe))
    plugin.set_route_preference(then)
    return picture(win)


for _id, _title, _now, _was, _then in (
        ("usb", "can't switch to USB while streaming over Wi-Fi", "wifi", "auto", "usb"),
        ("wifi", "can't switch to Wi-Fi while streaming over the cable", "usb", "auto", "wifi"),
        ("auto", "can't go back to Automatic", "wifi", "usb", "auto")):
    scene(f"connection-route-{_id}", "states", f"Connection · {_title}")(
        lambda stack, now=_now, was=_was, then=_then: route_choice(stack, now, was, then))


def cable_plugged_in(stack):
    """The phone was plugged in while it streams over Wi-Fi and moving over by itself didn't work: the card offers it."""
    from telescope.phones import Route
    import telescope.sources as sources
    win = start_flow(stack, patches=[(sources.PhoneSource, "hand_over", lambda self, route: False)])
    streams(win, win._session.worker)
    plugin = next(p for p in win._plugins if p.name == "connection")
    plugin._on_usb_available(plugin._watch_id, Route("usb", "127.0.0.1", USB_SERIAL))
    return picture(win)


scene("connection-switch-to-usb", "states", "Connection · cable plugged in while streaming over Wi-Fi")(cable_plugged_in)


# ── Connection card: updating the phone's app over the cable ─────────────────

class SyncThread:
    """A thread that runs its job where it is started, so a scene needn't wait for one."""

    def __init__(self, target=None, args=(), kwargs=None, daemon=None):
        self.target, self.args, self.kwargs = target, args, kwargs or {}

    def start(self):
        self.target(*self.args, **self.kwargs)


def updating(stack, install):
    """The card after Update over USB was pressed, with install standing in for adb putting the app on the phone."""
    import logging
    from types import SimpleNamespace
    from unittest import mock
    import telescope.plugins.connection as connection
    from telescope.phones import PHONE_OUTDATED, Resolution, Route
    with_adb(stack)
    stack.enter_context(mock.patch.object(connection, "threading", SimpleNamespace(Thread=SyncThread)))
    stack.enter_context(mock.patch.object(connection, "adb_install", install))
    stack.enter_context(mock.patch.object(logging.getLogger(connection.__name__), "exception", lambda *a, **k: None))
    win, plugin = connection_window(stack)
    plugin._apply_resolution(Resolution(PHONE_OUTDATED, Route("usb", "127.0.0.1", USB_SERIAL)))
    plugin._check_status = lambda *a, **k: None
    frozen(plugin._update_action)
    return picture(win)


def install_freezes(_serial, _apk):
    raise Freeze


def install_fails(_serial, _apk):
    raise OSError("device offline")


scene("connection-updating-phone", "states", "Connection · updating the phone's app over the cable")(
    lambda stack: updating(stack, install_freezes))
scene("connection-update-failed", "states", "Connection · the phone's app couldn't be updated")(
    lambda stack: updating(stack, install_fails))
scene("connection-updated", "states", "Connection · the phone's app was updated")(
    lambda stack: updating(stack, lambda _serial, _apk: (True, "")))


# ── Connection card: with no phone ───────────────────────────────────────────

def browser_card(stack, state):
    """Browser camera picked instead of a phone: the card says what it waits for, by the plugin's own hooks."""
    win, plugin = connection_window(stack)
    plugin.select("browser")
    if state != "idle":
        plugin.on_stream_start("http://localhost/video", None)
    if state == "live":
        plugin._on_stream_connected("browser")
    return picture(win)


for _state, _title in (("idle", "Browser camera picked, no phone needed"), ("waiting", "waiting for the browser"),
                       ("live", "streaming from the browser")):
    scene(f"connection-browser-{_state}", "states", f"Connection · {_title}")(
        lambda stack, state=_state: browser_card(stack, state))


# ── The virtual camera is not ready ──────────────────────────────────────────

def virtual_camera(stack, loaded, interactive, setup=None):
    """Start on Linux with no virtual camera, by the connection plugin's own set-up step."""
    from unittest import mock
    import telescope.plugins.connection as connection
    for name, value in (("v4l2_devices_ready", lambda: False), ("v4l2_module_loaded", lambda: loaded)):
        stack.enter_context(mock.patch.object(connection, name, value))
    win, plugin = connection_window(stack)
    if setup is not None:
        stack.enter_context(mock.patch.object(connection, "v4l2_setup", setup))
        stack.enter_context(mock.patch.object(plugin, "_ask_virtual_camera", lambda: True))
    plugin._ensure_virtual_camera(interactive)
    return picture(win)


def needs_a_terminal(_persist):
    from telescope.platform.linux import NO_PROMPT, PrivResult
    from telescope.i18n import tr
    return PrivResult(False, tr(NO_PROMPT), "sudo modprobe v4l2loopback devices=1 video_nr=10 card_label=Telescope exclusive_caps=1")


def setup_failed(_persist):
    from telescope.platform.linux import PrivResult
    return PrivResult(False, "modprobe: FATAL: Module v4l2loopback not found in directory /lib/modules")


scene("connection-vcam-other-setup", "states", "Virtual camera · set up by another app")(
    lambda stack: virtual_camera(stack, True, True))
scene("connection-vcam-off", "states", "Virtual camera · off, and a start nobody asked for")(
    lambda stack: virtual_camera(stack, False, False))
scene("connection-vcam-terminal", "states", "Virtual camera · needs a terminal to be set up")(
    lambda stack: virtual_camera(stack, False, True, needs_a_terminal))
scene("connection-vcam-failed", "states", "Virtual camera · couldn't be set up")(
    lambda stack: virtual_camera(stack, False, True, setup_failed))


# ── Dialogs: adding a phone ──────────────────────────────────────────────────

def pairing_dialog(stack, adb=True, windows=False, states=(), network=True, downloading=False):
    """The Add phone dialog with the pairing server, adb and what adb sees as given; the USB check answers at once."""
    from types import SimpleNamespace
    from unittest import mock
    import telescope.plugins.connection as connection
    from telescope.widgets.adb import AdbDownload

    class Server:
        def __init__(self, **_kwargs):
            self.offer = SimpleNamespace(candidates=["192.168.1.20"] if network else [], payload="telescope://pair/example",
                                         usb_payload="telescope://pair/usb", port=47000)

        def start(self):
            return self.offer

        def stop(self):
            pass

    use_scratch_profile(stack)
    with_adb(stack, adb, windows)
    nothing = lambda *_a, **_k: None  # noqa: E731
    for target, name, value in (
            (connection, "adb_device_models", lambda: list(states)), (connection, "PairingServer", Server),
            (connection, "adb_reverse", nothing), (connection, "adb_broadcast_pair", nothing),
            (connection.AddPhoneDialog, "_poll_usb", lambda self: self._on_usb_state(connection.adb_device_models()))):
        stack.enter_context(mock.patch.object(target, name, value))
    stack.enter_context(mock.patch.object(AdbDownload, "running", property(lambda self: downloading, lambda self, value: None),
                                          create=True))
    mapped_tags(stack)
    return shown(stack, connection.AddPhoneDialog(None, "computer-id", "Studio PC", lambda *_args: None))


def adb_download_failed(stack):
    dialog = pairing_dialog(stack, adb=False, windows=True)
    dialog._on_adb_downloaded(False, "Couldn't download adb. Try again.")
    return shown(stack, dialog)


def adb_picked(stack):
    from unittest import mock
    from telescope.plugins.connection import PICK_ADB_LINK
    dialog = pairing_dialog(stack, adb=False, windows=True)
    stack.enter_context(mock.patch.object(type(dialog._adb_download), "pick_and_start",
                                          lambda self, window, starting=None: starting() or True))
    dialog._get_adb(PICK_ADB_LINK)
    return shown(stack, dialog)


def usb_picked(stack):
    dialog = pairing_dialog(stack, states=[("R5CT123ABC", "device", "Pixel 8")])
    dialog._pick("R5CT123ABC")
    return shown(stack, dialog)


for _id, _title, _build in (
        ("no-network", "this computer isn't on a network", lambda stack: pairing_dialog(stack, network=False)),
        ("windows-no-adb", "Windows without adb", lambda stack: pairing_dialog(stack, adb=False, windows=True)),
        ("getting-adb", "getting adb from Google", lambda stack: pairing_dialog(stack, adb=False, windows=True, downloading=True)),
        ("copying-adb", "copying the adb the person picked", adb_picked),
        ("adb-failed", "getting adb failed", adb_download_failed),
        ("usb-unauthorized", "phone plugged in, USB debugging not allowed",
         lambda stack: pairing_dialog(stack, states=[("R5CT123ABC", "unauthorized", "")])),
        ("usb-pick", "phones plugged in, waiting for one to be picked",
         lambda stack: pairing_dialog(stack, states=[("R5CT123ABC", "device", "Pixel 8"), ("9B07", "device", "SM-S911B")])),
        ("usb-pairing", "phone plugged in and picked, pairing over the cable", usb_picked),
        ("usb-unplugged", "nothing plugged in", lambda stack: pairing_dialog(stack, states=[]))):
    scene(f"dialog-add-phone-{_id}", "dialogs", f"Add phone · {_title}")(_build)


# ── Dialogs: your phones, and questions the connection plugin asks ───────────

def phones_window(stack, phones, browsers=(), computer_name=None, hostname=None):
    """The window with these phones paired, and its Connection plugin; browsers: (id, name, detail) with settings of their own."""
    from types import SimpleNamespace
    from unittest import mock
    import telescope.plugins.connection as connection
    if hostname is not None:  # a computer that gives no name, so the plugin falls back to its own word
        stack.enter_context(mock.patch.object(connection, "socket", SimpleNamespace(gethostname=lambda: hostname)))
    win, plugin = connection_window(stack, phone=False)
    config = plugin.get_config()
    config.pop("computer_name")  # set_config would turn the plugin's own fallback word into plain text
    plugin.set_config({**config, "selected_phone": phones[0]["id"] if phones else None, "phones": phones,
                       **({"computer_name": computer_name} if computer_name else {})})
    plugin._on_remembered(list(browsers))
    return win, plugin


def pixel(ips=(WIFI_IP,), active=WIFI_IP):
    return {"id": "phone-1", "name": "Pixel 8", "token": "token", "ips": list(ips), "active_ip": active}


def galaxy():
    return {"id": "phone-2", "name": "Galaxy S23", "token": "token", "ips": [], "active_ip": None}


def phones_dialog_open(stack, phones, browsers=(), **options):
    win, plugin = phones_window(stack, phones, browsers, **options)
    plugin.open_phones()
    return shown(stack, plugin._phones_dlg)


scene("dialog-phones-with-browser", "dialogs", "Your phones · a phone, one on another network and a browser")(
    lambda stack: phones_dialog_open(stack, [pixel(), galaxy()], [("browser:x", "Chrome on Android", "Browser")]))
scene("dialog-phones-none", "dialogs", "Your phones · none paired yet")(lambda stack: phones_dialog_open(stack, []))
scene("dialog-phones-unnamed-computer", "dialogs", "Your phones · a computer with no name")(
    lambda stack: phones_dialog_open(stack, [pixel()], hostname=""))


def phones_question(stack, part, **options):
    """A question or box the Your phones dialog asks, kept open for the picture."""
    held = hold_dialogs(stack)
    win, plugin = phones_window(stack, [pixel(), galaxy()], [("browser:x", "Chrome on Android", "Browser")], **options)
    plugin.open_phones()
    stack.callback(plugin._phones_dlg.close)
    part(plugin._phones_dlg)
    return shown(stack, held[-1])


scene("dialog-phones-remove-browser", "dialogs", "Your phones · remove a browser")(
    lambda stack: phones_question(stack, lambda dialog: dialog._remove("browser:x")))
scene("dialog-phones-rename-computer", "dialogs", "Your phones · rename this computer")(
    lambda stack: phones_question(stack, lambda dialog: dialog._rename_computer()))


def same_phone(stack):
    held = hold_dialogs(stack)
    win, plugin = phones_window(stack, [pixel()])
    plugin._ask_same_phone("Pixel 8")
    return shown(stack, held[-1])


def virtual_camera_consent(stack):
    held = hold_dialogs(stack)
    win, plugin = connection_window(stack)
    plugin._ask_virtual_camera()
    return shown(stack, held[-1])


scene("dialog-same-phone", "dialogs", "Same phone? · a phone paired again with a new identity")(same_phone)
scene("dialog-vcam-consent", "dialogs", "Set up the virtual camera · asking before the password")(virtual_camera_consent)
