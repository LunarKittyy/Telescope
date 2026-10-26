"""Chroma key: the matte, spill removal, picking the screen's colour and the card."""

import numpy as np
import pytest

from telescope.overlays import Pick
from telescope.plugin import EventBus
from telescope.plugins.chroma_key import (
    SCREENS, ChromaKeyPlugin, Settings, chroma_of, key, key_tables, keyable, matte, sample,
)

GREEN = next(bgr for k, _t, bgr in SCREENS if k == "green")
BLUE = next(bgr for k, _t, bgr in SCREENS if k == "blue")
SKIN = (120, 150, 210)


def _scene(w=64, h=36):
    """A green screen with a skin-coloured block in the middle."""
    frame = np.full((h, w, 3), GREEN, np.uint8)
    frame[10:26, 20:44] = SKIN
    return frame


def test_the_screen_goes_and_the_person_stays():
    alpha, _clean = matte(_scene(), key_tables(GREEN, 40, 30, 60))
    assert alpha[2:6, 2:10].max() < 10  # screen
    assert alpha[14:22, 26:38].min() > 245  # person


def test_tolerance_decides_how_far_from_the_screen_still_goes():
    near = (80, 170, 70)  # a slightly different green, as a crease or shadow on the screen would be
    tight = key_tables(GREEN, 0, 0, 0)[0]
    loose = key_tables(GREEN, 90, 0, 0)[0]
    frame = np.full((4, 4, 3), near, np.uint8)
    assert matte(frame, (tight, *key_tables(GREEN, 0, 0, 0)[1:]))[0].min() > 200
    assert matte(frame, (loose, *key_tables(GREEN, 90, 0, 0)[1:]))[0].max() < 30


def test_softness_makes_a_ramp_instead_of_a_cliff():
    cb, cr = chroma_of(GREEN)
    hard = key_tables(GREEN, 40, 0, 0)[0].reshape(256, 256)
    soft = key_tables(GREEN, 40, 100, 0)[0].reshape(256, 256)
    ramp = lambda t: np.count_nonzero((t > 10) & (t < 245))  # noqa: E731
    assert ramp(soft) > 3 * ramp(hard)
    assert hard[round(cr) + 128, round(cb) + 128] == 0 == soft[round(cr) + 128, round(cb) + 128]


def test_spill_takes_green_out_of_a_green_tinged_edge_but_leaves_other_colours():
    tinged = (110, 170, 140)  # skin with green light on it
    frame = np.full((4, 4, 3), tinged, np.uint8)
    _alpha, kept = matte(frame, key_tables(GREEN, 10, 10, 0))
    _alpha, clean = matte(frame, key_tables(GREEN, 10, 10, 100))
    green_in = lambda px: int(px[1]) - (int(px[0]) + int(px[2])) // 2  # noqa: E731
    assert green_in(clean[0, 0]) < green_in(kept[0, 0]) - 10
    for colour in (SKIN, (40, 40, 220), (220, 60, 40)):  # skin, red, blue: nowhere near green
        frame = np.full((4, 4, 3), colour, np.uint8)
        _alpha, clean = matte(frame, key_tables(GREEN, 40, 30, 100))
        assert np.abs(clean[0, 0].astype(int) - colour).max() <= 6


def test_a_blue_screen_keys_blue_and_keeps_green():
    frame = np.full((8, 8, 3), BLUE, np.uint8)
    frame[:, 4:] = GREEN
    alpha, _clean = matte(frame, key_tables(BLUE, 40, 30, 60))
    assert alpha[:, :3].max() < 10 and alpha[:, 5:].min() > 245


def test_the_backdrop_is_a_colour_or_the_picture_covering_the_frame():
    tables = key_tables(GREEN, 40, 30, 60)
    out = key(_scene(), Settings(True, tables, "colour", (10, 20, 30)))
    assert tuple(out[2, 2]) == (10, 20, 30)
    assert np.abs(out[18, 32].astype(int) - SKIN).max() <= 6
    picture = np.zeros((10, 10, 3), np.uint8)
    picture[:, :, 2] = 200
    out = key(_scene(), Settings(True, tables, "picture", (10, 20, 30), picture))
    assert tuple(out[2, 2]) == (0, 0, 200)
    frame = _scene()
    assert key(frame, Settings(False, tables)) is frame


