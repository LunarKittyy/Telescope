"""The wait screen plugin: when it holds the camera, at what size, and the image it shows."""

import pytest

import telescope.vcam as vcam
from telescope.plugin import EventBus
from telescope.plugins.setup import CANVAS_PRESETS
from telescope.plugins.wait_screen import WaitScreenDialog, WaitScreenPlugin


class _Host:
    def __init__(self):
        self.streaming = False
        self.setup_cfg = {}
        self.saves = 0

    def is_streaming(self):
        return self.streaming

    def plugin_config(self, name):
        return self.setup_cfg if name == "setup" else None

    def schedule_save(self):
        self.saves += 1


class _Screen:
    def __init__(self):
        self.shown = []
        self.stops = 0
        self.watched = []

    def show(self, size, path):
        self.shown.append((size, path))

    def stop(self):
        self.stops += 1

    def set_watched(self, watched):
        self.watched.append(watched)


class _Watch:
    def __init__(self, on_change):
        self.on_change = on_change
        self.started = self.stopped = False
        self.watched = False

    def start(self):
        self.started = True

    def stop(self):
        self.stopped = True


@pytest.fixture
def env(qapp, config_home):
    host, bus, screen = _Host(), EventBus(), _Screen()
    plugin = WaitScreenPlugin(screen=screen, watch_cls=_Watch)
    plugin.setup(host, bus)
    qapp.processEvents()  # the first show waits a turn for every plugin's config
    return plugin, host, bus, screen


def test_shows_the_default_screen_at_start_and_watches(env):
    plugin, _host, _bus, screen = env
    assert screen.shown == [(vcam.DEFAULT_SIZE, None)]
    assert plugin._watch.started


def test_size_follows_the_canvas_then_the_last_stream(env):
    plugin, host, bus, _screen = env
    bus.vcam_opened.emit(640, 480)
    assert plugin.size() == (640, 480) and host.saves == 1
    bus.vcam_opened.emit(640, 480)
    assert host.saves == 1
    label, dims = next((label, v) for label, v in CANVAS_PRESETS if isinstance(v, tuple))
    host.setup_cfg = {"canvas_preset": label}
    assert plugin.size() == dims


def test_gives_the_camera_to_the_stream_and_takes_it_back(env):
    plugin, host, _bus, screen = env
    plugin.on_stream_starting()
    assert screen.stops == 1
    host.streaming = True
    plugin._show()
    assert len(screen.shown) == 1  # never while streaming
    host.streaming = False
    plugin.on_stream_stop()
    assert len(screen.shown) == 2


def test_a_reader_is_passed_to_the_screen_and_the_bus(env, qapp):
    plugin, _host, bus, screen = env
    seen = []
    bus.camera_watched.connect(seen.append)
    plugin._watch.on_change(True)  # as from the watch thread
    qapp.processEvents()
    assert screen.watched == [True] and seen == [True]


def test_a_chosen_image_is_kept_next_to_the_config(env, tmp_path, config_home):
    plugin, host, _bus, screen = env
    first = tmp_path / "first.PNG"
    first.write_bytes(b"one")
    plugin.set_image(str(first))
    kept = config_home.config_path().parent / "wait_screen.png"
    assert kept.read_bytes() == b"one" and plugin.image_path == str(kept)
    assert screen.shown[-1][1] == str(kept)

    second = tmp_path / "second.gif"
    second.write_bytes(b"two")
    plugin.set_image(str(second))
    assert not kept.exists()  # the old copy goes
    plugin.set_image(plugin.image_path)  # choosing the kept copy itself is fine
    assert (kept.parent / "wait_screen.gif").read_bytes() == b"two"

    plugin.set_image(None)
    assert plugin.image_path is None and screen.shown[-1][1] is None
    assert host.saves == 4


@pytest.mark.parametrize("size, kept", [
    ([640, 480], (640, 480)),
    ([8, 8], None),
    ([640], None),
    ([True, 480], None),
    ("640x480", None),
    ([640.0, 480], None),
])
def test_config_round_trip_rejects_odd_sizes(env, size, kept):
    plugin = env[0]
    plugin.set_config({"image": "/x/wait_screen.png", "last_size": size})
    assert plugin.last_size == kept
    assert plugin.get_config() == {"image": "/x/wait_screen.png", "last_size": list(kept) if kept else None}


def test_config_rejects_a_bad_image(env):
    plugin = env[0]
    plugin.set_config({"image": 5})
    assert plugin.image_path is None


def test_shutdown_lets_go_for_good(env):
    plugin, _host, _bus, screen = env
    plugin.shutdown()
    assert screen.stops == 1 and plugin._watch.stopped
    plugin.on_stream_stop()
    assert len(screen.shown) == 1


def test_menu_opens_the_dialog(env):
    plugin = env[0]
    (action,) = plugin.create_menu_actions()
    assert action.text() == "Wait screen…"
    action.trigger()
    dlg = plugin._dlg
    assert isinstance(dlg, WaitScreenDialog) and dlg.isVisible()
    assert not dlg._default_btn.isEnabled()
    assert dlg._preview.pixmap() is not None and not dlg._preview.pixmap().isNull()
    dlg.close()
