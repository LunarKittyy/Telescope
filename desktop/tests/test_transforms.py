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

    def schedule_save(self):
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


# ── Mapping a preview click back onto the phone's frame ──────────────────────

from telescope.plugins.transforms import inverse_map  # noqa: E402


@pytest.mark.parametrize("rotation", list(ROTATIONS.values()))
@pytest.mark.parametrize("flip_h,flip_v", [(False, False), (True, False), (False, True), (True, True)])
@pytest.mark.parametrize("zoom,pan", [(1.0, (0, 0)), (2.0, (0.0, 0.0)), (2.5, (0.6, -0.4))])
def test_inverse_map_undoes_what_the_preview_shows(rotation, flip_h, flip_v, zoom, pan):
    """Mark one pixel, run the real transforms, then map the marked pixel's spot back."""
    w, h = 160, 90
    src = (97, 41)
    frame = np.zeros((h, w, 3), np.uint8)
    frame[src[1], src[0]] = 255
    out = _transform_frame(_apply_zoom(frame, zoom, *pan), flip_h, flip_v, rotation)
    ys, xs = np.nonzero(out[:, :, 0] > 20)
    oh, ow = out.shape[:2]
    u, v = (xs.mean() + 0.5) / ow, (ys.mean() + 0.5) / oh
    x, y = inverse_map(u, v, w, h, zoom, pan[0], pan[1], flip_h, flip_v, rotation)
    assert abs(x * w - (src[0] + 0.5)) < 1.5
    assert abs(y * h - (src[1] + 0.5)) < 1.5


def test_inverse_map_clamps_and_survives_an_unknown_frame_size():
    assert inverse_map(1.2, -0.1, 0, 0, zoom=2.0) == (1.0, 0.0)


def test_a_picked_point_goes_out_in_phone_frame_coordinates(transforms_plugin):
    plugin, _host, _panel = transforms_plugin
    seen = []
    plugin._bus.focus_point.connect(lambda x, y: seen.append((round(x, 3), round(y, 3))))
    plugin.process_frame(np.zeros((90, 160, 3), np.uint8))
    plugin.flip_h = True
    plugin._bus.focus_point_picked.emit(0.25, 0.5)
    assert seen == [(0.75, 0.5)]


# ── Splitting zoom between the phone and this computer ───────────────────────

from telescope.plugins.transforms import PhoneZoom, PhoneZoomCaps, split_zoom  # noqa: E402

_TELE = PhoneZoomCaps(ratio_max=10.0, crop_max=4.0, freeform=True)
_CENTRE_ONLY = PhoneZoomCaps(ratio_max=10.0, crop_max=4.0, freeform=False)
_CROP_ONLY = PhoneZoomCaps(ratio_max=1.0, crop_max=2.0, freeform=True)


def _framing(split):
    """(size, centre_x, centre_y) the phone's crop and the desktop's remainder end up showing, 0..1."""
    phone = split.phone or PhoneZoom()
    view = 1.0 / (phone.ratio * phone.crop)
    vx = 0.5 + (phone.x - 0.5) / phone.ratio
    vy = 0.5 + (phone.y - 0.5) / phone.ratio
    zoom, pan_x, pan_y = split.desktop
    step = view * (1.0 - 1.0 / zoom) / 2.0
    return view / zoom, vx + pan_x * step, vy + pan_y * step


@pytest.mark.parametrize("caps", [None, _TELE, _CENTRE_ONLY, _CROP_ONLY, PhoneZoomCaps(1.5, 1.0, False)])
@pytest.mark.parametrize("zoom,pan", [(1.0, (0, 0)), (2.0, (0, 0)), (3.0, (1, 0)), (5.0, (-0.4, 0.8)), (2.5, (1, -1))])
def test_split_zoom_always_frames_what_was_asked(caps, zoom, pan):
    size, cx, cy = _framing(split_zoom(zoom, *pan, caps))
    assert size == pytest.approx(1.0 / zoom, abs=1e-3)
    assert cx == pytest.approx(0.5 + pan[0] * (1 - 1 / zoom) / 2, abs=1e-3)
    assert cy == pytest.approx(0.5 + pan[1] * (1 - 1 / zoom) / 2, abs=1e-3)


def test_a_lens_that_cant_zoom_leaves_it_all_to_the_desktop():
    split = split_zoom(2.5, 0.3, -0.2, None)
    assert split.phone is None
    assert split.desktop == (2.5, 0.3, -0.2)


