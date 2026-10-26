"""Cinema tools: false colour, zebras and focus peaking on the preview's copy, the level guide, and the card that
switches them."""

import cv2
import numpy as np
import pytest

from telescope.overlays import PreviewOverlays
from telescope.plugin import EventBus
from telescope.plugins.cinema import (
    FALSE_COLOURS, CinemaLayer, CinemaPlugin, Level, LevelFilter, Look, false_colour, level_line, paint_level,
    peaking_mask, shown_roll, zebra_mask,
)
from telescope.sensors import SensorReading, SensorStatus
from telescope.view import View


def _grey(level, w=64, h=36):
    return np.full((h, w, 3), level, np.uint8)


def _colour_for(pct):
    return next(c for upper, c, _n in FALSE_COLOURS if pct <= upper)


def test_false_colour_marks_crushed_middle_grey_skin_and_clipped():
    assert tuple(false_colour(_grey(0))[0, 0]) == _colour_for(0)
    assert tuple(false_colour(_grey(255))[0, 0]) == _colour_for(100)
    assert tuple(false_colour(_grey(round(0.42 * 255)))[0, 0]) == _colour_for(42)  # 18% grey lands around 42%
    assert tuple(false_colour(_grey(round(0.55 * 255)))[0, 0]) == _colour_for(55)
    plain = false_colour(_grey(round(0.25 * 255)))[0, 0]
    assert plain[0] == plain[1] == plain[2]  # well exposed stays grey


def test_zebras_stripe_only_what_is_bright_enough():
    frame = _grey(200)
    frame[:, 32:] = 250
    mask = zebra_mask(frame, 95)
    assert not mask[:, :32].any()
    assert 0.3 < mask[:, 32:].mean() < 0.7  # stripes, not a solid block
    assert not (mask == zebra_mask(frame, 95, phase=2)).all()  # they move along


def test_peaking_finds_sharp_edges_and_nothing_on_a_flat_picture():
    frame = _grey(40)
    frame[:, 32:] = 220
    assert peaking_mask(frame, 5)[:, 30:34].any()
    assert not peaking_mask(_grey(128), 10).any()
    soft = np.tile(np.linspace(40, 220, 64, dtype=np.uint8), (36, 1))[..., None].repeat(3, axis=2)
    assert not peaking_mask(soft, 1).any()  # a gentle ramp isn't an edge


def test_the_layer_tints_its_copy_and_only_analyses_with_a_scope():
    layer = CinemaLayer()
    assert layer.analyse_fps == 0
    layer.look = Look(false_colour=True, scope="waveform")
    assert layer.analyse_fps > 0
    frame = _grey(0)
    assert tuple(layer.tint(frame)[0, 0]) == _colour_for(0)
    assert layer.analyse(np.zeros((36, 64, 3), np.uint8))
    assert layer._scope[0] == "waveform" and not layer._scope[1].isNull()


def test_the_tools_judge_the_picture_before_any_of_them_paints():
    layer = CinemaLayer()
    wall = _grey(250)  # a plain bright wall: zebras all over, nothing sharp
    layer.look = Look(zebras=True, peaking=True, peaking_colour="green")
    out = layer.tint(wall.copy())
    green = np.all(out == (60, 255, 60), axis=2)
    assert green.mean() < 0.01  # the zebras' stripes aren't edges to peak on
    assert (out == 0).all(axis=2).mean() > 0.3
    clipped = _grey(255)
    layer.look = Look(zebras=True, false_colour=True)
    out = layer.tint(clipped.copy())
    assert (out == 0).all(axis=2).mean() > 0.3  # still striped under false colour's red
    assert (out == _colour_for(100)).all(axis=2).mean() > 0.3


def test_a_big_preview_is_judged_small_and_marked_at_its_own_size():
    layer = CinemaLayer()
    frame = _grey(40, w=2560, h=1440)
    frame[:, 1280:] = 255
    layer.look = Look(peaking=True, zebras=True, peaking_colour="green")
    out = layer.tint(frame.copy())
    assert out.shape == frame.shape
    assert np.all(out[:, 1276:1284] == (60, 255, 60), axis=2).any()  # the edge, where it is in the big frame
    assert not np.all(out[:, :1200] == (60, 255, 60), axis=2).any()
    assert 0.3 < (out[:, 1300:] == 0).all(axis=2).mean() < 0.7  # striped, not blacked out
    layer.look = Look(false_colour=True)
    assert tuple(layer.tint(frame.copy())[10, 2000]) == _colour_for(100)


