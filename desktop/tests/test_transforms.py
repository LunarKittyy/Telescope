import cv2
import numpy as np
import pytest
from PyQt6.QtCore import Qt

from telescope.plugin import EventBus
import telescope.view_controller as view_controller_mod
from telescope.theme import ACCENT, ERR
from telescope.plugins.transforms import TransformsPlugin
from telescope.view import ROTATIONS, apply_zoom as _apply_zoom, transform_frame as _transform_frame
from telescope.view_controller import ViewController


class _Host:
    """The window as far as Transforms goes: its view is a real ViewController."""

    def __init__(self, bus):
        self.saves = 0
        self.vc = ViewController()
        self.vc.setup(self, bus)

    def set_view(self, **changes):
        self.vc.set_view(**changes)

    def schedule_save(self):
        self.saves += 1

    def focused_source_id(self):
        return None

    def is_streaming(self):
        return False

    def is_streaming_from(self, _source_id):
        return False


@pytest.fixture
def transforms_plugin(qt_widgets):
    bus = EventBus()
    host = _Host(bus)
    plugin = TransformsPlugin()
    plugin.setup(host, bus)
    panel = qt_widgets(plugin.create_panel())
    plugin.set_config({})  # the saved settings load, as the window does at startup
    bus.phones_changed.emit(1)
    return plugin, host, panel


def _both(plugin, hook, *args):
    getattr(plugin._host.vc, hook)(*args)
    getattr(plugin, hook)(*args)


def test_the_card_is_greyed_out_until_a_phone_is_paired(qt_widgets):
    bus = EventBus()
    plugin = TransformsPlugin()
    plugin.setup(_Host(bus), bus)
    panel = qt_widgets(plugin.create_panel())
    assert not panel.isEnabled()
    bus.phones_changed.emit(1)
    assert panel.isEnabled()


def test_auto_framing_visibly_disables_the_zoom_handle_and_value(transforms_plugin, qapp):
    from PyQt6.QtGui import QColor
    from PyQt6.QtWidgets import QStyle, QStyleOptionSlider
    from telescope import theme

    theme.apply_theme(qapp)
    plugin, host, panel = transforms_plugin
    panel.resize(420, 600)
    panel.show()
    qapp.processEvents()

    def handle_color():
        slider = plugin._zoom_slider
        option = QStyleOptionSlider()
        slider.initStyleOption(option)
        rect = slider.style().subControlRect(QStyle.ComplexControl.CC_Slider, option,
                                             QStyle.SubControl.SC_SliderHandle, slider)
        return slider.grab().toImage().pixelColor(rect.center())

    assert handle_color() == QColor(theme.hue("transforms"))
    assert host.vc.take_view("Auto framing")
    assert not plugin._zoom_slider.isEnabled() and not plugin._zoom_val_lbl.isEnabled()
    assert handle_color() == QColor(theme.TEXT_DISABLED)
    host.vc.release_view("Auto framing")
    assert plugin._zoom_slider.isEnabled() and plugin._zoom_val_lbl.isEnabled()
    assert handle_color() == QColor(theme.hue("transforms"))
    panel.close()


def test_visible_rotation_and_pan_controls_keep_their_shortcuts(transforms_plugin):
    plugin, _host, _panel = transforms_plugin
    plugin._zoom_slider.setValue(200)
    actions = {a.id: a for a in plugin.create_actions()}
    for aid, params in (("transforms.rotation", {"op": "set", "option": "90 CW"}),
                        ("transforms.pan_x", {"op": "set", "value": 50}),
                        ("transforms.pan_y", {"op": "set", "value": -25})):
        action = actions[aid]
        assert action.run(action.full_params(params)) is not None
    assert plugin.rotation == cv2.ROTATE_90_CLOCKWISE
    assert plugin.pan_x == 0.5 and plugin.pan_y == -0.25


def test_pan_pad_works_from_the_keyboard(transforms_plugin):
    from PyQt6.QtTest import QTest
    plugin, _host, _panel = transforms_plugin
    plugin._zoom_slider.setValue(200)
    pad = plugin._pan_pad
    assert pad.focusPolicy() == Qt.FocusPolicy.StrongFocus
    QTest.keyClick(pad, Qt.Key.Key_Right)
    QTest.keyClick(pad, Qt.Key.Key_Down, Qt.KeyboardModifier.ShiftModifier)
    assert plugin.pan_x > 0 and plugin.pan_y == 0.1
    QTest.keyClick(pad, Qt.Key.Key_Home)
    assert plugin.pan_x == plugin.pan_y == 0


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


def test_the_view_crops_flips_and_rotates_as_the_card_says(transforms_plugin):
    plugin, _host, _panel = transforms_plugin
    plugin._flip_h.setChecked(True)
    plugin._rot_combo.setCurrentText("90 CW")

    result = plugin._host.vc.process_frame(_grid())

    expected = cv2.rotate(cv2.flip(_grid(), 1), cv2.ROTATE_90_CLOCKWISE)
    assert np.array_equal(result, expected)


def test_a_frozen_frame_step_keeps_the_settings_it_was_taken_with(transforms_plugin):
    plugin, host, _panel = transforms_plugin
    plugin._flip_h.setChecked(True)
    step = host.vc.frame_step()
    plugin._flip_h.setChecked(False)
    plugin._rot_combo.setCurrentText("90 CW")
    assert np.array_equal(step(_grid()), cv2.flip(_grid(), 1))