def test_centred_zoom_goes_all_to_the_phone_ratio():
    for caps in (_TELE, _CENTRE_ONLY):
        split = split_zoom(3.0, 0, 0, caps)
        assert split.phone == PhoneZoom(3.0, 1.0, 0.5, 0.5)
        assert split.desktop == (1.0, 0.0, 0.0)


def test_panning_to_the_edge_backs_the_ratio_off_and_crops_off_centre():
    split = split_zoom(3.0, 1, 0, _TELE)
    assert split.phone.ratio == pytest.approx(1.0)
    assert split.phone.crop == pytest.approx(3.0)
    assert split.phone.x > 0.5
    assert split.desktop == (1.0, 0.0, 0.0)


def test_a_centre_only_phone_zooms_as_far_as_the_window_fits_and_the_desktop_pans():
    split = split_zoom(4.0, 0.5, 0, _CENTRE_ONLY)
    assert (split.phone.x, split.phone.y) == (0.5, 0.5)
    assert 1.0 < split.phone.ratio < 4.0
    assert split.desktop[0] > 1.0 and split.desktop[1] == pytest.approx(1.0)


def test_zoom_past_what_the_phone_allows_is_finished_on_the_desktop():
    split = split_zoom(5.0, 0, 0, _CROP_ONLY)
    assert split.phone == PhoneZoom(1.0, 2.0, 0.5, 0.5)
    assert split.desktop[0] == pytest.approx(2.5)


def test_caps_come_from_the_current_camera_entry():
    assert PhoneZoomCaps.from_camera(None) is None
    assert PhoneZoomCaps.from_camera({"id": "0"}) is None  # an older phone app
    assert PhoneZoomCaps.from_camera({"zoomRatioMax": None, "cropZoomMax": None}) is None
    assert PhoneZoomCaps.from_camera({"zoomRatioMax": 8.0, "cropZoomMax": 1.0, "freeformCrop": True}) == \
        PhoneZoomCaps(8.0, 1.0, True)


class _Ctrl:
    def __init__(self):
        self.sent = []

    def send(self, **params):
        self.sent.append(params)


def _state(**caps):
    return {"cameras": [{"id": "1", "current": False}, {"id": "0", "current": True, **caps}]}


def test_plugin_tells_the_phone_only_when_its_share_changes(transforms_plugin):
    plugin, _host, _panel = transforms_plugin
    ctrl = _Ctrl()
    plugin.on_stream_start("http://phone/v1/video", ctrl)
    assert ctrl.sent == []  # no caps yet: nothing to tell it
    plugin.on_phone_state(_state(zoomRatioMax=10.0, cropZoomMax=4.0, freeformCrop=True))
    plugin.on_phone_state(_state(zoomRatioMax=10.0, cropZoomMax=4.0, freeformCrop=True))
    plugin._zoom_slider.setValue(300)
    assert ctrl.sent == [{"action": "zoom", "ratio": 1.0, "crop": 1.0, "x": 0.5, "y": 0.5},
                         {"action": "zoom", "ratio": 3.0, "crop": 1.0, "x": 0.5, "y": 0.5}]
    assert plugin._desktop_crop == (1.0, 0.0, 0.0)


def test_plugin_crops_everything_itself_without_a_phone_that_zooms(transforms_plugin):
    plugin, _host, _panel = transforms_plugin
    ctrl = _Ctrl()
    plugin.on_stream_start("http://phone/v1/video", ctrl)
    plugin.on_phone_state(_state(zoomRatioMax=4.0))
    plugin._zoom_slider.setValue(200)
    plugin.on_stream_stop()
    assert plugin._desktop_crop == (2.0, 0.0, 0.0)
    plugin.on_stream_start("http://phone/v1/video", ctrl)
    plugin._bus.camera_switched.emit({"id": "2"})  # a lens that can't zoom itself
    assert plugin._desktop_crop == (2.0, 0.0, 0.0)
    assert [p["ratio"] for p in ctrl.sent] == [1.0, 2.0]  # nothing new for the lens that can't zoom


def test_a_picked_point_only_undoes_the_desktop_share_of_the_zoom(transforms_plugin):
    plugin, _host, _panel = transforms_plugin
    seen = []
    plugin._bus.focus_point.connect(lambda x, y: seen.append((round(x, 3), round(y, 3))))
    plugin.on_stream_start("http://phone/v1/video", _Ctrl())
    plugin.on_phone_state(_state(zoomRatioMax=10.0))
    plugin._zoom_slider.setValue(200)
    plugin.process_frame(np.zeros((90, 160, 3), np.uint8))
    plugin._bus.focus_point_picked.emit(0.25, 0.5)
    assert seen == [(0.25, 0.5)]  # the phone zoomed, so the point is already in its stream frame
