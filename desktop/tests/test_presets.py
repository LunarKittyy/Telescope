from PyQt6.QtWidgets import QPushButton
import pytest

from telescope.plugin import EventBus, TelescopePlugin
from telescope.plugins.camera_control import CameraControlPlugin
from telescope.plugins.presets import PresetsPlugin, clean_presets
from telescope.plugins.stream_output import StreamOutputPlugin
from telescope.plugins.transforms import TransformsPlugin


class _Host:
    """Just enough host for the plugins here, with plugin_config/apply_preset over real plugins."""

    def __init__(self):
        self.saves = 0
        self.plugins = {}
        self.applied = []
        self.fps = []

    def schedule_save(self):
        self.saves += 1

    def plugin_config(self, name):
        p = self.plugins.get(name)
        return p.get_config() if p else None

    def apply_preset(self, name, cfg):
        self.applied.append(name)
        if name in self.plugins:
            self.plugins[name].apply_preset(cfg)

    def update_stream_output(self, fps=None, **_kw):
        self.fps.append(fps)


class _Ctrl:
    def __init__(self):
        self.sent = []

    def send(self, **params):
        self.sent.append(params)


class _Stub(TelescopePlugin):
    def __init__(self, name, cfg):
        self.name, self.cfg = name, dict(cfg)

    def get_config(self):
        return dict(self.cfg)

    def set_config(self, cfg):
        self.cfg = dict(cfg)


def _presets(host):
    plugin = PresetsPlugin()
    plugin.setup(host, EventBus())
    return plugin


_CAMS = [
    {"id": "0", "label": "Back camera", "current": True,
     "supportedSizes": [{"width": 1920, "height": 1080}, {"width": 1280, "height": 720}]},
    {"id": "2", "label": "Back camera (wide)", "current": False,
     "supportedSizes": [{"width": 1920, "height": 1080}, {"width": 1280, "height": 720}]},
]


def test_save_snapshots_each_section_and_replaces_by_name(qapp):
    host = _Host()
    host.plugins = {"camera_control": _Stub("camera_control", {"iso": 100}),
                    "transforms": _Stub("transforms", {"zoom": 2.0})}
    plugin = _presets(host)

    plugin.save("  Desk  ")
    host.plugins["camera_control"].cfg = {"iso": 800}
    plugin.save("Night")
    host.plugins["camera_control"].cfg = {"iso": 200}
    plugin.save("Desk")

    assert plugin.names() == ["Desk", "Night"]
    assert plugin.find("Desk") == {"name": "Desk", "camera": {"iso": 200}, "transforms": {"zoom": 2.0}}
    assert plugin.find("Night")["camera"] == {"iso": 800}
    assert host.saves == 3


def test_blank_names_are_ignored(qapp):
    host = _Host()
    plugin = _presets(host)
    plugin.save("   ")
    assert plugin.names() == [] and host.saves == 0


def test_apply_hands_sections_to_the_host_camera_first(qapp):
    host = _Host()
    stubs = {n: _Stub(n, {}) for n in ("camera_control", "stream_output", "transforms")}
    host.plugins = stubs
    plugin = _presets(host)
    plugin.set_config({"presets": [
        {"name": "A", "transforms": {"zoom": 3.0}, "stream": {"fps": 24}, "camera": {"iso": 400}},
    ]})

    plugin.apply("A")
    plugin.apply("missing")

    assert host.applied == ["camera_control", "stream_output", "transforms"]
    assert stubs["camera_control"].cfg == {"iso": 400}
    assert stubs["stream_output"].cfg == {"fps": 24}
    assert stubs["transforms"].cfg == {"zoom": 3.0}


def test_rename_and_delete(qapp):
    host = _Host()
    plugin = _presets(host)
    plugin.set_config({"presets": [{"name": "A"}, {"name": "B"}, {"name": "C"}]})

    plugin.rename("A", "Desk")
    plugin.rename("B", "C")  # takes over the name; the old C goes
    plugin.delete("Desk")
    plugin.delete("nope")

    assert plugin.names() == ["C"]
    assert plugin.get_config() == {"presets": [{"name": "C"}]}


def test_clean_presets_drops_malformed_and_duplicate_entries():
    raw = [{"name": "A", "camera": {"iso": 1}, "stream": "bad"}, {"name": ""}, "junk",
           {"name": "A"}, {"name": "B", "extra": 1}]
    assert clean_presets(raw) == [{"name": "A", "camera": {"iso": 1}}, {"name": "B"}]
    assert clean_presets(None) == []