def test_config_round_trip_updates_widgets(transforms_plugin):
    plugin, _host, _panel = transforms_plugin
    cfg = {
        "flip_h": True,
        "flip_v": True,
        "rotation": "180",
        "zoom": 3.0,
        "pan_x": 0.4,
        "pan_y": -0.6,
        "stretch_x": 1.5,
        "stretch_y": 0.75,
    }

    plugin.set_config(cfg)

    assert plugin.get_config() == pytest.approx(cfg)
    assert plugin._pan_x_slider._slider.isEnabled()


@pytest.mark.parametrize("zoom", [1.15, 2.03, 2.3, 4.1])
def test_a_saved_zoom_loads_exactly(transforms_plugin, zoom):
    plugin, _host, _panel = transforms_plugin
    plugin.set_config({"zoom": zoom})
    assert plugin.get_config()["zoom"] == pytest.approx(zoom)


def test_saved_pan_config_updates_runtime_processing_state(transforms_plugin):
    plugin, _host, _panel = transforms_plugin

    plugin.set_config({"zoom": 2.0, "pan_x": 0.75, "pan_y": -0.5})

    assert plugin.pan_x == pytest.approx(0.75)
    assert plugin.pan_y == pytest.approx(-0.5)


# ── Mapping a preview click back onto the phone's frame ──────────────────────

from telescope.view import inverse_map  # noqa: E402


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
    plugin._host.vc.process_frame(np.zeros((90, 160, 3), np.uint8))
    plugin._flip_h.setChecked(True)
    plugin._bus.focus_point_picked.emit(0.25, 0.5)
    assert seen == [(0.75, 0.5)]



# ── Stretch ──────────────────────────────────────────────────────────────────

from telescope.view import apply_stretch, shown_to_view, view_to_shown  # noqa: E402


def test_no_stretch_is_zero_copy():
    frame = _grid()
    assert apply_stretch(frame, 1.0, 1.0) is frame


def test_a_squish_leaves_black_bars_and_a_stretch_crops():
    frame = np.full((90, 160, 3), 255, np.uint8)
    frame[:, :80] = 100  # left half grey, right half white
    out = apply_stretch(frame, 0.5, 2.0)
    assert out.shape == frame.shape
    assert out[:, :38].max() == 0 and out[:, 122:].max() == 0  # squished sideways: black either side
    assert out[45, 60, 0] == 100 and out[45, 100, 0] == 255  # the middle is still the picture, halved
    assert out[0, 42:118].min() > 0 and out[-1, 42:118].min() > 0  # stretched up and down: no bars there


@pytest.mark.parametrize("stretch", [(1.0, 1.0), (1.5, 1.0), (1.0, 0.6), (0.5, 2.0), (2.0, 0.5)])
@pytest.mark.parametrize("rotation", [None, cv2.ROTATE_90_CLOCKWISE])
@pytest.mark.parametrize("zoom,pan", [(1.0, (0, 0)), (2.0, (0.2, -0.1))])
def test_inverse_map_undoes_a_stretch(stretch, rotation, zoom, pan):
    """A marked spot through the real crop, rotation and stretch, then the shown spot mapped back."""
    w, h = 160, 90
    src = (84, 47)
    frame = np.zeros((h, w, 3), np.uint8)
    frame[src[1] - 1:src[1] + 2, src[0] - 1:src[0] + 2] = 255
    out = apply_stretch(_transform_frame(_apply_zoom(frame, zoom, *pan), True, False, rotation), *stretch)
    ys, xs = np.nonzero(out[:, :, 0] > 20)
    oh, ow = out.shape[:2]
    u, v = (xs.mean() + 0.5) / ow, (ys.mean() + 0.5) / oh
    x, y = inverse_map(u, v, w, h, zoom, pan[0], pan[1], True, False, rotation, *stretch)
    assert abs(x * w - (src[0] + 0.5)) < 1.5
    assert abs(y * h - (src[1] + 0.5)) < 1.5


@pytest.mark.parametrize("stretch", [(1.5, 1.0), (0.5, 2.0)])
def test_shown_and_view_points_round_trip_through_a_stretch(stretch):
    t = (2.0, 0.3, -0.5, True, False, cv2.ROTATE_90_COUNTERCLOCKWISE, *stretch)
    for u, v in ((0.1, 0.2), (0.5, 0.5), (0.9, 0.7)):
        assert view_to_shown(*shown_to_view(u, v, *t), *t) == pytest.approx((u, v))
    assert shown_to_view(0.5, 0.5, *t) == pytest.approx(shown_to_view(0.5, 0.5, *t[:6]))  # the centre stays put


def test_the_view_stretches_the_picture_as_shown(transforms_plugin):
    plugin, host, _panel = transforms_plugin
    plugin._rot_combo.setCurrentText("90 CW")
    plugin._stretch_x_slider.setValue(150)
    plugin._stretch_y_slider.setValue(80)
    assert host.vc.view.stretch_x == pytest.approx(1.5) and host.vc.view.stretch_y == pytest.approx(0.8)
    expected = apply_stretch(cv2.rotate(_grid(), cv2.ROTATE_90_CLOCKWISE), 1.5, 0.8)
    assert np.array_equal(host.vc.process_frame(_grid()), expected)
    assert np.array_equal(host.vc.frame_step()(_grid()), expected)
    assert plugin._stretch_x_lbl.text() == "150%"