class _Host:
    def __init__(self):
        self.overlays = PreviewOverlays(View)
        self.saves = 0
        self.wanted = {}
        self.status = SensorStatus()
        self.reading = None
        self.shown_view = View()
        self.refreshes = 0

    def want_sensors(self, owner, kinds):
        self.wanted[owner] = list(kinds)

    def sensor_status(self, source_id=None):
        return self.status

    def sensor_reading(self, source_id=None):
        return self.reading

    def focused_source_id(self):
        return "phone"

    def view(self):
        return self.shown_view

    def add_preview_layer(self, owner, layer):
        self.overlays.add_layer(owner, layer)

    def remove_preview_layer(self, owner):
        self.overlays.remove_layer(owner)

    def refresh_preview(self):
        self.refreshes += 1

    def schedule_save(self):
        self.saves += 1


@pytest.fixture
def cinema(qt_widgets):
    host, bus = _Host(), EventBus()
    plugin = CinemaPlugin()
    plugin.setup(host, bus)
    qt_widgets(plugin.create_panel())
    for section in plugin.create_settings_sections():
        qt_widgets(section.widget)
    plugin.set_config({})
    return plugin, host


def test_the_layer_is_on_the_preview_only_while_a_tool_is(cinema):
    plugin, host = cinema
    assert host.overlays.layers() == []
    plugin._zebras_switch.setChecked(True)
    assert host.overlays.layers() == [plugin._layer] and plugin._layer.look.zebras
    plugin._zebras_switch.setChecked(False)
    assert host.overlays.layers() == []


def test_settings_round_trip_and_bad_values_fall_back(cinema):
    plugin, _host = cinema
    plugin.set_config({"peaking": True, "scope": "vectorscope", "peaking_colour": "blue", "peaking_level": 9,
                       "zebra_level": 80, "scope_size": "large"})
    assert plugin.get_config() == {"peaking": True, "zebras": False, "false_colour": False, "level": False,
                                   "scope": "vectorscope",
                                   "peaking_colour": "blue", "peaking_level": 9, "zebra_level": 80,
                                   "scope_size": "large"}
    assert plugin._scope_combo.currentData() == "vectorscope" and plugin._colour_btns["blue"].isChecked()
    plugin.set_config({"scope": "oscilloscope", "peaking_colour": "plaid", "zebra_level": 3, "peaking_level": "x"})
    cfg = plugin.get_config()
    assert (cfg["scope"], cfg["peaking_colour"], cfg["zebra_level"], cfg["peaking_level"]) == ("off", "red", 70, 5)


def test_the_settings_window_changes_what_the_layer_draws(cinema):
    plugin, host = cinema
    plugin._peaking_switch.setChecked(True)
    plugin._colour_btns["white"].click()
    plugin._zebra_slider.setValue(85)
    plugin._size_btns["small"].click()
    look = plugin._layer.look
    assert (look.peaking_colour, look.zebra_level, look.scope_size) == ("white", 85, "small")
    assert plugin._zebra_lbl.text() == "85%"
    assert host.saves


def test_diagnostics_say_what_is_on(cinema):
    plugin, _host = cinema
    assert plugin.diagnostics() == {"Cinema tools": "nothing on"}
    plugin.set_config({"zebras": True, "scope": "parade"})
    assert plugin.diagnostics() == {"Cinema tools": "zebras, parade"}


# ── Level guide ──────────────────────────────────────────────────────────────

def test_the_roll_as_shown_follows_the_views_flip_and_rotation():
    assert shown_roll(5.0, View()) == 5.0
    assert shown_roll(5.0, View(flip_h=True)) == -5.0
    assert shown_roll(5.0, View(flip_v=True)) == -5.0
    assert shown_roll(5.0, View(flip_h=True, flip_v=True)) == 5.0  # both flips are a half turn
    assert shown_roll(-90.0, View(rotation=cv2.ROTATE_90_CLOCKWISE)) == 0.0  # portrait phone, turned upright here
    assert shown_roll(90.0, View(rotation=cv2.ROTATE_90_COUNTERCLOCKWISE)) == 0.0
    assert shown_roll(170.0, View(rotation=cv2.ROTATE_180)) == -10.0
    # Flipped, then turned: the flip comes first
    assert shown_roll(80.0, View(flip_h=True, rotation=cv2.ROTATE_90_CLOCKWISE)) == 10.0


def test_the_filter_follows_a_change_gradually():
    f = LevelFilter(smooth_s=0.3)
    assert f.update(10.0, 0.0, 0.0).roll == 10.0  # the first reading counts as it is
    rolls = [f.update(0.0, 0.0, i / 30).roll for i in range(1, 31)]
    assert all(a >= b for a, b in zip(rolls, rolls[1:])) and rolls[0] > 8.0 and rolls[5] > 3.0
    assert rolls[-1] == 0.0  # a second later it's there, and shows as level


