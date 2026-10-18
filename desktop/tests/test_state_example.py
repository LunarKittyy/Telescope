"""docs/state.json is one /v1/state that the phone's StateExampleTest.kt checks it still sends; this checks the desktop still reads it."""

import json
import re
from pathlib import Path

from telescope.models import PhoneState

ROOT = Path(__file__).resolve().parents[2]
EXAMPLE = json.loads((ROOT / "docs" / "state.json").read_text(encoding="utf-8"))


def test_the_desktop_reads_the_phones_state():
    state = PhoneState.from_dict(EXAMPLE)
    assert not state.is_empty
    assert state.iso == 400 and state.shutter_ns == 16_666_666 and state.battery == 87
    camera = state.cameras[0]
    assert camera.id == "2" and camera.iso_max == 3200 and camera.lens_zooms == (3.0,)


def test_every_state_field_the_desktop_asks_for_is_one_the_phone_sends():
    # A field renamed or dropped on the phone shows up here, instead of as a control that quietly stops working
    asked = set()
    for path in (ROOT / "desktop" / "telescope").rglob("*.py"):
        asked |= set(re.findall(r'\bstate\.get\("([A-Za-z0-9_]+)"', path.read_text(encoding="utf-8")))
    assert asked, "the pattern no longer finds anything: update it"
    assert asked - EXAMPLE.keys() == set()