def test_a_picked_point_lands_on_the_phone_frame_through_a_stretch(transforms_plugin):
    plugin, host, _panel = transforms_plugin
    seen = []
    plugin._bus.focus_point.connect(lambda x, y: seen.append((round(x, 3), round(y, 3))))
    host.vc.process_frame(np.zeros((90, 160, 3), np.uint8))
    plugin._stretch_x_slider.setValue(200)
    plugin._stretch_y_slider.setValue(50)
    plugin._bus.focus_point_picked.emit(0.25, 0.6)
    assert seen == [(0.375, 0.7)]


def test_stretch_saves_loads_and_resets(transforms_plugin):
    plugin, host, _panel = transforms_plugin
    plugin.set_config({"stretch_x": 1.25, "stretch_y": 0.7})
    assert plugin.get_config()["stretch_x"] == pytest.approx(1.25)
    assert plugin.get_config()["stretch_y"] == pytest.approx(0.7)
    assert host.vc.view.stretch_y == pytest.approx(0.7)
    plugin.set_config({"stretch_x": "junk", "stretch_y": 9})
    assert (plugin.stretch_x, plugin.stretch_y) == (1.0, 2.0)
    plugin._reset_btn.click()
    assert (host.vc.view.stretch_x, host.vc.view.stretch_y) == (1.0, 1.0)
    assert plugin.get_config()["stretch_x"] == 1.0

# ── Splitting zoom between the phone and this computer ───────────────────────

from telescope.plugins.transforms import lens_note  # noqa: E402
from telescope.view import PhoneZoom, PhoneZoomCaps, lens_step, split_zoom  # noqa: E402

_TELE = PhoneZoomCaps(ratio_max=10.0, crop_max=4.0, freeform=True, lens_zooms=(3.7,))
_CENTRE_ONLY = PhoneZoomCaps(ratio_max=10.0, crop_max=4.0, freeform=False, lens_zooms=(3.7,))
_CROP_ONLY = PhoneZoomCaps(ratio_max=1.0, crop_max=2.0, freeform=True)
_ONE_LENS = PhoneZoomCaps(ratio_max=10.0, crop_max=4.0, freeform=True)


def _framing(split):
    """(size, centre_x, centre_y) the phone's crop and the desktop's remainder end up showing, 0..1."""
    phone = split.phone or PhoneZoom()
    view = 1.0 / (phone.ratio * phone.crop)
    vx = 0.5 + (phone.x - 0.5) / phone.ratio
    vy = 0.5 + (phone.y - 0.5) / phone.ratio
    zoom, pan_x, pan_y = split.desktop
    step = view * (1.0 - 1.0 / zoom) / 2.0
    return view / zoom, vx + pan_x * step, vy + pan_y * step


@pytest.mark.parametrize("caps", [None, _TELE, _CENTRE_ONLY, _CROP_ONLY, _ONE_LENS, PhoneZoomCaps(1.5, 1.0, False)])
@pytest.mark.parametrize("zoom,pan", [(1.0, (0, 0)), (2.0, (0, 0)), (3.0, (1, 0)), (5.0, (-0.4, 0.8)), (2.5, (1, -1)),
                                      (4.5, (0, 0)), (5.0, (0.3, -0.2))])
@pytest.mark.parametrize("current", [1.0, 3.7])
def test_split_zoom_always_frames_what_was_asked(caps, zoom, pan, current):
    size, cx, cy = _framing(split_zoom(zoom, *pan, caps, current))
    assert size == pytest.approx(1.0 / zoom, abs=1e-3)
    assert cx == pytest.approx(0.5 + pan[0] * (1 - 1 / zoom) / 2, abs=1e-3)
    assert cy == pytest.approx(0.5 + pan[1] * (1 - 1 / zoom) / 2, abs=1e-3)


def test_a_lens_that_cant_zoom_leaves_it_all_to_the_desktop():
    split = split_zoom(2.5, 0.3, -0.2, None)
    assert split.phone is None
    assert split.desktop == (2.5, 0.3, -0.2)


def test_below_the_telephoto_the_crop_zooms_and_the_ratio_stays_on_the_main_lens():
    split = split_zoom(3.0, 0, 0, _TELE)
    assert split.phone == PhoneZoom(1.0, 3.0, 0.5, 0.5)
    assert split.desktop == (1.0, 0.0, 0.0)


def test_past_the_telephoto_the_ratio_switches_lens_and_the_crop_does_the_rest():
    split = split_zoom(5.0, 0, 0, _TELE)
    assert split.phone.ratio == 3.7
    assert split.phone.crop == pytest.approx(5.0 / 3.7, abs=1e-3)
    assert split.desktop == (1.0, 0.0, 0.0)


def test_panning_out_of_the_telephotos_view_goes_back_to_the_main_lens():
    split = split_zoom(5.0, 1, 0, _TELE, current_ratio=3.7)
    assert split.phone.ratio == 1.0
    assert split.phone.crop == pytest.approx(4.0)  # as far as the crop goes; this computer does the rest
    assert split.phone.x > 0.5


