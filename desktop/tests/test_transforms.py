import cv2
import numpy as np
import pytest

from telescope.plugin import EventBus
from telescope.plugins.transforms import (
    ROTATIONS,
    TransformsPlugin,
    _apply_zoom,
    _transform_frame,
)


class _Host:
    def __init__(self):
        self.saves = 0

    def _schedule_save(self):
        self.saves += 1


@pytest.fixture
def transforms_plugin(qapp):
    host = _Host()
    plugin = TransformsPlugin()
    plugin.setup(host, EventBus())
    panel = plugin.create_panel()
    return plugin, host, panel


def _grid():
    return np.arange(4 * 6, dtype=np.uint8).reshape(4, 6, 1).repeat(3, axis=2)


@pytest.mark.parametrize("zoom", [0.5, 1.0])
def test_zoom_at_or_below_one_is_zero_copy(zoom):
    frame = _grid()
    assert _apply_zoom(frame, zoom, 0, 0) is frame


@pytest.mark.parametrize("pan_x,pan_y", [(-1, -1), (0, 0), (1, 1)])
def test_zoom_preserves_shape_for_all_pan_extremes(pan_x, pan_y):
    frame = _grid()
    result = _apply_zoom(frame, 2.0, pan_x, pan_y)
    assert result.shape == frame.shape


def test_zoom_pan_selects_opposite_source_regions():
    frame = _grid()
    top_left = _apply_zoom(frame, 2.0, -1, -1)
    bottom_right = _apply_zoom(frame, 2.0, 1, 1)

    assert top_left.mean() < bottom_right.mean()


@pytest.mark.parametrize(
    "flip_h,flip_v,expected",
    [
        (False, False, lambda f: f),
        (True, False, lambda f: np.flip(f, axis=1)),
        (False, True, lambda f: np.flip(f, axis=0)),
        (True, True, lambda f: np.flip(f, axis=(0, 1))),
    ],
)
def test_transform_flip_combinations(flip_h, flip_v, expected):
    frame = _grid()
    assert np.array_equal(_transform_frame(frame, flip_h, flip_v, None), expected(frame))


@pytest.mark.parametrize("label,rotation", list(ROTATIONS.items()))
def test_transform_rotations_match_opencv(label, rotation):
    frame = _grid()
    expected = frame if rotation is None else cv2.rotate(frame, rotation)
    assert np.array_equal(_transform_frame(frame, False, False, rotation), expected), label


def test_plugin_handlers_update_runtime_state_and_schedule_save(transforms_plugin):
    plugin, host, _panel = transforms_plugin

    plugin._flip_h.setChecked(True)
    plugin._rot_combo.setCurrentText("90 CW")
    plugin._zoom_slider.setValue(250)
    plugin._pan_x_slider._slider.setValue(100)
    plugin._pan_y_slider._slider.setValue(-50)

    assert plugin.flip_h is True
    assert plugin.rotation == cv2.ROTATE_90_CLOCKWISE
    assert plugin.zoom == 2.5
    assert plugin.pan_x == 0.5
    assert plugin.pan_y == -0.25
    assert plugin._pan_x_slider._slider.isEnabled()
    assert host.saves >= 5


def test_returning_zoom_to_one_resets_and_disables_pan(transforms_plugin):
    plugin, _host, _panel = transforms_plugin
    plugin._zoom_slider.setValue(200)
    plugin._pan_x_slider._slider.setValue(160)
    plugin._pan_y_slider._slider.setValue(-160)

    plugin._zoom_slider.setValue(100)

    assert plugin.pan_x == 0
    assert plugin.pan_y == 0
    assert plugin._pan_x_slider.get_value() == 0
    assert plugin._pan_y_slider.get_value() == 0
    assert not plugin._pan_x_slider._slider.isEnabled()


def test_plugin_process_frame_uses_runtime_transform_state(transforms_plugin):
    plugin, _host, _panel = transforms_plugin
    plugin.flip_h = True
    plugin.flip_v = False
    plugin.rotation = cv2.ROTATE_90_CLOCKWISE
    plugin.zoom = 1.0

    result = plugin.process_frame(_grid())

    expected = cv2.rotate(cv2.flip(_grid(), 1), cv2.ROTATE_90_CLOCKWISE)
    assert np.array_equal(result, expected)


def test_config_round_trip_updates_widgets(transforms_plugin):
    plugin, _host, _panel = transforms_plugin
    cfg = {
        "flip_h": True,
        "flip_v": True,
        "rotation": "180",
        "zoom": 3.0,
        "pan_x": 0.4,
        "pan_y": -0.6,
    }

    plugin.set_config(cfg)

    assert plugin.get_config() == pytest.approx(cfg)
    assert plugin._pan_x_slider._slider.isEnabled()


def test_saved_pan_config_updates_runtime_processing_state(transforms_plugin):
    plugin, _host, _panel = transforms_plugin

    plugin.set_config({"zoom": 2.0, "pan_x": 0.75, "pan_y": -0.5})

    assert plugin.pan_x == pytest.approx(0.75)
    assert plugin.pan_y == pytest.approx(-0.5)
