"""Records what the Python app does for Setup and Monitoring, for tests/config_parity.rs to compare the Rust plugins with.

    cd desktop && QT_QPA_PLATFORM=offscreen python ../rust/crates/setup/tests/fixtures/make_setup_fixtures.py

Writes next to this file: setup_configs.json, what each plugin's get_config answers after set_config for odd settings (null when
set_config refuses them), and for Setup the canvas get_canvas_dims works out.
"""
import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parents[4] / "desktop"))

from PyQt6.QtWidgets import QApplication  # noqa: E402

from telescope import theme, vision  # noqa: E402
from telescope.plugin import EventBus  # noqa: E402
from telescope.plugins.monitoring import MonitoringPlugin  # noqa: E402
from telescope.plugins.setup import SetupPlugin  # noqa: E402

_APP = QApplication.instance() or QApplication([])


class _Host:
    def schedule_save(self):
        pass


def setup():
    plugin = SetupPlugin()
    plugin.setup(_Host(), EventBus())
    return plugin


def monitoring():
    plugin = MonitoringPlugin()
    plugin.setup(_Host(), EventBus())
    plugin.create_panel()
    return plugin


CASES = {
    "setup": [
        {},
        {"canvas_preset": "720p 16:9 - 1280 x 720"},
        {"canvas_preset": "1280 × 720"},
        {"canvas_preset": "Custom...", "custom_canvas_w": 1111, "custom_canvas_h": 777},
        {"canvas_preset": "Custom...", "custom_canvas_w": 7680, "custom_canvas_h": 4319},
        {"canvas_preset": "Custom...", "custom_canvas_w": 7679, "custom_canvas_h": 4320},
        {"canvas_preset": "Custom...", "custom_canvas_w": "1920", "custom_canvas_h": 1080},
        {"canvas_preset": "Custom...", "custom_canvas_w": 0, "custom_canvas_h": 720},
        {"canvas_preset": "Custom...", "custom_canvas_w": 63, "custom_canvas_h": 64},
        {"canvas_preset": "Custom...", "custom_canvas_w": 64, "custom_canvas_h": 4321},
        {"canvas_preset": "Custom...", "custom_canvas_w": 1920.0, "custom_canvas_h": 1080},
        {"canvas_preset": "Custom...", "custom_canvas_w": True, "custom_canvas_h": 1080},
        {"canvas_preset": 5},
        {"canvas_preset": None},
        {"max_zoom": 2}, {"max_zoom": 30}, {"max_zoom": 1}, {"max_zoom": 31}, {"max_zoom": "8"}, {"max_zoom": True}, {"max_zoom": 12.0},
        {"max_gain": 12}, {"max_gain": 48}, {"max_gain": 30}, {"max_gain": 24.0}, {"max_gain": "36"}, {"max_gain": True}, {"max_gain": None},
        {"limiter": False}, {"limiter": 0}, {"limiter": None}, {"limiter": "off"},
        {"hw_decode": True}, {"hw_decode": 1}, {"hw_decode": "yes"},
        {"power_saving": True}, {"power_saving": 1},
        {"plugin_hues": False}, {"plugin_hues": 0}, {"plugin_hues": None},
        {"language": "sv"}, {"language": 5}, {"language": None}, {"language": "xx"},
    ],
    "monitoring": [
        {},
        {"battery_alert": 30, "temp_alert": 50, "battery_notify": False, "battery_stop": True, "temp_notify": False, "temp_stop": True},
        {"battery_alert": 1, "temp_alert": 1},
        {"battery_alert": 100, "temp_alert": 100},
        {"battery_alert": 0, "temp_alert": 0},
        {"battery_alert": "40", "temp_alert": " 50 "},
        {"battery_alert": 30.9, "temp_alert": 50.9},
        {"battery_alert": True, "temp_alert": True},
        {"battery_alert": "lots"},
        {"temp_alert": "1.5"},
        {"battery_alert": [1]},
        {"temp_alert": {"x": 1}},
        {"battery_alert": None, "temp_alert": None},
        {"battery_alert": "", "temp_alert": []},
        {"battery_notify": 0, "temp_notify": None},
        {"battery_stop": "yes", "temp_stop": 1},
        {"battery_stop": True, "temp_stop": True},
    ],
}


def record() -> list:
    out = []
    for name, cases in CASES.items():
        for cfg in cases:
            plugin = {"setup": setup, "monitoring": monitoring}[name]()
            try:
                plugin.set_config(cfg)
                got = plugin.get_config()
            except Exception:
                got = None
            case = {"plugin": name, "input": cfg, "output": got}
            if name == "setup" and got is not None:
                try:
                    case["canvas"] = list(plugin.get_canvas_dims())
                except TypeError:  # a list as the preset can't be looked up
                    case["canvas"] = None
            out.append(case)
    theme.set_plugin_hues(True)
    vision.set_power_saving(False)
    return out


if __name__ == "__main__":
    cases = record()
    (HERE / "setup_configs.json").write_text(json.dumps(cases, indent=1, ensure_ascii=False) + "\n", encoding="utf-8")
    print(f"{len(cases)} cases")