@pytest.mark.parametrize("caps", [_TELE, _CENTRE_ONLY, _ONE_LENS])
@pytest.mark.parametrize("zoom", [2.0, 3.0, 4.5])
def test_a_pan_sweep_switches_the_ratio_at_most_at_the_lens_edges(caps, zoom):
    """The wobble: a ratio that follows the pan makes the phone animate zoom on every step."""
    ratios, current = [], 1.0
    for step in range(-20, 21):
        split = split_zoom(zoom, step / 20, step / 40, caps, current)
        current = split.phone.ratio
        ratios.append(current)
    changes = sum(a != b for a, b in zip(ratios, ratios[1:]))
    assert set(ratios) <= {1.0, *caps.lens_zooms}
    assert changes <= 2  # into the telephoto's view and back out, nothing in between


def test_the_lens_doesnt_flip_back_and_forth_at_its_edge():
    pan = 0.0526  # at 4.5x the window then fits a 3.8x view: inside the telephoto's, short of the margin
    assert split_zoom(4.5, pan, 0, _TELE, current_ratio=1.0).phone.ratio == 1.0
    assert split_zoom(4.5, pan, 0, _TELE, current_ratio=3.7).phone.ratio == 3.7


def test_a_centred_window_just_past_a_lens_gets_back_onto_it():
    """3.8x is inside the margin, so panning out and back used to leave it stuck on the main camera."""
    out = split_zoom(3.8, 1, 0, _TELE, current_ratio=3.7).phone.ratio
    back = split_zoom(3.8, 0, 0, _TELE, current_ratio=out).phone.ratio
    assert (out, back) == (1.0, 3.7)


def test_a_centre_only_phones_share_doesnt_change_with_the_pan():
    shares = {split_zoom(3.0, step / 10, 0, _CENTRE_ONLY).phone for step in range(-10, 11)}
    assert shares == {PhoneZoom(1.0, 1.0, 0.5, 0.5)}
    assert split_zoom(4.5, 0, 0, _CENTRE_ONLY).phone == PhoneZoom(3.7, 1.0, 0.5, 0.5)


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
    assert PhoneZoomCaps.from_camera({"zoomRatioMax": 5.0, "lensZooms": [9.0, 3.7, 0.6]}).lens_zooms == (3.7,)


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
    _both(plugin, 'on_stream_start', "http://phone/v1/video", ctrl)
    assert ctrl.sent == []  # no caps yet: nothing to tell it
    _both(plugin, 'on_phone_state', _state(zoomRatioMax=10.0, cropZoomMax=4.0, freeformCrop=True))
    _both(plugin, 'on_phone_state', _state(zoomRatioMax=10.0, cropZoomMax=4.0, freeformCrop=True))
    plugin._zoom_slider.setValue(300)
    assert ctrl.sent == [{"action": "zoom", "ratio": 1.0, "crop": 1.0, "x": 0.5, "y": 0.5},
                         {"action": "zoom", "ratio": 1.0, "crop": 3.0, "x": 0.5, "y": 0.5}]
    assert plugin._host.vc._desktop_crop == (1.0, 0.0, 0.0)


def test_plugin_crops_everything_itself_without_a_phone_that_zooms(transforms_plugin):
    plugin, _host, _panel = transforms_plugin
    ctrl = _Ctrl()
    _both(plugin, 'on_stream_start', "http://phone/v1/video", ctrl)
    _both(plugin, 'on_phone_state', _state(zoomRatioMax=4.0))
    plugin._zoom_slider.setValue(200)
    _both(plugin, 'on_stream_stop')
    assert plugin._host.vc._desktop_crop == (2.0, 0.0, 0.0)
    _both(plugin, 'on_stream_start', "http://phone/v1/video", ctrl)
    plugin._bus.camera_switched.emit({"id": "2"})  # a lens that can't zoom itself
    assert plugin._host.vc._desktop_crop == (2.0, 0.0, 0.0)
    assert ctrl.sent == [{"action": "zoom", "ratio": 1.0, "crop": 1.0, "x": 0.5, "y": 0.5}]  # a ratio alone picks no lens


def test_a_picked_point_only_undoes_the_desktop_share_of_the_zoom(transforms_plugin):
    plugin, _host, _panel = transforms_plugin
    seen = []
    plugin._bus.focus_point.connect(lambda x, y: seen.append((round(x, 3), round(y, 3))))
    _both(plugin, 'on_stream_start', "http://phone/v1/video", _Ctrl())
    _both(plugin, 'on_phone_state', _state(zoomRatioMax=10.0, cropZoomMax=4.0, freeformCrop=True))
    plugin._zoom_slider.setValue(200)
    plugin._host.vc.process_frame(np.zeros((90, 160, 3), np.uint8))
    plugin._bus.focus_point_picked.emit(0.25, 0.5)
    assert seen == [(0.25, 0.5)]  # the phone zoomed, so the point is already in its stream frame


def test_lens_note_says_nothing_on_the_default_camera():
    assert lens_note(2.0, 1.0, _TELE, "~24mm OIS", "~24mm OIS", "") == ("", "")
    assert lens_note(3.0, 1.0, None, "", "", "") == ("", "")


def test_lens_note_says_when_the_phone_switched():
    level, note = lens_note(4.0, 3.7, _TELE, "Tele ~85mm", "~24mm OIS", "Tele ~85mm")
    assert level == "on"
    assert note.startswith("Switched to Tele ~85mm")
    assert "wobble for a moment" in note


