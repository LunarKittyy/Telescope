"""Cinema tools: false colour, zebras and focus peaking on the preview's copy, and the card that switches them."""

import numpy as np
import pytest

from telescope.overlays import PreviewOverlays
from telescope.plugin import EventBus
from telescope.plugins.cinema import (
    FALSE_COLOURS, CinemaLayer, CinemaPlugin, Look, false_colour, peaking_mask, zebra_mask,
)
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


class _Host:
    def __init__(self):
        self.overlays = PreviewOverlays(View)
        self.saves = 0

    def add_preview_layer(self, owner, layer):
        self.overlays.add_layer(owner, layer)

    def remove_preview_layer(self, owner):
        self.overlays.remove_layer(owner)

    def refresh_preview(self):
        pass

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
    assert plugin.get_config() == {"peaking": True, "zebras": False, "false_colour": False, "scope": "vectorscope",
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