def test_jitter_near_level_stays_level_and_green():
    f = LevelFilter()
    rng = np.random.default_rng(1)
    levels = [f.update(float(rng.normal(0.0, 0.4)), float(rng.normal(0.0, 0.6)), i / 30) for i in range(120)]
    assert all(lv.roll_level and lv.roll == 0.0 for lv in levels[10:])
    assert all(lv.pitch_level and lv.pitch == 0.0 for lv in levels[10:])


def test_level_has_a_margin_either_way():
    f = LevelFilter(smooth_s=0.001)  # no smoothing, to see the margins
    assert f.update(0.7, 0.0, 0.0).roll_level is False  # not level yet: has to come within LEVEL_SNAP
    assert f.update(0.4, 0.0, 0.1).roll_level is True
    assert f.update(0.8, 0.0, 0.2).roll_level is True  # stays level until past LEVEL_LEAVE
    lv = f.update(1.2, 0.0, 0.3)
    assert lv.roll_level is False and lv.roll == pytest.approx(1.2)


def test_the_filter_goes_the_short_way_round_past_180():
    f = LevelFilter(smooth_s=0.3)
    f.update(179.0, 0.0, 0.0)
    roll = f.update(-179.0, 0.0, 1 / 30).roll
    assert abs(roll) > 178.9  # not swung back through 0


def test_pointing_straight_up_has_no_roll_but_keeps_pitch():
    f = LevelFilter()
    f.update(3.0, 10.0, 0.0)
    lv = f.update(None, 85.0, 0.1)
    assert lv.roll is None and lv.pitch > 10.0
    assert f.update(None, None, 0.2) is None


def test_the_line_turns_clockwise_for_a_positive_roll():
    from PyQt6.QtCore import QRectF
    rect = QRectF(0, 0, 640, 360)
    left, right = level_line(rect, 0.0)
    assert left.y() == right.y() == 180 and left.x() < 320 < right.x()
    left, right = level_line(rect, 10.0)
    assert right.y() > 180 > left.y()  # widget y goes down: the right end drops
    left, right = level_line(rect, 90.0)
    assert left.x() == pytest.approx(320) and right.y() > left.y()


def test_the_guide_paints_without_trouble(qt_widgets):
    from PyQt6.QtCore import QRectF
    from PyQt6.QtGui import QImage, QPainter
    image = QImage(320, 180, QImage.Format.Format_ARGB32)
    for level in (Level(3.2, -4.0, False, False), Level(0.0, 0.0, True, True), Level(None, 88.0, False, False)):
        image.fill(0)
        p = QPainter(image)
        paint_level(p, QRectF(0, 0, 320, 180), level)
        p.end()
        assert QImage(image).constBits() is not None


def test_the_level_switch_asks_for_the_sensors_only_while_on(cinema):
    plugin, host = cinema
    assert host.wanted.get("cinema", []) == []
    plugin._level_switch.setChecked(True)
    assert host.wanted["cinema"] == ["orientation"] and host.overlays.layers() == [plugin._layer]
    plugin._level_switch.setChecked(False)
    assert host.wanted["cinema"] == [] and host.overlays.layers() == []
    plugin.set_config({"level": True})
    assert host.wanted["cinema"] == ["orientation"] and plugin.get_config()["level"] is True
    plugin.shutdown()
    assert host.wanted["cinema"] == []


def test_readings_reach_the_layer_as_shown(cinema):
    plugin, host = cinema
    plugin._level_switch.setChecked(True)
    host.shown_view = View(flip_h=True)
    host.reading = SensorReading(at=1.0, roll=6.0, pitch=-3.0, camera="0")
    plugin._on_sensors("phone")
    level, _at = plugin._layer.level
    assert (level.roll, level.pitch, level.roll_level) == (-6.0, -3.0, False)
    refreshes = host.refreshes
    host.reading = SensorReading(at=1.01, roll=6.0, pitch=-3.0, camera="0")
    plugin._on_sensors("phone")
    assert host.refreshes == refreshes  # nothing new to draw
    plugin._on_sensors("another phone")  # not the shown stream
    host.reading = None
    plugin._on_sensors("phone")
    assert plugin._layer.level is None


def test_why_the_level_cant_show_is_on_the_card(cinema):
    plugin, host = cinema
    plugin._level_switch.setChecked(True)
    host.status = SensorStatus("unavailable", reason="old_phone")
    plugin._on_sensors("phone")
    assert not plugin._level_note.isHidden() and "Update Telescope" in plugin._level_note.text()
    host.status = SensorStatus("on", kinds=("orientation",), streaming=("orientation",))
    plugin._on_sensors("phone")
    assert plugin._level_note.isHidden()
    host.status = SensorStatus("unavailable", reason="not_phone")
    plugin._on_sensors("phone")
    plugin._level_switch.setChecked(False)
    assert plugin._level_note.isHidden()