def test_lens_note_says_when_panning_fell_back_to_the_main_camera():
    level, note = lens_note(4.0, 1.0, _TELE, "~24mm OIS", "~24mm OIS", "Tele ~85mm")
    assert level == "off"
    assert "Panned past what Tele ~85mm can see, so it's on ~24mm OIS" in note
    assert "the telephoto can see" in lens_note(4.0, 1.0, _TELE, "", "", "")[1]


def test_lens_note_says_when_the_phone_stayed_on_the_main_camera_by_itself():
    level, note = lens_note(4.0, 3.7, _TELE, "~24mm OIS", "~24mm OIS", "Tele ~85mm")
    assert level == "off"
    assert "The phone stayed on ~24mm OIS by itself" in note
    # not while a switch may still be under way, nor when the phone doesn't say which lens it's on
    assert lens_note(4.0, 3.7, _TELE, "~24mm OIS", "~24mm OIS", "", settled=False) == ("", "")
    assert lens_note(4.0, 3.7, _TELE, "", "", "") == ("", "")


def test_red_wins_when_the_phone_is_off_the_lens_after_switching():
    level, _note = lens_note(4.0, 1.0, _TELE, "Tele ~85mm", "~24mm OIS", "Tele ~85mm")
    assert level == "off"  # still on the tele, but the pan already asked for the main camera


def test_a_lens_step_never_lands_under_the_lens():
    assert lens_step(3.0) == 300
    assert lens_step(3.004) == 301
    assert lens_step(3.7) == 370


def _lens_state(active):
    state = _state(zoomRatioMax=10.0, cropZoomMax=4.0, freeformCrop=True, lensZooms=[3.7])
    state["cameras"] += [{"id": "2", "current": False, "label": "Back ~24mm OIS [phys]"},
                         {"id": "4", "current": False, "label": "Back Telephoto ~85mm OIS [phys]"}]
    state["active_lens"] = active
    return state


def _clock(monkeypatch):
    now = [100.0]
    monkeypatch.setattr(view_controller_mod.time, "monotonic", lambda: now[0])
    return now


def _settled_state(plugin, now, active):
    """A phone state from after the ratio sent last had time to take."""
    now[0] += 3.0
    _both(plugin, 'on_phone_state', _lens_state(active))


def test_the_lens_dot_shows_only_when_the_phone_switched_or_fell_back(transforms_plugin, monkeypatch):
    plugin, _host, _panel = transforms_plugin
    now = _clock(monkeypatch)
    _both(plugin, 'on_stream_start', "http://phone/v1/video", _Ctrl())
    _both(plugin, 'on_phone_state', _lens_state("2"))
    _settled_state(plugin, now, "2")  # unzoomed: learns the default camera
    assert plugin._lens_dot.isHidden()
    plugin._zoom_slider.setValue(400)
    _settled_state(plugin, now, "4")
    assert not plugin._lens_dot.isHidden()
    assert plugin._lens_dot._color.name() == ACCENT
    assert plugin._lens_dot.toolTip().startswith("Switched to Tele ~85mm OIS")
    plugin._pan_x_slider._slider.setValue(plugin._pan_x_slider._slider.maximum())
    _settled_state(plugin, now, "2")
    assert "Panned past what Tele ~85mm OIS can see" in plugin._lens_dot.toolTip()
    assert plugin._lens_dot._color.name() == ERR
    plugin._zoom_slider.setValue(200)
    assert plugin._lens_dot.isHidden()
    assert plugin._zoom_slider.toolTip().startswith("Zoomed on the phone's sensor\nDots mark")


def test_the_dot_goes_red_once_the_phone_had_time_to_switch_and_didnt(transforms_plugin, monkeypatch):
    plugin, _host, _panel = transforms_plugin
    now = _clock(monkeypatch)
    _both(plugin, 'on_stream_start', "http://phone/v1/video", _Ctrl())
    _both(plugin, 'on_phone_state', _lens_state("2"))
    _settled_state(plugin, now, "2")
    plugin._zoom_slider.setValue(400)
    _both(plugin, 'on_phone_state', _lens_state("2"))
    assert plugin._lens_dot.isHidden()  # just asked: give it a moment
    now[0] += 3.0
    plugin._settle_timer.timeout.emit()
    assert not plugin._lens_dot.isHidden()
    assert "stayed on ~24mm OIS by itself" in plugin._lens_dot.toolTip()


def test_sweeping_the_zoom_under_a_lens_and_back_doesnt_make_the_telephoto_the_unzoomed_lens(
        transforms_plugin, monkeypatch):
    plugin, _host, _panel = transforms_plugin
    now = _clock(monkeypatch)
    _both(plugin, 'on_stream_start', "http://phone/v1/video", _Ctrl())
    _both(plugin, 'on_phone_state', _lens_state("2"))
    _settled_state(plugin, now, "2")
    plugin._zoom_slider.setValue(500)
    _settled_state(plugin, now, "4")
    for _ in range(3):  # quick back and forth, dipping under 3.7x: the phone is still on the tele meanwhile
        plugin._zoom_slider.setValue(350)
        now[0] += 0.3
        _both(plugin, 'on_phone_state', _lens_state("4"))
        plugin._zoom_slider.setValue(600)
        now[0] += 0.3
        _both(plugin, 'on_phone_state', _lens_state("4"))
    _settled_state(plugin, now, "4")
    plugin._settle_timer.timeout.emit()
    assert plugin._lens_dot._color.name() == ACCENT
    assert plugin._lens_dot.toolTip().startswith("Switched to Tele ~85mm OIS")