def test_a_picked_colour_is_the_median_around_the_click():
    frame = np.full((40, 40, 3), GREEN, np.uint8)
    frame[20, 20] = (255, 255, 255)  # one bright speck doesn't decide it
    assert sample(frame, 0.5, 0.5) == GREEN
    assert sample(frame, 1.0, 0.0) == GREEN
    assert keyable(GREEN) and not keyable((128, 128, 128)) and not keyable((250, 250, 250))


class _Host:
    def __init__(self, can_pick=True):
        self.can_pick, self.picks, self.saves = can_pick, [], 0

    def pick_on_preview(self, owner, kind, on_done, start=None, hint=""):
        self.picks.append((owner, kind, on_done))
        return self.can_pick

    def schedule_save(self):
        self.saves += 1


@pytest.fixture
def chroma(qt_widgets):
    def make(**kw):
        host, bus = _Host(**kw), EventBus()
        plugin = ChromaKeyPlugin()
        plugin.setup(host, bus)
        qt_widgets(plugin.create_panel())
        bus.phones_changed.emit(1)  # the card is dimmed until a phone is paired
        plugin.set_config({})
        return plugin, host
    return make


def test_picking_on_the_preview_keys_that_colour(chroma):
    plugin, host = chroma()
    purple = (160, 40, 150)
    frame = np.full((36, 64, 3), purple, np.uint8)
    plugin.process_frame(frame)
    plugin._screen_btns["picked"].click()
    owner, kind, on_done = host.picks[-1]
    assert (owner, kind) == ("chroma_key", "point")
    assert not plugin._screen_btns["picked"].isChecked()  # nothing picked yet
    on_done(Pick("point", ((0.5, 0.5),), ((0.5, 0.5),)))
    assert plugin.get_config()["picked"] == list(purple) and plugin._screen_btns["picked"].isChecked()
    plugin._switch.setChecked(True)
    assert plugin.process_frame(frame)[5, 5].tolist() == list(plugin.get_config()["colour"])


def test_a_grey_pick_is_refused(chroma):
    plugin, host = chroma()
    plugin.process_frame(np.full((36, 64, 3), 128, np.uint8))
    plugin._pick()
    host.picks[-1][2](Pick("point", ((0.5, 0.5),), ((0.5, 0.5),)))
    assert plugin.get_config()["picked"] is None and "hardly any colour" in plugin._status.text()


def test_without_a_stream_the_card_says_to_start_one(chroma):
    plugin, _host = chroma(can_pick=False)
    plugin._pick()
    assert "Start streaming" in plugin._status.text() and not plugin._status.isHidden()


def test_settings_round_trip_and_bad_values_fall_back(chroma):
    plugin, _host = chroma()
    cfg = {"on": True, "screen": "picked", "picked": [160, 40, 150], "tolerance": 70, "softness": 10, "spill": 0,
           "background": "colour", "colour": [1, 2, 3], "picture": ""}
    plugin.set_config(cfg)
    assert plugin.get_config() == cfg
    plugin.set_config({"screen": "picked", "picked": [128, 128, 128], "tolerance": 500, "colour": "red",
                       "background": "video"})
    got = plugin.get_config()
    assert (got["screen"], got["picked"], got["tolerance"], got["background"]) == ("green", None, 100, "colour")


def test_a_stream_behind_the_panels_keeps_its_key(chroma):
    plugin, _host = chroma()
    assert plugin.frame_step() is None
    plugin.set_config({"on": True, "colour": [9, 9, 9]})
    step = plugin.frame_step()
    plugin.set_config({})
    assert tuple(step(_scene())[2, 2]) == (9, 9, 9)
