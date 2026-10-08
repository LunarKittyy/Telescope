"""The wait screen plugin: when it holds the camera, at what size, and the image it shows."""

from pathlib import Path

import pytest
from PyQt6.QtCore import QCoreApplication

import telescope.plugins.wait_screen as wait_screen_module
import telescope.vcam as vcam
from telescope.plugin import EventBus
from telescope.plugins.setup import CANVAS_PRESETS
from telescope.plugins.wait_screen import WaitScreenDialog, WaitScreenPlugin
from test_app import camera_env, window  # noqa: F401 (the fixtures)


class _Host:
    def __init__(self):
        self.streaming = False
        self.camera_on = True
        self.setup_cfg = {}
        self.saves = 0

    def is_streaming(self):
        return self.streaming

    def is_camera_on(self):
        return self.camera_on

    def plugin_config(self, name):
        return self.setup_cfg if name == "setup" else None

    def schedule_save(self):
        self.saves += 1


class _Screen:
    def __init__(self):
        self.shown = []
        self.stops = 0
        self.watched = []
        self.mirrored = []

    def show(self, size, path, mirror=False):
        self.shown.append((size, path))
        self.mirrored.append(mirror)

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


def test_extra_cameras_show_it_while_nothing_streams_to_them(qapp, config_home):
    host, bus, made = _Host(), EventBus(), {}
    plugin = WaitScreenPlugin(screen=_Screen(), watch_cls=_Watch,
                              extra_screen=lambda slot: made.setdefault(slot, _Screen()))
    plugin.setup(host, bus)
    bus.idle_outputs.emit([1, 2])
    assert made[1].shown == [(vcam.DEFAULT_SIZE, None)] and made[2].shown == [(vcam.DEFAULT_SIZE, None)]

    host.streaming = True  # the first camera streams; a second one is about to take camera 2
    bus.idle_outputs.emit([1])
    assert made[2].stops == 1 and made[1].stops == 0

    plugin.set_mirror(True)  # every screen picks it up
    assert made[1].mirrored[-1] is True and len(made[2].shown) == 1

    bus.idle_outputs.emit([1, 2])  # that stream stopped
    assert len(made[2].shown) == 2
    plugin.shutdown()
    assert made[1].stops == 1 and made[2].stops == 2


def test_the_main_camera_shows_it_while_only_extra_ones_stream(qapp, config_home):
    host, bus, screen = _Host(), EventBus(), _Screen()
    plugin = WaitScreenPlugin(screen=screen, watch_cls=_Watch, extra_screen=lambda slot: _Screen())
    plugin.setup(host, bus)
    host.streaming = True
    bus.idle_outputs.emit([2, 3])  # the main camera and camera 2 stream
    assert screen.shown == []
    bus.idle_outputs.emit([0, 2, 3])  # the main camera's stream stopped; camera 2's goes on
    assert len(screen.shown) == 1
    bus.idle_outputs.emit([0, 2, 3])
    assert len(screen.shown) == 1  # already up
    plugin.on_stream_starting()  # a stream takes the main camera again
    bus.idle_outputs.emit([2, 3])
    assert screen.stops == 1
    plugin.shutdown()


def test_takes_the_camera_back_while_the_stream_has_its_camera_off(env):
    plugin, host, _bus, screen = env
    plugin.on_stream_starting()
    host.streaming = True
    host.camera_on = False
    plugin.on_camera_off()
    assert len(screen.shown) == 2  # the call keeps a picture while the stream is just the mic
    plugin.on_stream_starting()  # the camera coming back on
    assert screen.stops == 2


def test_a_reader_is_passed_to_the_screen_and_the_bus(env, qapp):
    plugin, _host, bus, screen = env
    seen = []
    bus.camera_watched.connect(seen.append)
    plugin._watch.on_change(True)  # as from the watch thread
    qapp.processEvents()
    assert screen.watched == [True] and seen == [True]


def _kept(config_home):
    return sorted(p.name for p in config_home.config_path().parent.glob("wait_screen*"))


def test_a_chosen_image_is_kept_next_to_the_config(env, tmp_path, config_home):
    plugin, host, _bus, screen = env
    first = tmp_path / "first.PNG"
    first.write_bytes(b"one")
    plugin.set_image(str(first))
    kept = Path(plugin.image_path)
    assert kept.parent == config_home.config_path().parent and kept.suffix == ".png"
    assert kept.read_bytes() == b"one" and screen.shown[-1][1] == str(kept)

    second = tmp_path / "second.gif"
    second.write_bytes(b"two")
    plugin.set_image(str(second))
    assert not kept.exists()  # the old copy goes
    second_kept = Path(plugin.image_path)
    plugin.set_image(plugin.image_path)  # choosing the kept copy itself is fine
    assert plugin.image_path == str(second_kept) and second_kept.read_bytes() == b"two"
    assert _kept(config_home) == [second_kept.name]

    plugin.set_image(None)
    assert plugin.image_path is None and screen.shown[-1][1] is None
    assert host.saves == 4