def test_the_zoom_slider_marks_each_lens(transforms_plugin):
    plugin, _host, _panel = transforms_plugin
    _both(plugin, 'on_stream_start', "http://phone/v1/video", _Ctrl())
    _both(plugin, 'on_phone_state', _lens_state("2"))
    assert plugin._zoom_slider.snaps() == [370]
    _both(plugin, 'on_stream_stop')
    assert plugin._zoom_slider.snaps() == []


def _drag_slider(slider, *values):
    """Press the handle and drag it with the mouse to where each value sits; the values it passed through."""
    from PyQt6.QtCore import QPoint
    from PyQt6.QtTest import QTest
    y = slider.height() // 2
    seen = []
    slider.valueChanged.connect(seen.append)
    QTest.mousePress(slider, Qt.MouseButton.LeftButton, pos=QPoint(round(slider.mark_x(slider.value())), y))
    for v in values:
        QTest.mouseMove(slider, QPoint(round(slider.mark_x(v)), y))
    QTest.mouseRelease(slider, Qt.MouseButton.LeftButton, pos=QPoint(round(slider.mark_x(values[-1])), y))
    slider.valueChanged.disconnect()
    return seen


def test_dragging_the_zoom_sticks_to_a_lens_but_keys_dont(transforms_plugin):
    plugin, _host, panel = transforms_plugin
    panel.resize(420, panel.sizeHint().height())
    panel.show()
    _both(plugin, 'on_stream_start', "http://phone/v1/video", _Ctrl())
    _both(plugin, 'on_phone_state', _lens_state("2"))  # a lens at 3.7x
    slider = plugin._zoom_slider
    step_px = (slider.mark_x(1000) - slider.mark_x(100)) / 900
    near = 370 - int(3 / step_px)  # 3 px short of the mark
    _drag_slider(slider, near)
    assert slider.value() == 370 and plugin.zoom == 3.7
    _drag_slider(slider, 200)  # well clear: a themed handle can trail the mouse a little on a narrow slider
    assert slider.value() < 300
    slider.setValue(370)
    slider.setFocus()
    from PyQt6.QtTest import QTest
    QTest.keyClick(slider, Qt.Key.Key_Left)
    assert slider.value() == 369  # keys step off it freely


def test_a_double_click_resets_zoom_and_pan(transforms_plugin):
    from PyQt6.QtTest import QTest
    plugin, _host, panel = transforms_plugin
    panel.show()
    plugin._zoom_slider.setValue(420)
    plugin._pan_x_slider._slider.setValue(120)
    QTest.mouseDClick(plugin._pan_x_slider._slider, Qt.MouseButton.LeftButton)
    assert plugin.pan_x == 0.0 and plugin.zoom == 4.2
    plugin._pan_x_slider._slider.setValue(120)
    QTest.mouseDClick(plugin._zoom_slider, Qt.MouseButton.LeftButton)
    assert plugin.zoom == 1.0 and plugin.pan_x == 0.0


def test_the_zoom_slider_follows_max_zoom(transforms_plugin):
    plugin, _host, _panel = transforms_plugin
    assert plugin._zoom_slider.maximum() == 1000  # 10x by default
    plugin._zoom_slider.setValue(800)
    plugin._bus.max_zoom_changed.emit(5)
    assert plugin._zoom_slider.maximum() == 500
    assert plugin.zoom == 5.0  # clamped, through the normal handler
    plugin._bus.max_zoom_changed.emit(20)
    plugin._zoom_slider.setValue(2000)
    assert plugin.zoom == 20.0


# ── Dragging and scrolling the preview, lens boxes ───────────────────────────

from telescope.view import lens_boxes, pan_for, zoom_detent  # noqa: E402

_ALL_TRANSFORMS = [(fh, fv, rot) for fh in (False, True) for fv in (False, True) for rot in ROTATIONS.values()]


@pytest.mark.parametrize("flip_h,flip_v,rotation", _ALL_TRANSFORMS)
def test_shown_and_view_points_map_both_ways_and_agree_with_focus(flip_h, flip_v, rotation):
    for u, v in ((0.1, 0.2), (0.5, 0.5), (0.9, 0.35), (-0.2, 1.3)):
        x, y = shown_to_view(u, v, 2.5, 0.4, -0.7, flip_h, flip_v, rotation)
        assert view_to_shown(x, y, 2.5, 0.4, -0.7, flip_h, flip_v, rotation) == pytest.approx((u, v))
    x, y = shown_to_view(0.3, 0.6, 2.0, 0.5, -0.5, flip_h, flip_v, rotation)
    assert (x, y) == pytest.approx(inverse_map(0.3, 0.6, 4000, 3000, 2.0, 0.5, -0.5, flip_h, flip_v, rotation),
                                   abs=1e-3)


def test_pan_for_is_the_inverse_of_the_window_centre_and_clamps():
    assert pan_for(0.5, 2.0) == 0.0
    assert pan_for(0.75, 2.0) == 1.0
    assert pan_for(0.9, 2.0) == 1.0
    assert pan_for(0.6, 1.0) == 0.0  # nothing to pan at 1x


