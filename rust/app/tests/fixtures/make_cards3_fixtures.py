"""Records what the Python app does for Presets, Startup and the Wait screen, for tests/cards3_parity.rs to compare the Rust plugins with.

    cd desktop && QT_QPA_PLATFORM=offscreen python ../rust/app/tests/fixtures/make_cards3_fixtures.py

Writes next to this file: cards3_configs.json, what each plugin's get_config answers after set_config for odd settings (null when
set_config refuses them).
"""
import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parents[3] / "desktop"))

from PyQt6.QtWidgets import QApplication  # noqa: E402

from telescope.plugin import EventBus  # noqa: E402
from telescope.plugins.presets import PresetsPlugin  # noqa: E402
from telescope.plugins.startup import StartupPlugin  # noqa: E402
from telescope.plugins.wait_screen import WaitScreenPlugin  # noqa: E402

_APP = QApplication.instance() or QApplication([])


class _Host:
    """Just enough host for these plugins to load settings."""

    def set_keep_in_tray(self, _keep):
        pass

    def schedule_save(self):
        pass

    def is_streaming(self):
        return False

    def is_camera_on(self):
        return True

    def stream_count(self):
        return 0

    def canvas_size(self):
        return (None, None)


class _Screen:
    def show(self, *_a, **_k):
        pass

    def stop(self):
        pass

    def set_watched(self, _w):
        pass


class _Watch:
    watched = False

    def __init__(self, _on_change):
        pass

    def start(self):
        pass

    def stop(self):
        pass


def presets():
    plugin = PresetsPlugin()
    return plugin, plugin.setup(_Host(), EventBus())


def startup():
    plugin = StartupPlugin()
    plugin.setup(_Host(), EventBus())
    return plugin


def wait_screen():
    plugin = WaitScreenPlugin(screen=_Screen(), watch_cls=_Watch)
    plugin.setup(_Host(), EventBus())
    return plugin


LONG = "x" * 100

CASES = {
    "presets": [
        {},
        {"presets": []},
        {"presets": "not a list"},
        {"presets": None},
        {"presets": [{"name": "Desk", "camera": {"iso": 100, "lens": "0"}, "stream": {"fps": 30}, "transforms": {"zoom": 2.0}}]},
        {"presets": [{"name": "A", "camera": {"iso": 1}, "stream": "bad"}, {"name": ""}, "junk", {"name": "A"}, {"name": "B", "extra": 1}]},
        {"presets": [{"name": "  Desk  & <b>window</b> "}, {"name": "Des\x00k\n<b>"}, {"name": "\x00\x01"}]},
        {"presets": [{"name": LONG}, {"name": 5}, {"name": None}, {"name": 1.5}, {"name": True}, {"x": 1}]},
        {"presets": [{"name": "Night", "background": {"mode": "picture", "picture": "desk.png"}, "gone": {"x": 1}, "junk": 3}]},
        {"presets": [{"name": "A", "camera": {"b": 1, "a": [1, 2.5, None]}, "stream": {}}]},
        {"presets": [{"name": "Tab\there"}, {"name": "Ni\x07ght\t" + LONG}]},
    ],
    "startup": [
        {},
        {"start_on": "ready", "stop_idle": "own", "stop_delay_s": 60, "notify_stop": False},
        {"start_on": "watched", "stop_idle": "any", "stop_delay_s": 300, "notify_stop": True},
        {"start_on": "sometimes", "stop_idle": None, "stop_delay_s": "5", "notify_stop": "no"},
        {"stop_idle": "own", "stop_delay_s": 2, "auto_stream": True},
        {"start_on": "ready", "stop_delay_s": 99999},
        {"start_on": "ready", "stop_delay_s": True},
        {"start_on": "ready", "stop_delay_s": 30.0},
        {"start_on": "ready", "stop_delay_s": -4},
        {"start_on": 3, "stop_idle": ["own"]},
        {"auto_stream": True},
        {"watch_stream": True},
        {"auto_stream": True, "watch_stream": True},
        {"auto_stream": "yes", "watch_stream": 1},
        {"start_on": "off"},
        {"stop_idle": "any"},
        {"notify_stop": 0},
        {"notify_stop": None},
    ],
    "wait_screen": [
        {},
        {"image": "/x/wait_screen-abc.png", "mirror": True, "last_size": [640, 480]},
        {"image": 5},
        {"image": ""},
        {"image": None, "mirror": 1},
        {"mirror": "yes"},
        {"mirror": False},
        {"last_size": [8, 8]},
        {"last_size": [640]},
        {"last_size": [True, 480]},
        {"last_size": "640x480"},
        {"last_size": [640.0, 480]},
        {"last_size": [640, 480, 1]},
        {"last_size": [16, 8192]},
        {"last_size": [15, 8192]},
        {"last_size": [16, 8193]},
        {"last_size": None},
    ],
}


def record() -> list:
    out = []
    for name, cases in CASES.items():
        for cfg in cases:
            plugin = {"presets": lambda: presets()[0], "startup": startup, "wait_screen": wait_screen}[name]()
            try:
                plugin.set_config(cfg)
                got = plugin.get_config()
            except Exception:
                got = None
            out.append({"plugin": name, "input": cfg, "output": got})
    return out


if __name__ == "__main__":
    cases = record()
    (HERE / "cards3_configs.json").write_text(json.dumps(cases, indent=1, ensure_ascii=False) + "\n", encoding="utf-8")
    print(f"{len(cases)} cases")