def test_menu_lists_presets_then_save_rename_delete(qapp):
    host = _Host()
    plugin = _presets(host)
    plugin.create_header_widget()
    empty = plugin.build_menu()
    labels = [a.text() for a in empty.actions()]
    assert labels == ["Save current as…", "Rename", "Delete"]
    assert not empty.actions()[1].isEnabled()

    plugin.set_config({"presets": [{"name": "A"}, {"name": "B"}]})
    menu = plugin.build_menu()
    labels = [a.text() for a in menu.actions() if not a.isSeparator()]
    assert labels == ["A", "B", "Save current as…", "Rename", "Delete"]
    delete = menu.actions()[-1].menu()
    delete.actions()[0].trigger()
    assert plugin.names() == ["B"]
    assert isinstance(plugin._btn, QPushButton)


_PANELS = []  # the panels own the widgets; keep them alive for the test


def _camera(host, bus):
    plugin = CameraControlPlugin()
    plugin.setup(host, bus)
    _PANELS.append(plugin.create_panel())
    host.plugins["camera_control"] = plugin
    return plugin


def _stream(host, bus):
    plugin = StreamOutputPlugin()
    plugin.setup(host, bus)
    _PANELS.append(plugin.create_panel())
    host.plugins["stream_output"] = plugin
    return plugin


def test_camera_saves_the_lens_and_applying_switches_it_before_sending_settings(qapp):
    host, bus = _Host(), EventBus()
    cam = _camera(host, bus)
    ctrl = _Ctrl()
    cam.on_stream_start("url", ctrl)
    cam.on_phone_state({"cameras": _CAMS, "auto": True})
    assert cam.get_config()["lens"] == "0"
    saved = dict(cam.get_config(), lens="2", exp_manual=True, iso=400, shutter_ns=10_000_000)
    ctrl.sent.clear()

    cam.apply_preset(saved)

    assert ctrl.sent[0] == {"action": "camera", "id": "2"}
    iso = next(m["value"] for m in ctrl.sent if m["action"] == "iso")
    assert iso == pytest.approx(400, rel=0.02)  # the log slider's step
    assert {"action": "auto"} not in ctrl.sent
    assert cam.get_config()["lens"] == "2"
    assert cam._rb_exp_manual.isChecked()


def test_camera_preset_for_the_live_lens_sends_no_switch(qapp):
    host, bus = _Host(), EventBus()
    cam = _camera(host, bus)
    ctrl = _Ctrl()
    cam.on_stream_start("url", ctrl)
    cam.on_phone_state({"cameras": _CAMS, "auto": True})
    ctrl.sent.clear()

    cam.apply_preset(dict(cam.get_config(), lens="0"))
    cam.apply_preset(dict(cam.get_config(), lens="gone"))

    assert all(m["action"] != "camera" for m in ctrl.sent)
    assert {"action": "auto"} in ctrl.sent


def test_camera_preset_while_idle_only_loads(qapp):
    host, bus = _Host(), EventBus()
    cam = _camera(host, bus)
    cam.apply_preset(dict(cam.get_config(), wb_manual=True, wb_kelvin=3200, lens="2"))
    assert cam._rb_wb_manual.isChecked()
    assert cam.get_config()["lens"] == "2"


def test_stream_output_preset_sends_fps_quality_and_resolution(qapp):
    host, bus = _Host(), EventBus()
    out = _stream(host, bus)
    ctrl = _Ctrl()
    out.on_stream_start("url", ctrl)
    out.on_phone_state({"cameras": _CAMS, "stream_width": 1920, "stream_height": 1080})
    ctrl.sent.clear()

    out.apply_preset({"fps": 24, "jpeg_quality": 60, "resolution": "1280×720"
                      if out._find_by_label("1280×720") else out._res_combo.itemText(1)})

    actions = [m["action"] for m in ctrl.sent]
    assert {"action": "fps_target", "value": 24} in ctrl.sent
    assert {"action": "jpeg_quality", "value": 60} in ctrl.sent
    assert actions[-1] == "resolution"
    assert (ctrl.sent[-1]["width"], ctrl.sent[-1]["height"]) == (1280, 720)
    assert host.fps[-1] == 24


def test_full_round_trip_follows_the_lens_switch(qapp):
    host, bus = _Host(), EventBus()
    cam, out = _camera(host, bus), _stream(host, bus)
    tr = TransformsPlugin()
    tr.setup(host, bus)
    _PANELS.append(tr.create_panel())
    host.plugins["transforms"] = tr
    presets = _presets(host)
    ctrl = _Ctrl()
    for p in (cam, out):
        p.on_stream_start("url", ctrl)
    state = {"cameras": _CAMS, "auto": True, "stream_width": 1920, "stream_height": 1080}
    cam.on_phone_state(state)
    out.on_phone_state(state)

    tr.set_config({"zoom": 2.0, "flip_h": True})
    presets.save("Wide")
    tr.set_config({})
    presets.find("Wide")["camera"]["lens"] = "2"
    ctrl.sent.clear()

    presets.apply("Wide")

    assert ctrl.sent[0] == {"action": "camera", "id": "2"}
    assert out._current_camera_id == "2"
    assert tr.get_config()["zoom"] == pytest.approx(2.0)
    assert tr.get_config()["flip_h"] is True