def test_a_scroll_step_stops_on_the_first_lens_it_crosses():
    assert zoom_detent(290, 320, [300, 700]) == 300
    assert zoom_detent(300, 330, [300, 700]) == 330  # from the mark, it goes on
    assert zoom_detent(800, 650, [300, 700]) == 700
    assert zoom_detent(700, 640, [300, 700]) == 640
    assert zoom_detent(150, 170, [300]) == 170


def test_a_lens_box_is_the_centred_part_of_the_view_that_lens_sees():
    assert lens_boxes(None, 1.0, 0.0, 0.0) == []
    (label, *box), = lens_boxes(_TELE, 1.0, 0.0, 0.0)
    assert label == "3.7×"
    assert box == pytest.approx([0.5 - 0.5 / 3.7, 0.5 - 0.5 / 3.7, 0.5 + 0.5 / 3.7, 0.5 + 0.5 / 3.7])
    # zoomed 2x and panned fully right: the window is the right half, so the box shifts left and doubles
    (_, x0, _, x1, _), = lens_boxes(_TELE, 2.0, 1.0, 0.0)
    assert (x0, x1) == pytest.approx(((0.5 - 0.5 / 3.7 - 0.5) * 2, (0.5 + 0.5 / 3.7 - 0.5) * 2))
    # rotated a quarter turn, the box's sides swap axes
    (_, x0, y0, x1, y1), = lens_boxes(_TELE, 2.0, 1.0, 0.0, rotation=cv2.ROTATE_90_CLOCKWISE)
    assert (y0, y1) == pytest.approx(((0.5 - 0.5 / 3.7 - 0.5) * 2, (0.5 + 0.5 / 3.7 - 0.5) * 2))


def _drag_setup(transforms_plugin, rot="None", flip_h=False):
    plugin, host, _panel = transforms_plugin
    plugin._flip_h.setChecked(flip_h)
    plugin._rot_combo.setCurrentText(rot)
    plugin._zoom_slider.setValue(250)
    return plugin, host


@pytest.mark.parametrize("rot,flip_h", [("None", False), ("90 CW", False), ("180", True), ("90 CCW", True)])
def test_dragging_the_preview_moves_the_picture_with_the_mouse(transforms_plugin, rot, flip_h):
    plugin, host = _drag_setup(transforms_plugin, rot, flip_h)
    t = lambda: (plugin.zoom, plugin.pan_x, plugin.pan_y, plugin.flip_h, plugin.flip_v, plugin.rotation)  # noqa: E731
    spot = shown_to_view(0.4, 0.55, *t())  # what's under the mouse when the drag starts
    plugin._bus.view_dragged.emit(0.1, -0.05)
    assert view_to_shown(*spot, *t()) == pytest.approx((0.5, 0.5))  # it moved along with the mouse
    assert host.saves >= 1
    assert plugin.get_config()["pan_x"] == plugin.pan_x  # kept finer than the slider's steps


def test_dragging_stops_at_the_edge_and_does_nothing_unzoomed(transforms_plugin):
    plugin, _host = _drag_setup(transforms_plugin)
    plugin._bus.view_dragged.emit(-5.0, 0.0)
    assert plugin.pan_x == 1.0
    assert plugin._pan_x_lbl.text() == "+100%"
    plugin._zoom_slider.setValue(100)
    plugin._bus.view_dragged.emit(0.3, 0.3)
    assert (plugin.pan_x, plugin.pan_y) == (0.0, 0.0)


@pytest.mark.parametrize("rot", ["None", "90 CW"])
def test_scrolling_zooms_around_the_mouse(transforms_plugin, rot):
    plugin, _host = _drag_setup(transforms_plugin, rot)
    t = lambda: (plugin.zoom, plugin.pan_x, plugin.pan_y, plugin.flip_h, plugin.flip_v, plugin.rotation)  # noqa: E731
    spot = shown_to_view(0.6, 0.45, *t())
    plugin._bus.view_scrolled.emit(1.1, 0.6, 0.45)
    assert plugin.zoom == 2.75
    assert view_to_shown(*spot, *t()) == pytest.approx((0.6, 0.45), abs=0.01)  # the slider's 0.01x steps
    plugin._bus.view_scrolled.emit(1 / 1.1 ** 20, 0.6, 0.45)
    assert plugin.zoom == 1.0 and plugin.pan_x == 0.0
    for _ in range(40):
        plugin._bus.view_scrolled.emit(1.1, 0.5, 0.5)
    assert plugin.zoom == 10.0


def test_small_scrolls_add_up_and_stop_on_a_lens(transforms_plugin):
    plugin, _host, _panel = transforms_plugin
    _both(plugin, 'on_stream_start', "http://phone/v1/video", _Ctrl())
    _both(plugin, 'on_phone_state', _lens_state("2"))  # a lens at 3.7x
    plugin._zoom_slider.setValue(350)
    for _ in range(10):
        plugin._bus.view_scrolled.emit(1.002, 0.5, 0.5)  # a trackpad: well under a slider step each
    assert plugin.zoom > 3.5
    plugin._bus.view_scrolled.emit(1.1, 0.5, 0.5)
    assert plugin.zoom == 3.7  # stopped on the lens
    plugin._bus.view_scrolled.emit(1.1, 0.5, 0.5)
    assert plugin.zoom > 3.7


