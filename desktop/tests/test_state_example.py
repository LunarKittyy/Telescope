"""docs/state.json is one /v1/state that the phone's StateExampleTest.kt checks it still sends; this checks the desktop still reads it."""

import json
import re
from pathlib import Path

from telescope.models import PhoneState
from telescope.plugins.camera_control import derive_camera_control_view
from telescope.view import PhoneZoomCaps

ROOT = Path(__file__).resolve().parents[2]
EXAMPLE = json.loads((ROOT / "docs" / "state.json").read_text(encoding="utf-8"))


class ContractDict(dict):
    """A typo must fail here, even when a parser would otherwise quietly take its default."""
    def get(self, key, default=None):
        assert key in self, f"The phone's example has no field {key!r}"
        return super().get(key, default)


def _contract(value):
    if isinstance(value, dict):
        return ContractDict({k: _contract(v) for k, v in value.items()})
    return [_contract(v) for v in value] if isinstance(value, list) else value


def test_the_desktop_reads_the_phones_state():
    state = PhoneState.from_dict(_contract(EXAMPLE))
    assert not state.is_empty
    assert state.iso == 400 and state.shutter_ns == 16_666_666 and state.battery == 87
    camera = state.cameras[0]
    assert camera.id == "2" and camera.iso_max == 3200 and camera.lens_zooms == (3.0,)
    assert (state.auto, state.wb_manual, state.ois, state.focus_mode, state.focus_distance) == (False, True, False, "manual", 2.5)
    assert (state.ae_comp, state.nr_mode, state.edge_mode, state.black_level_lock, state.torch) == (-3, 2, 2, True, True)
    assert (state.charging, state.battery_temp_c) == (True, 32.5)
    assert (EXAMPLE["cameras"][0]["facing"], EXAMPLE["cameras"][0]["focalMm"], EXAMPLE["cameras"][0]["lensKind"]) == ("back", 24, "physical")
    assert (camera.logical_id, camera.label, camera.current, camera.has_ois, camera.iso_min) == ("0", "Back ~24mm OIS [phys]", True, True, 50)
    assert (camera.shutter_min_ns, camera.shutter_max_ns, camera.min_focus_distance) == (100_000, 1_000_000_000, 10)
    assert camera.supports_manual_sensor and camera.supports_manual_wb and camera.supports_manual_focus
    assert camera.supports_focus_point and camera.supports_meter_point
    assert camera.supports_flash and camera.freeform_crop
    assert (camera.zoom_ratio_max, camera.crop_zoom_max, camera.hw_level) == (10, 8, "FULL")
    assert (camera.ae_comp_min, camera.ae_comp_max, camera.ae_comp_step) == (-12, 12, 0.16666667)
    assert state.raw == EXAMPLE


def test_nested_camera_and_control_consumers_read_the_same_contract():
    state = _contract(EXAMPLE)
    view = derive_camera_control_view(state)
    assert view.manual_exposure and view.manual_wb and view.manual_focus
    assert view.low_light and view.low_light_available and view.torch
    caps = PhoneZoomCaps.from_camera(state["cameras"][0])
    assert (caps.ratio_max, caps.crop_max, caps.lens_zooms) == (10, 8, (3.0,))


def test_every_state_field_the_desktop_asks_for_is_one_the_phone_sends():
    # A field renamed or dropped on the phone shows up here, instead of as a control that quietly stops working
    asked = set()
    for path in (ROOT / "desktop" / "telescope").rglob("*.py"):
        asked |= set(re.findall(r'\bstate\.get\("([A-Za-z0-9_]+)"', path.read_text(encoding="utf-8")))
    assert asked, "the pattern no longer finds anything: update it"
    assert asked - EXAMPLE.keys() == set()