def test_a_second_image_of_the_same_type_is_a_different_path(env, tmp_path, config_home):
    plugin, _host, _bus, screen = env
    for name, data in (("a.png", b"one"), ("b.png", b"two")):
        (tmp_path / name).write_bytes(data)
        plugin.set_image(str(tmp_path / name))
    first, second = (shown[1] for shown in screen.shown[-2:])
    assert first != second  # so the camera reloads instead of keeping the first picture
    assert Path(second).read_bytes() == b"two" and _kept(config_home) == [Path(second).name]


def test_a_failed_replacement_keeps_the_previous_image(env, tmp_path, config_home, monkeypatch):
    plugin, host, _bus, screen = env
    (tmp_path / "a.png").write_bytes(b"one")
    plugin.set_image(str(tmp_path / "a.png"))
    previous, saves, shows = plugin.image_path, host.saves, len(screen.shown)
    (tmp_path / "b.jpg").write_bytes(b"two")

    def full_disk(src, dst):
        Path(dst).write_bytes(b"tw")
        raise OSError(28, "No space left on device")

    monkeypatch.setattr(wait_screen_module.shutil, "copyfile", full_disk)
    assert "No space" in plugin.set_image(str(tmp_path / "b.jpg"))
    assert plugin.image_path == previous and Path(previous).read_bytes() == b"one"
    assert _kept(config_home) == [Path(previous).name]  # no half-written leftovers
    assert host.saves == saves and len(screen.shown) == shows


def test_the_dialog_says_when_a_pick_failed_or_wont_load(env, tmp_path, monkeypatch):
    plugin = env[0]
    dlg = WaitScreenDialog(plugin)
    monkeypatch.setattr(WaitScreenPlugin, "set_image", lambda self, path: "No space left on device")
    dlg._set(str(tmp_path / "x.png"))
    assert "No space left on device" in dlg._note.text() and "Keeping" in dlg._note.text()
    monkeypatch.undo()

    bad = tmp_path / "bad.png"
    bad.write_bytes(b"not an image")
    plugin.set_image(str(bad))
    dlg.refresh()
    assert "Couldn't read that image" in dlg._note.text()
    assert not dlg._preview.pixmap().isNull()  # the default screen stands in

    plugin.set_image(None)
    dlg.refresh()
    assert "Couldn't" not in dlg._note.text()


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
    assert plugin.get_config() == {"image": "/x/wait_screen.png", "mirror": False,
                                   "last_size": list(kept) if kept else None}


def test_config_rejects_a_bad_image(env):
    plugin = env[0]
    plugin.set_config({"image": 5})
    assert plugin.image_path is None


@pytest.mark.parametrize("value, mirror", [(True, True), (False, False), ("yes", False), (None, False)])
def test_config_keeps_mirror_only_when_it_is_really_on(env, value, mirror):
    plugin = env[0]
    plugin.set_config({"mirror": value})
    assert plugin.mirror is mirror and plugin.get_config()["mirror"] is mirror


def test_mirror_is_off_by_default_and_reshows_when_toggled(env):
    plugin, host, _bus, screen = env
    assert screen.mirrored == [False]
    plugin.set_mirror(True)
    assert screen.mirrored[-1] is True and host.saves == 1


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
    assert not dlg._mirror_box.isChecked()
    dlg._mirror_box.setChecked(True)
    assert plugin.mirror
    dlg.close()


def test_a_camera_off_stream_on_an_extra_camera_gets_its_wait_screen(camera_env, monkeypatch):
    import telescope.app as app_module
    window, _conn, mic, *_ = camera_env
    monkeypatch.setattr(app_module.vcam, "slot_ready", lambda _slot: True)
    mic.follows_focus = False
    extras = {}
    wait = WaitScreenPlugin(screen=_Screen(), watch_cls=_Watch,
                            extra_screen=lambda slot: extras.setdefault(slot, _Screen()))
    window.register_plugin(wait)
    QCoreApplication.processEvents()
    window._start()
    window.add_stream("phone-b")
    second = window._focus
    window.stop_stream("Phone")
    assert second.slot == 1
    mic.config["enabled"] = True
    shown = len(extras[1].shown)
    window.set_camera_on(False)
    assert 1 in wait._idle and len(extras[1].shown) > shown
    stops = extras[1].stops
    window.set_camera_on(True)
    assert 1 not in wait._idle and extras[1].stops > stops  # the camera has it back
    wait.shutdown()