def test_the_preview_hears_about_the_lens_boxes_and_the_pan(transforms_plugin):
    plugin, _host, _panel = transforms_plugin
    boxes, pannable = [], []
    plugin._bus.lens_boxes.connect(lambda b, moved: boxes.append((b, moved)))
    plugin._bus.view_pannable.connect(pannable.append)
    _both(plugin, 'on_stream_start', "http://phone/v1/video", _Ctrl())
    _both(plugin, 'on_phone_state', _lens_state("2"))
    assert boxes[-1][0][0][0] == "3.7×" and boxes[-1][1] is False  # new lens data: no flash
    plugin._zoom_slider.setValue(200)
    assert boxes[-1][1] is True and pannable[-1] is True
    _both(plugin, 'on_stream_stop')
    assert boxes[-1] == ([], False)


@pytest.mark.parametrize("lens", [1.51, 2.0, 3.7, 3.9, 5.0, 6.3])
def test_a_zoom_right_on_a_lens_mark_gets_that_lens(lens):
    caps = PhoneZoomCaps(ratio_max=10.0, crop_max=4.0, freeform=True, lens_zooms=(lens,))
    zoom = lens_step(lens) / 100  # where the slider's mark (and its snap) puts it
    assert split_zoom(zoom, 0.0, 0.0, caps).phone.ratio == lens
    # and so the dot doesn't claim it fell back (it did at 3.9x: 1/(1/3.9) is a hair under 3.9)
    assert lens_note(zoom, lens, caps, "Tele", "Main", "Tele")[0] == "on"


def test_dragging_the_preview_sticks_at_the_centre_then_lets_go(transforms_plugin):
    plugin, _host = _drag_setup(transforms_plugin)  # 2.5x: a pan of 1 is 0.75 frames off centre
    plugin._bus.view_dragged.emit(-0.3, 0.0)
    assert plugin.pan_x == pytest.approx(0.4)
    seen = []
    for _ in range(60):  # back towards the centre and past it, 0.65% of the frame at a time
        plugin._bus.view_dragged.emit(0.0065, 0.0)
        seen.append(plugin.pan_x)
    stuck = [i + 1 for i, pan in enumerate(seen) if pan == 0.0]
    assert stuck == list(range(44, 50))  # 0.3 - 0.0065k within 2% of the frame either side of the centre
    assert seen[50 - 1] == pytest.approx(-0.025 / 0.75)  # lets go right where the drag is, no jump
    assert plugin.pan_y == 0.0
    plugin._pan_x_slider._slider.setValue(100)  # moved another way: the next drag starts from there
    plugin._bus.view_dragged.emit(-0.001, 0.0)
    assert plugin.pan_x > 0.5


def test_a_damaged_saved_zoom_or_pan_falls_back_instead_of_breaking_frames(transforms_plugin):
    plugin, _host, _panel = transforms_plugin
    plugin.set_config({"zoom": 2.0, "pan_x": float("nan"), "pan_y": 7})
    assert (plugin.pan_x, plugin.pan_y) == (0.0, 1.0)

    plugin.set_config({"zoom": "big"})
    assert plugin.get_config()["zoom"] == 1.0
    assert plugin._host.vc.process_frame(_grid()).shape == (4, 6, 3)


def test_back_on_a_tile_the_phone_isnt_told_its_zoom_again_but_after_a_real_stop_it_is(transforms_plugin):
    plugin, _host, _panel = transforms_plugin
    ctrl = _Ctrl()
    _both(plugin, 'on_stream_start', "http://phone/v1/video", ctrl)
    _both(plugin, 'on_phone_state', _state(zoomRatioMax=10.0, cropZoomMax=4.0, freeformCrop=True))
    _both(plugin, 'on_focus_lost')
    ctrl.sent.clear()
    _both(plugin, 'on_focus_gained', "http://phone/v1/video", ctrl)
    _both(plugin, 'on_phone_state', _state(zoomRatioMax=10.0, cropZoomMax=4.0, freeformCrop=True))
    assert ctrl.sent == []
    _both(plugin, 'on_stream_stop')
    _both(plugin, 'on_focus_gained', "http://phone/v1/video", ctrl)
    _both(plugin, 'on_phone_state', _state(zoomRatioMax=10.0, cropZoomMax=4.0, freeformCrop=True))
    assert ctrl.sent == [{"action": "zoom", "ratio": 1.0, "crop": 1.0, "x": 0.5, "y": 0.5}]


def test_a_slow_pan_moves_the_picture_evenly_instead_of_in_whole_pixels():
    small = np.random.default_rng(2).integers(0, 255, (36, 64), np.uint8)
    frame = cv2.cvtColor(cv2.resize(small, (640, 360), interpolation=cv2.INTER_CUBIC), cv2.COLOR_GRAY2BGR)
    shots = [cv2.cvtColor(_apply_zoom(frame, 1.4, -0.5 + i * 0.002, 0.0), cv2.COLOR_BGR2GRAY).astype(np.float32)
             for i in range(12)]
    steps = [cv2.phaseCorrelate(a, b)[0][0] for a, b in zip(shots, shots[1:])]
    # Each frame the window moves about a third of an output pixel: whole-pixel crops made that 0, 0, 1.4, 0...
    assert all(step < -0.05 for step in steps) and max(steps) - min(steps) < 0.04
