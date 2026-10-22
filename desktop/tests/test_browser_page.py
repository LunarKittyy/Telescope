"""The Browser camera page's script (telescope/web/app.js), run in Node against stand-ins for the browser."""

import shutil
import subprocess
from pathlib import Path

import pytest

HARNESS = Path(__file__).parent / "js" / "page_harness.js"
NODE = shutil.which("node")

pytestmark = pytest.mark.skipif(NODE is None, reason="needs Node.js")

SCENARIOS = [
    "the_page_offers_and_applies_the_answer",
    "a_superseded_offer_is_not_sent",
    "a_failed_connection_offers_again",
    "flip_keeps_the_mic",
    "the_mirror_follows_the_camera_not_the_request",
    "the_mic_follows_the_computer",
    "a_lost_mic_comes_back",
    "a_refused_mic_leaves_the_camera_working",
    "a_size_the_camera_ignores_reopens_it",
    "a_new_link_stops_the_page",
    "a_dropped_socket_reconnects",
    "stats_report_what_is_sent",
    "one_camera_hides_flip",
    "a_front_camera_lights_the_screen",
    "a_flash_stays_on_through_a_new_size",
    "stop_while_the_camera_opens",
    "a_double_tap_wakes_the_black_screen",
    "a_lone_tap_times_out",
    "the_button_tap_is_not_half_a_double_tap",
    "black_screen_without_full_screen",
    "iphone_black_screen_uses_the_video_player",
    "a_stalled_camera_closes_the_iphone_player",
    "a_moving_camera_keeps_the_iphone_player",
    "a_black_video_that_does_not_play_is_not_used",
    "a_failing_black_video_leaves_start_working",
    "the_player_works_again_after_ios_paused_the_video",
    "pausing_in_the_player_resumes",
    "stop_closes_the_iphone_player",
    "no_black_video_where_the_page_can_go_full_screen",
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
