import cv2
import numpy as np
import pytest

from telescope.plugin import EventBus
import telescope.plugins.transforms as transforms_mod
from telescope.theme import ACCENT, ERR
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

from telescope.plugins.transforms import (  # noqa: E402
    PhoneZoom, PhoneZoomCaps, lens_note, lens_step, split_zoom,
)

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
    plugin.on_stream_start("http://phone/v1/video", ctrl)
    assert ctrl.sent == []  # no caps yet: nothing to tell it
    plugin.on_phone_state(_state(zoomRatioMax=10.0, cropZoomMax=4.0, freeformCrop=True))
    plugin.on_phone_state(_state(zoomRatioMax=10.0, cropZoomMax=4.0, freeformCrop=True))
    plugin._zoom_slider.setValue(300)
    assert ctrl.sent == [{"action": "zoom", "ratio": 1.0, "crop": 1.0, "x": 0.5, "y": 0.5},
                         {"action": "zoom", "ratio": 1.0, "crop": 3.0, "x": 0.5, "y": 0.5}]
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
    assert ctrl.sent == [{"action": "zoom", "ratio": 1.0, "crop": 1.0, "x": 0.5, "y": 0.5}]  # a ratio alone picks no lens


def test_a_picked_point_only_undoes_the_desktop_share_of_the_zoom(transforms_plugin):
    plugin, _host, _panel = transforms_plugin
    seen = []
    plugin._bus.focus_point.connect(lambda x, y: seen.append((round(x, 3), round(y, 3))))
    plugin.on_stream_start("http://phone/v1/video", _Ctrl())
    plugin.on_phone_state(_state(zoomRatioMax=10.0, cropZoomMax=4.0, freeformCrop=True))
    plugin._zoom_slider.setValue(200)
    plugin.process_frame(np.zeros((90, 160, 3), np.uint8))
    plugin._bus.focus_point_picked.emit(0.25, 0.5)
    assert seen == [(0.25, 0.5)]  # the phone zoomed, so the point is already in its stream frame


def test_lens_note_says_nothing_on_the_default_camera():
    assert lens_note(2.0, 1.0, _TELE, "~24mm OIS", "~24mm OIS", "") == ("", "")
    assert lens_note(3.0, 1.0, None, "", "", "") == ("", "")


def test_lens_note_says_when_the_phone_switched():
    level, note = lens_note(4.0, 3.7, _TELE, "Tele ~85mm", "~24mm OIS", "Tele ~85mm")
    assert level == "on"
    assert note.startswith("Switched to Tele ~85mm")
    assert "let it settle" in note


def test_lens_note_says_when_panning_fell_back_to_the_main_camera():
    level, note = lens_note(4.0, 1.0, _TELE, "~24mm OIS", "~24mm OIS", "Tele ~85mm")
    assert level == "off"
    assert "Panned past what Tele ~85mm can see, so it's using ~24mm OIS for now" in note
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


def test_the_lens_dot_shows_only_when_the_phone_switched_or_fell_back(transforms_plugin):
    plugin, _host, _panel = transforms_plugin
    plugin.on_stream_start("http://phone/v1/video", _Ctrl())
    plugin.on_phone_state(_lens_state("2"))  # unzoomed: learns the default camera
    assert plugin._lens_dot.isHidden()
    plugin._zoom_slider.setValue(400)
    plugin.on_phone_state(_lens_state("4"))
    assert not plugin._lens_dot.isHidden()
    assert plugin._lens_dot._color.name() == ACCENT
    assert plugin._lens_dot.toolTip().startswith("Switched to Tele ~85mm OIS")
    plugin._pan_x_slider._slider.setValue(plugin._pan_x_slider._slider.maximum())
    plugin.on_phone_state(_lens_state("2"))
    assert "Panned past what Tele ~85mm OIS can see" in plugin._lens_dot.toolTip()
    assert plugin._lens_dot._color.name() == ERR
    plugin._zoom_slider.setValue(200)
    assert plugin._lens_dot.isHidden()
    assert plugin._zoom_slider.toolTip().startswith("Zoomed on the phone's sensor\nDots mark")


def test_the_dot_goes_red_once_the_phone_had_time_to_switch_and_didnt(transforms_plugin, monkeypatch):
    plugin, _host, _panel = transforms_plugin
    now = [100.0]
    monkeypatch.setattr(transforms_mod.time, "monotonic", lambda: now[0])
    plugin.on_stream_start("http://phone/v1/video", _Ctrl())
    plugin.on_phone_state(_lens_state("2"))
    plugin._zoom_slider.setValue(400)
    plugin.on_phone_state(_lens_state("2"))
    assert plugin._lens_dot.isHidden()  # just asked: give it a moment
    now[0] += 3.0
    plugin._settle_timer.timeout.emit()
    assert not plugin._lens_dot.isHidden()
    assert "stayed on ~24mm OIS by itself" in plugin._lens_dot.toolTip()


def test_the_zoom_slider_marks_each_lens(transforms_plugin):
    plugin, _host, _panel = transforms_plugin
    plugin.on_stream_start("http://phone/v1/video", _Ctrl())
    plugin.on_phone_state(_lens_state("2"))
    assert plugin._zoom_slider.marks() == [370]
    plugin.on_stream_stop()
    assert plugin._zoom_slider.marks() == []


def test_dragging_the_zoom_sticks_to_a_lens_but_keys_dont(transforms_plugin):
    plugin, _host, _panel = transforms_plugin
    plugin.on_stream_start("http://phone/v1/video", _Ctrl())
    plugin.on_phone_state(_lens_state("2"))
    slider = plugin._zoom_slider
    slider.setValue(362)
    assert plugin.zoom == 3.62  # not dragging
    slider.setSliderDown(True)
    slider.setValue(363)
    assert slider.value() == 370 and plugin.zoom == 3.7
    slider.setValue(381)
    assert slider.value() == 381  # far enough to leave it
    slider.setSliderDown(False)


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

from telescope.plugins.transforms import (  # noqa: E402
    lens_boxes, pan_for, shown_to_view, view_to_shown, zoom_detent,
)

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
    plugin.on_stream_start("http://phone/v1/video", _Ctrl())
    plugin.on_phone_state(_lens_state("2"))  # a lens at 3.7x
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
    plugin.on_stream_start("http://phone/v1/video", _Ctrl())
    plugin.on_phone_state(_lens_state("2"))
    assert boxes[-1][0][0][0] == "3.7×" and boxes[-1][1] is False  # new lens data: no flash
    plugin._zoom_slider.setValue(200)
    assert boxes[-1][1] is True and pannable[-1] is True
    plugin.on_stream_stop()
    assert boxes[-1] == ([], False)
