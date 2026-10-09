"""The Browser camera page's script (telescope/web/app.js), run in Node against stand-ins for the browser."""

import shutil
import subprocess
from pathlib import Path

import pytest

HARNESS = Path(__file__).parent / "js" / "page_harness.js"
NODE = shutil.which("node")

pytestmark = pytest.mark.skipif(NODE is None, reason="needs Node.js")

SCENARIOS = [
    "the_capability_check_builds_the_encoder",
    "stop_during_the_capability_check",
    "stop_during_a_failed_capability_check",
    "a_stale_capability_check_does_not_serve_the_next_run",
    "a_mic_setup_from_before_stop_adds_no_second_sender",
    "a_failed_mic_setup_from_before_stop_leaves_the_mic_error_alone",
    "a_double_tap_wakes_the_black_screen",
    "a_lone_tap_times_out",
    "the_button_tap_is_not_half_a_double_tap",
    "black_screen_without_full_screen",
    "stop_wakes_the_black_screen",
    "a_dropped_wake_lock_is_asked_for_again",
    "a_lock_dropped_straight_away_is_not_asked_for_again",
    "a_third_tap_after_waking_does_not_press_stop",
]


def _run(*args):
    return subprocess.run([NODE, str(HARNESS), *args], capture_output=True, text=True, timeout=60)


def test_every_scenario_is_run():
    assert _run("--list").stdout.split() == SCENARIOS


@pytest.mark.parametrize("scenario", SCENARIOS)
def test_page_scenario(scenario):
    result = _run(scenario)
    assert result.returncode == 0, result.stderr
