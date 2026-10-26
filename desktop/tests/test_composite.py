"""Several cameras in one picture: the layout maths, drawing it and keeping it in settings and presets."""

import time

import numpy as np
import pytest

from telescope import composite, vcam
from telescope.composite import Compositor, Layout, Tile
from telescope.plugin import StreamFrame


def _solid(colour, size=(1080, 1920)) -> np.ndarray:
    frame = np.empty((*size, 3), np.uint8)
    frame[:] = colour
    return frame


# ── Crop and fit ──────────────────────────────────────────────────────────

def test_a_box_the_streams_shape_shows_all_of_it():
    assert composite.crop_px(Tile("a"), 1920, 1080, 640, 360) == (0, 0, 1920, 1080)


def test_a_box_of_another_shape_cuts_evenly_from_the_middle():
    assert composite.crop_px(Tile("a"), 1920, 1080, 500, 500) == (420, 0, 1500, 1080)
    assert composite.crop_px(Tile("a"), 1080, 1920, 1600, 900) == (0, 656, 1080, 1264)


def test_zooming_in_shows_less_and_the_centre_stays_inside_the_frame():
    assert composite.crop_px(Tile("a", zoom=2.0), 1920, 1080, 640, 360) == (480, 270, 1440, 810)
    assert composite.crop_px(Tile("a", zoom=2.0, cx=1.0, cy=0.0), 1920, 1080, 640, 360) == (960, 0, 1920, 540)


def test_fit_box_keeps_the_streams_shape_inside_its_cell():
    x, y, w, h = composite.fit_box(16 / 9, (0.0, 0.0, 0.5, 1.0), 16 / 9)
    assert (x, w) == (0.0, 0.5) and h == pytest.approx(0.5) and y == pytest.approx(0.25)
    x, y, w, h = composite.fit_box(9 / 16, (0.0, 0.0, 1.0, 1.0), 16 / 9)  # a phone held upright
    assert h == 1.0 and w * 16 / 9 / h == pytest.approx(9 / 16) and x == pytest.approx((1 - w) / 2)


def test_box_px_rounds_and_stays_inside_the_picture():
    assert composite.box_px(Tile("a", "", 0.25, 0.5, 0.5, 0.5), 1920, 1080) == (480, 540, 1440, 1080)
    assert composite.box_px(Tile("a", "", 0.999, 0.999, 0.0001, 0.0001), 100, 100) == (99, 99, 100, 100)


# ── Moving, resizing, snapping and order ──────────────────────────────────

def test_a_moved_box_stays_in_the_picture():
    tile = composite.moved(Tile("a", w=0.4, h=0.3), 0.9, -0.2)
    assert (tile.x, tile.y) == (pytest.approx(0.6), 0.0)


def test_resizing_from_a_corner_moves_only_its_edges():
    assert composite.resized((0.2, 0.2, 0.4, 0.4), "br", 0.1, 0.05) == pytest.approx((0.2, 0.2, 0.5, 0.45))
    assert composite.resized((0.2, 0.2, 0.4, 0.4), "tl", -0.1, 0.1) == pytest.approx((0.1, 0.3, 0.5, 0.3))
    assert composite.resized((0.2, 0.2, 0.4, 0.4), "r", 5.0, 0.0) == pytest.approx((0.2, 0.2, 0.8, 0.4))
    x, y, w, h = composite.resized((0.2, 0.2, 0.4, 0.4), "l", 1.0, 0.0)
    assert w == pytest.approx(composite.MIN_SIZE)


def test_resizing_with_the_shape_kept_holds_its_aspect_and_fits():
    x, y, w, h = composite.resized((0.1, 0.1, 0.4, 0.2), "br", 0.2, 0.0, aspect=2.0)
    assert w / h == pytest.approx(2.0) and (x, y) == (0.1, 0.1) and w == pytest.approx(0.6)
    x, y, w, h = composite.resized((0.1, 0.1, 0.4, 0.2), "br", 2.0, 2.0, aspect=2.0)  # past the edge
    assert w / h == pytest.approx(2.0) and x + w <= 1.0 + 1e-9 and y + h <= 1.0 + 1e-9
    x, y, w, h = composite.resized((0.5, 0.5, 0.4, 0.2), "tl", -0.2, 0.0, aspect=2.0)
    assert w / h == pytest.approx(2.0) and x + w == pytest.approx(0.9) and y + h == pytest.approx(0.7)


def test_edges_snap_to_the_picture_and_other_streams():
    tiles = (Tile("a", "", 0.1, 0.1, 0.3, 0.3), Tile("b", "", 0.6, 0.2, 0.3, 0.3))
    xs, ys = composite.snap_lines(tiles, skip=1)
    assert 0.4 in xs and 0.5 in xs and 0.25 in xs and 0.1 in ys
    assert composite.snap((0.405, 0.555, 0.705), xs) == (pytest.approx(-0.005), 0.4)
    assert composite.snap((0.3,), [0.0, 1.0]) == (0.0, None)


def test_bringing_forward_and_sending_back_change_the_order():
    tiles = (Tile("a"), Tile("b"), Tile("c"))
    assert [t.source_id for t in composite.raised(tiles, 0, 1)] == ["b", "a", "c"]
    assert [t.source_id for t in composite.raised(tiles, 2, -5)] == ["c", "a", "b"]
    assert composite.raised(tiles, 7, 1) == tiles


def test_the_starting_layouts_fill_the_picture_sensibly():
    streams = [("a", "A", 16 / 9), ("b", "B", 16 / 9), ("c", "C", 9 / 16)]
    side = composite.side_by_side(streams[:2], 16 / 9)
    assert [t.y + t.h / 2 for t in side] == pytest.approx([0.5, 0.5]) and side[0].x + side[0].w < side[1].x
    big = composite.big_and_small(streams, 16 / 9)
    assert (big[0].w, big[0].h) == (1.0, 1.0) and big[1].border and big[2].y + big[2].h < big[1].y
    assert all(0 <= t.x and t.x + t.w <= 1 and 0 <= t.y and t.y + t.h <= 1 for t in big)
    assert len(composite.grid(streams, 16 / 9)) == 3 and composite.grid([], 16 / 9) == ()


def test_big_and_small_shrinks_upright_phones_so_the_stack_fits():
    streams = [("a", "A", 16 / 9)] + [(c, c, 9 / 16) for c in "bcd"]
    small = composite.big_and_small(streams, 16 / 9)[1:]
    assert all(t.y > 0 and t.y + t.h < 1 for t in small)
    assert all(upper.y + upper.h < lower.y for lower, upper in zip(small, small[1:]))  # no overlaps
    assert all(t.w / t.h * 16 / 9 == pytest.approx(9 / 16) for t in small)  # still their own shape


# ── Scaling, stretching and cropping like OBS ─────────────────────────────

WIDE = 16 / 9


def test_a_plain_drag_scales_the_stream_keeping_its_shape():
    tile = Tile("a", "", 0.2, 0.2, 0.4, 0.4)
    corner = composite.dragged(tile, "br", 0.2, 0.0, "scale", WIDE)
    assert corner.w / corner.h == pytest.approx(1.0) and corner.w == pytest.approx(0.6) and (corner.x, corner.y) == (0.2, 0.2)
    side = composite.dragged(tile, "r", 0.1, 0.0, "scale", WIDE)  # from the opposite side, centred the other way
    assert (side.x, side.w, side.h) == (0.2, pytest.approx(0.5), pytest.approx(0.5))
    assert side.y + side.h / 2 == pytest.approx(0.4)
    top = composite.dragged(tile, "t", 0.0, -0.1, "scale", WIDE)
    assert top.y + top.h == pytest.approx(0.6) and top.w == pytest.approx(0.5) and top.x + top.w / 2 == pytest.approx(0.4)
    huge = composite.dragged(tile, "l", -5.0, 0.0, "scale", WIDE)  # stops where the picture ends
    assert huge.x >= 0 and huge.y >= 0 and huge.x + huge.w <= 1 + 1e-9 and huge.y + huge.h <= 1 + 1e-9


def test_shift_stretches_the_stream_to_the_new_box():
    tile = Tile("a", "", 0.2, 0.2, 0.4, 0.4)  # a 16:9 box for a 16:9 stream
    wide = composite.dragged(tile, "r", 0.2, 0.0, "stretch", WIDE, WIDE)
    assert (wide.x, wide.y, wide.w, wide.h, wide.stretch) == (0.2, 0.2, pytest.approx(0.6), pytest.approx(0.4), True)
    assert composite.crop_px(wide, 1920, 1080, 1152, 432) == (0, 0, 1920, 1080)  # all of it, squashed


def test_stretching_a_centre_cropped_stream_keeps_what_it_showed():
    tile = Tile("a", "", 0.2, 0.2, 0.3, 0.4)  # narrower than the stream: its middle is shown
    before = composite.crop_px(tile, 1920, 1080, 576, 432)
    stretched = composite.dragged(tile, "b", 0.0, 0.0, "stretch", WIDE, WIDE)
    assert stretched.stretch and stretched.crop_l > 0 and stretched.crop_l == pytest.approx(stretched.crop_r)
    assert composite.crop_px(stretched, 1920, 1080, 576, 432) == before


def test_alt_crops_the_side_being_dragged_and_leaves_the_rest_in_place():
    tile = Tile("a", "", 0.2, 0.2, 0.4, 0.4)
    left = composite.dragged(tile, "l", 0.1, 0.0, "crop", WIDE, WIDE)
    assert (left.x, left.w, left.h) == (pytest.approx(0.3), pytest.approx(0.3), pytest.approx(0.4))
    assert (left.crop_l, left.crop_t, left.crop_r, left.crop_b) == (pytest.approx(0.25), 0.0, 0.0, 0.0)
    assert composite.crop_px(left, 1920, 1080, 576, 432) == (480, 0, 1920, 1080)  # the same size as before on screen
    corner = composite.dragged(left, "br", -0.1, -0.2, "crop", WIDE, WIDE)
    assert (corner.crop_l, corner.crop_r, corner.crop_b) == (pytest.approx(0.25), pytest.approx(0.25), pytest.approx(0.5))
    assert (corner.x, corner.y) == (pytest.approx(0.3), 0.2)


def test_cropping_outwards_brings_back_only_what_was_cut_off():
    tile = composite.dragged(Tile("a", "", 0.2, 0.2, 0.4, 0.4), "l", 0.1, 0.0, "crop", WIDE, WIDE)
    back = composite.dragged(tile, "l", -0.5, 0.0, "crop", WIDE, WIDE)
    assert back.crop_l == 0.0 and back.x == pytest.approx(0.2) and back.w == pytest.approx(0.4)
    assert composite.dragged(back, "r", 5.0, 0.0, "crop", WIDE, WIDE).w == pytest.approx(0.4)  # nothing more to show


def test_a_cropped_stream_scales_with_its_crop():
    cropped = composite.dragged(Tile("a", "", 0.2, 0.2, 0.4, 0.4), "l", 0.1, 0.0, "crop", WIDE, WIDE)
    bigger = composite.dragged(cropped, "br", 0.15, 0.0, "scale", WIDE)
    assert bigger.crop_l == cropped.crop_l and bigger.w / bigger.h == pytest.approx(cropped.w / cropped.h)


def test_stretching_or_cropping_a_stream_thats_off_only_moves_the_box():
    tile = Tile("gone", "", 0.2, 0.2, 0.4, 0.4)
    for how in ("stretch", "crop"):
        moved = composite.dragged(tile, "r", 0.1, 0.0, how, WIDE)
        assert moved.w == pytest.approx(0.5) and not moved.stretch and moved.crop_r == 0.0


def test_fitting_its_shape_undoes_stretch_crop_and_zoom():
    tile = composite.dragged(Tile("a", "", 0.2, 0.2, 0.4, 0.4, zoom=2.0), "l", 0.1, 0.0, "crop", WIDE, WIDE)
    tile = composite.dragged(tile, "t", 0.0, 0.1, "stretch", WIDE, WIDE)
    fitted = composite.fitted(tile, WIDE, WIDE)
    assert (fitted.zoom, fitted.stretch, fitted.crop_l, fitted.crop_t, fitted.crop_r, fitted.crop_b) == (1.0, False, 0, 0, 0, 0)
    assert fitted.w / fitted.h == pytest.approx(1.0)


def test_zooming_stays_inside_the_crop():
    tile = Tile("a", crop_l=0.5, zoom=2.0, cx=0.0)
    assert composite.crop_px(tile, 1920, 1080, 960, 1080) == (960, 270, 1440, 810)


# ── Saving ────────────────────────────────────────────────────────────────

def test_a_layout_goes_through_settings_unchanged():
    layout = Layout("Face big", (Tile("phone:1", "Pixel", 0.0, 0.0, 1.0, 1.0),
                                 Tile("browser:x", "Laptop", 0.7, 0.7, 0.25, 0.25, 1.5, 0.4, 0.6, 4, "#ff8800", 24)),
                    "#202020", "/home/me/bg.png")
    assert composite.clean_layout(composite.layout_dict(layout)) == layout
    cropped = Layout("Cropped", (Tile("a", crop_l=0.1, crop_t=0.2, crop_r=0.3, crop_b=0.05, stretch=True),))
    assert composite.clean_layout(composite.layout_dict(cropped)) == cropped


def test_a_layout_saved_before_crop_and_stretch_still_loads_and_looks_the_same():
    old = {"name": "Old", "background": "#202020", "picture": "", "tiles": [
        {"source_id": "a", "name": "A", "x": 0.1, "y": 0.1, "w": 0.3, "h": 0.4, "zoom": 1.5, "cx": 0.4, "cy": 0.6,
         "border": 2, "border_colour": "#ffffff", "radius": 8}]}
    tile = composite.clean_layout(old).tiles[0]
    assert (tile.crop_l, tile.crop_t, tile.crop_r, tile.crop_b, tile.stretch) == (0.0, 0.0, 0.0, 0.0, False)
    assert composite.crop_px(tile, 1920, 1080, 500, 500) == (408, 288, 1128, 1008)  # zoomed around its centre, as before


def test_broken_crop_values_still_leave_some_of_the_stream():
    tile = composite.clean_tile({"source_id": "a", "crop_l": 0.99, "crop_r": 0.5, "crop_t": -1, "crop_b": "x",
                                 "stretch": "yes"})
    assert tile.crop_l + tile.crop_r == pytest.approx(1.0 - composite.MIN_SHOWN) and tile.crop_r == 0.0
    assert (tile.crop_t, tile.crop_b, tile.stretch) == (0.0, 0.0, False)


@pytest.mark.parametrize("raw", [None, [], "x", {"tiles": "nope"}])
def test_something_that_isnt_a_layout_is_none_or_empty(raw):
    layout = composite.clean_layout(raw)
    assert layout is None or layout.tiles == ()


def test_broken_values_are_fixed_rather_than_dropped():
    layout = composite.clean_layout({"name": "<b>x</b>" + "y" * 80, "background": "red", "tiles": [
        {"source_id": "a", "x": 0.9, "w": 0.5, "h": float("nan"), "zoom": 99, "border": -3, "radius": True,
         "border_colour": "#GGGGGG"},
        {"source_id": "a"}, {"name": "no id"}, {"source_id": "b"}, {"source_id": "c"}, {"source_id": "d"},
        {"source_id": "e"}]})
    tile = layout.tiles[0]
    assert len(layout.name) == composite.MAX_NAME and "<" not in layout.name
    assert layout.background == composite.DEFAULT_BACKGROUND
    assert (tile.x, tile.w, tile.h, tile.zoom, tile.border, tile.radius, tile.border_colour) == (
        0.5, 0.5, 1.0, composite.MAX_ZOOM, 0, 0, "#ffffff")
    assert [t.source_id for t in layout.tiles] == ["a", "b", "c", "d"]  # one each, at most four


def test_saved_layouts_need_a_name_each():
    raw = [{"name": "A"}, {"name": ""}, {"name": "A", "background": "#ffffff"}, {"name": "B"}, "junk"]
    assert [layout.name for layout in composite.clean_layouts(raw)] == ["A", "B"]
    assert len(composite.clean_layouts([{"name": str(i)} for i in range(50)])) == composite.MAX_LAYOUTS


# ── Drawing ───────────────────────────────────────────────────────────────

def _render(layout, frames, size=(320, 180)):
    comp = Compositor()
    comp.prepare(layout, size)
    return comp.render(frames)


def test_each_stream_lands_in_its_box_over_the_background_back_to_front():
    layout = Layout(tiles=(Tile("a", "", 0.0, 0.0, 0.5, 1.0), Tile("b", "", 0.25, 0.25, 0.5, 0.5)), background="#0000ff")
    out = _render(layout, {"a": (_solid((0, 255, 0)), None), "b": (_solid((0, 0, 255)), None)})
    assert out[90, 20].tolist() == [0, 255, 0]  # a
    assert out[90, 160].tolist() == [0, 0, 255]  # b, in front of a
    assert out[10, 300].tolist() == [255, 0, 0]  # the background (#0000ff is BGR 255, 0, 0)


def test_a_stream_that_isnt_running_is_left_out():
    layout = Layout(tiles=(Tile("a", "", 0.0, 0.0, 0.5, 0.5), Tile("gone", "", 0.5, 0.5, 0.5, 0.5)), background="#000000")
    comp = Compositor()
    comp.prepare(layout, (100, 100))
    frames = {"a": (_solid((9, 9, 9)), None)}
    assert comp.live_tiles(frames) == 1
    assert comp.render(frames)[75, 75].tolist() == [0, 0, 0]


def test_rounded_corners_show_the_background_and_blend_at_the_curve():
    layout = Layout(tiles=(Tile("a", radius=108),), background="#000000")
    out = _render(layout, {"a": (_solid((255, 255, 255)), None)}, size=(400, 200))  # radius 20 px at 200 lines
    assert out[0, 0].tolist() == [0, 0, 0] and out[100, 200].tolist() == [255, 255, 255]
    assert out[0, 20].tolist() == [255, 255, 255]  # past the curve
    corner = out[:20, :20, 0]
    assert ((corner > 0) & (corner < 255)).sum() > 10  # smoothed, not stepped


def test_a_border_goes_inside_the_box_in_its_colour():
    layout = Layout(tiles=(Tile("a", "", 0.25, 0.25, 0.5, 0.5, border=54, border_colour="#ff0000"),),
                    background="#000000")
    out = _render(layout, {"a": (_solid((0, 255, 0)), None)}, size=(400, 200))  # 10 px at 200 lines
    assert out[52, 102].tolist() == [0, 0, 255]  # border, BGR
    assert out[100, 200].tolist() == [0, 255, 0]
    assert out[48, 98].tolist() == [0, 0, 0]


def test_a_stretched_or_cropped_stream_is_drawn_that_way():
    frame = np.empty((90, 160, 3), np.uint8)
    frame[:, :80] = (0, 0, 255)
    frame[:, 80:] = (255, 0, 0)
    comp = Compositor()
    comp.prepare(Layout(tiles=(Tile("a", "", 0.0, 0.0, 0.5, 1.0, stretch=True),
                               Tile("b", "", 0.5, 0.0, 0.5, 1.0, crop_l=0.5))), (320, 180))
    out = comp.render({"a": (frame, 1), "b": (frame, 1)})
    assert tuple(out[90, 10]) == (0, 0, 255) and tuple(out[90, 150]) == (255, 0, 0)  # both halves, squashed into it
    assert tuple(out[90, 170]) == (255, 0, 0) and tuple(out[90, 310]) == (255, 0, 0)  # only the right half


def test_corner_masks_are_worked_out_once_per_radius():
    composite.corner_masks.cache_clear()
    first = composite.corner_masks(12)
    assert composite.corner_masks(12) is first and composite.corner_masks.cache_info().hits == 1
    (tl_in, tl_out), (tr_in, _), (bl_in, _), (br_in, _) = first
    assert tl_in[0, 0] == 0 and tl_in[-1, -1] == 1 and np.allclose(tl_in + tl_out, 1)
    assert np.array_equal(tr_in, tl_in[:, ::-1]) and np.array_equal(br_in, tl_in[::-1, ::-1])
    assert np.array_equal(bl_in, tl_in[::-1, :])


def test_a_frame_seen_before_isnt_scaled_again():
    layout = Layout(tiles=(Tile("a", "", 0.0, 0.0, 0.5, 0.5), Tile("b", "", 0.5, 0.5, 0.5, 0.5)))
    comp = Compositor()
    comp.prepare(layout, (320, 180))
    frame = _solid((1, 2, 3))
    comp.render({"a": (frame, 41), "b": (frame, None)})
    scaled_a = comp._scaled[0][1]
    comp.render({"a": (frame, 41), "b": (frame, None)})
    assert comp._scaled[0][1] is scaled_a
    comp.render({"a": (frame, 42), "b": (frame, None)})
    assert comp._scaled[0][1] is not scaled_a


def test_shrinking_goes_in_halves_then_the_rest():
    frame = np.random.default_rng(1).integers(0, 255, (1080, 1920, 3), np.uint8)
    assert composite.scale_to(frame, 480, 270).shape == (270, 480, 3)
    assert composite.scale_to(frame, 333, 187).shape == (187, 333, 3)
    assert composite.scale_to(frame, 1920, 1080) is frame


def test_the_background_picture_covers_the_picture(tmp_path):
    import cv2
    path = tmp_path / "bg.png"
    cv2.imwrite(str(path), _solid((10, 20, 30), size=(40, 30)))
    out = _render(Layout(picture=str(path)), {}, size=(64, 36))
    assert out.shape == (36, 64, 3) and out[18, 32].tolist() == [10, 20, 30]
    assert _render(Layout(picture=str(tmp_path / "missing.png"), background="#ffffff"), {}, (8, 8))[0, 0].tolist() \
        == [255, 255, 255]


def test_a_missing_background_picture_is_read_again_once_its_there(tmp_path):
    import cv2
    path = tmp_path / "later.png"
    layout = Layout(picture=str(path), background="#ffffff")
    comp = Compositor()
    comp.prepare(layout, (8, 8))
    assert comp.render({})[0, 0].tolist() == [255, 255, 255]
    cv2.imwrite(str(path), _solid((10, 20, 30), size=(8, 8)))
    comp.prepare(layout, (8, 8))
    assert comp.render({})[0, 0].tolist() == [10, 20, 30]


def test_moving_a_tile_doesnt_scale_the_background_picture_again(tmp_path, monkeypatch):
    import cv2
    path = tmp_path / "bg.png"
    cv2.imwrite(str(path), _solid((10, 20, 30), size=(40, 30)))
    covered = []
    real = composite.cover
    monkeypatch.setattr(composite, "cover", lambda *a: covered.append(a[1:]) or real(*a))
    comp = Compositor()
    for x in (0.0, 0.1, 0.2):
        comp.prepare(Layout(tiles=(Tile("a", "", x, 0.0, 0.5, 0.5),), picture=str(path)), (64, 36))
    assert covered == [(64, 36)]
    comp.prepare(Layout(picture=str(path)), (32, 18))
    assert covered == [(64, 36), (32, 18)]


def test_four_1080p_streams_draw_well_inside_a_60_fps_frame():
    streams = [(c, c, 16 / 9) for c in "abcd"]
    rng = np.random.default_rng(2)
    frames = {c: (rng.integers(0, 255, (1080, 1920, 3), np.uint8), None) for c in "abcd"}
    for tiles in (composite.big_and_small(streams, 16 / 9), composite.grid(streams, 16 / 9)):
        comp = Compositor()
        comp.prepare(Layout(tiles=tiles), (1920, 1080))
        comp.render(frames)
        start = time.perf_counter()
        for _ in range(10):
            comp.render(frames)
        assert (time.perf_counter() - start) / 10 < 1 / 60


# ── The plugin ────────────────────────────────────────────────────────────

class _Host:
    def __init__(self):
        self.frames, self.current, self.saves = [], None, 0
        self.on_camera = ("a", None, None)  # what goes to the Telescope camera
        self.canvas = (320, 180)
        self.replaced = []  # (source id, frame, number) handed to replace_frame
        self.newer = False  # replace_frame finds the stream has sent a newer frame

    def stream_frames(self, copy=False):
        return list(self.frames)

    def stream_frame(self, source_id, copy=False):
        return next((sf for sf in self.frames if sf.source_id == source_id), None)

    def replace_frame(self, source_id, frame, number):
        if self.newer:
            return False
        self.replaced.append((source_id, frame, number))
        return True

    def frame_source(self):
        return self.current

    def stream_on_camera(self, slot=0):
        return self.on_camera if slot == 0 else None

    def canvas_size(self):
        return self.canvas

    def is_streaming_from(self, source_id):
        return any(sf.source_id == source_id for sf in self.frames)

    def schedule_save(self):
        self.saves += 1


class _Bus:
    class _Signal:
        def connect(self, _slot):
            pass

    def __getattr__(self, _name):
        return self._Signal()


@pytest.fixture
def plugin(qapp):
    from telescope.plugins.composite import CompositePlugin
    p = CompositePlugin()
    p.setup(_Host(), _Bus())
    p.card = p.create_panel()
    p.on_stream_start("u", None)
    yield p
    p.shutdown()


def _two_streams(plugin, b_age=0.0):
    now = time.monotonic()
    plugin._host.frames = [StreamFrame("a", "Phone A", _solid((0, 0, 0), (90, 160)), 1, now),
                           StreamFrame("b", "Phone B", _solid((0, 255, 0), (90, 160)), 2, now - b_age)]
    plugin.set_layout(Layout(tiles=(Tile("a", "Phone A", 0.0, 0.0, 0.5, 1.0), Tile("b", "Phone B", 0.5, 0.0, 0.5, 1.0))))
    plugin._switch.setChecked(True)


def test_the_picture_goes_out_on_the_main_stream_only(plugin):
    _two_streams(plugin)
    frame = _solid((255, 0, 0), (180, 320))
    plugin._host.current = "b"
    assert plugin.process_frame(frame) is frame
    plugin._host.current = "a"
    out = plugin.frame_step()(frame)
    assert out.shape == (180, 320, 3) and out[90, 40].tolist() == [255, 0, 0] and out[90, 280].tolist() == [0, 255, 0]
    assert vcam.slot_label(0) in plugin._note.text()


def test_the_picture_goes_out_on_the_telescope_camera_at_its_size(plugin):
    _two_streams(plugin)
    plugin._host.canvas = (None, None)  # Auto
    plugin._host.on_camera = ("a", 90, 160)  # a phone held upright on Telescope
    plugin._on_camera_opened("a", 0, 90, 160)
    assert plugin.size() == (90, 160)
    plugin._host.current = "a"
    assert plugin.process_frame(_solid((255, 0, 0), (160, 90))).shape == (160, 90, 3)
    # b is made the main stream (the mic moves) but still goes to its own camera: the picture stays on a's
    plugin._on_camera_opened("b", 1, 1920, 1080)
    assert plugin.size() == (90, 160)
    # a stops and b is the only stream: nothing goes to Telescope, so nothing is drawn
    plugin._host.on_camera = None
    plugin.on_stream_stop()
    frame = _solid((0, 0, 255), (180, 320))
    plugin._host.current = "b"
    assert plugin.process_frame(frame) is frame and "no stream goes there" in plugin._note.text()
    # b restarts onto Telescope, in landscape: the picture is drawn there at its new size
    plugin._host.on_camera = ("b", 320, 180)
    plugin._on_camera_opened("b", 0, 320, 180)
    assert plugin.size() == (320, 180) and plugin.process_frame(frame).shape == (180, 320, 3)


def test_while_the_telescope_stream_stalls_the_others_keep_moving(plugin):
    _two_streams(plugin)
    host = plugin._host
    host.current = "a"
    main = _solid((255, 0, 0), (90, 160))
    t0 = time.monotonic()
    plugin._main_frame = (main, t0)
    assert not plugin.draw_stalled(t0 + 0.03)  # a is on time: its own frame steps draw the picture
    assert plugin.draw_stalled(t0 + 0.1)
    sid, out, number = host.replaced[-1]
    assert (sid, number) == ("a", 1) and out[90, 40].tolist() == [255, 0, 0] and out[90, 280].tolist() == [0, 255, 0]
    assert not plugin.draw_stalled(t0 + 0.15)  # b hasn't sent a new frame either: nothing to draw again
    host.frames[1] = StreamFrame("b", "Phone B", _solid((0, 255, 255), (90, 160)), 3, t0 + 0.15)
    host.newer = True  # a sent a frame while this was drawn: that one wins
    assert not plugin.draw_stalled(t0 + 0.2)
    host.newer = False
    assert plugin.draw_stalled(t0 + 0.2) and host.replaced[-1][1][90, 280].tolist() == [0, 255, 255]
    # a gone for longer than STALE_S leaves the picture, as any other stream does
    host.frames[1] = StreamFrame("b", "Phone B", _solid((0, 255, 0), (90, 160)), 4, t0 + composite.STALE_S + 1)
    assert plugin.draw_stalled(t0 + composite.STALE_S + 1)
    out = host.replaced[-1][1]
    assert out[90, 40].tolist() == list(composite.bgr(composite.DEFAULT_BACKGROUND))


def test_the_stall_clock_draws_nothing_without_another_stream_or_a_frame(plugin):
    _two_streams(plugin, b_age=composite.STALE_S + 1)
    t0 = time.monotonic()
    plugin._main_frame = (_solid((255, 0, 0), (90, 160)), t0)
    assert not plugin.draw_stalled(t0 + 0.5)  # b dropped: a alone goes through as it is, frozen or not
    _two_streams(plugin)
    plugin._main_frame = None  # a hasn't sent a frame since the picture was switched on
    assert not plugin.draw_stalled(t0 + 0.5)
    plugin._main_frame = (_solid((255, 0, 0), (90, 160)), t0)
    plugin._switch.setChecked(False)
    assert not plugin.draw_stalled(t0 + 0.5) and plugin._host.replaced == []


def test_with_one_stream_running_or_switched_off_frames_go_through(plugin):
    _two_streams(plugin, b_age=composite.STALE_S + 1)  # b dropped a while ago
    plugin._host.current = "a"
    frame = _solid((255, 0, 0), (180, 320))
    assert plugin.process_frame(frame) is frame
    _two_streams(plugin)
    plugin._switch.setChecked(False)
    assert plugin.process_frame(frame) is frame


def test_a_closed_editor_is_freed(plugin, qapp):
    from PyQt6 import sip
    plugin.open_editor()
    editor = plugin._editor
    editor.close()
    qapp.processEvents()
    from PyQt6.QtCore import QCoreApplication, QEvent
    QCoreApplication.sendPostedEvents(None, QEvent.Type.DeferredDelete.value)
    assert plugin._editor is None and sip.isdeleted(editor)


def test_layouts_are_saved_picked_renamed_and_deleted(plugin):
    _two_streams(plugin)
    assert plugin.save_layout("  Side by side ") == "Side by side"
    assert plugin.saved_match() == "Side by side" and plugin._combo.currentText() == "Side by side"
    plugin.set_layout(Layout("Side by side", (Tile("a"),)))
    assert plugin.saved_match() == "" and plugin._combo.currentIndex() == -1  # changed since
    plugin.save_layout("Just one")
    assert plugin.pick("Side by side") and len(plugin.layout().tiles) == 2
    plugin.rename_layout("Side by side", "Two")
    assert plugin.names() == ["Two", "Just one"] and plugin.layout().name == "Two"
    plugin.delete_layout("Just one")
    assert plugin.names() == ["Two"] and not plugin.pick("Just one")


def test_settings_and_presets_bring_the_picture_back(plugin, qapp):
    from telescope.plugins.composite import CompositePlugin
    _two_streams(plugin)
    plugin.save_layout("Two")
    saved = plugin.get_config()
    other = CompositePlugin()
    other.setup(_Host(), _Bus())
    other.card = other.create_panel()
    other.set_config(saved)
    assert other.get_config() == saved and other._switch.isChecked()

    preset = dict(saved, layouts=[])  # what a preset saved, and a layout list that's changed since
    plugin.set_layout(Layout())
    plugin._switch.setChecked(False)
    plugin.apply_preset(preset)
    assert plugin.layout() == other.layout() and plugin._switch.isChecked()
    assert plugin.names() == ["Two"]  # a preset leaves the saved layouts alone


def test_a_key_picks_a_saved_layout(plugin):
    _two_streams(plugin)
    plugin.save_layout("Two")
    plugin.set_layout(Layout("One", (Tile("a"),)))
    plugin.save_layout("One")
    actions = {a.id: a for a in plugin.create_actions()}
    said = actions["composite.layout"].run({"op": "set", "option": "Two"})
    assert said and plugin.layout().name == "Two"
    actions["composite.on"].run({"op": "toggle"})
    assert plugin.get_config()["on"] is False


# ── The editor ────────────────────────────────────────────────────────────

@pytest.fixture
def editor(plugin, qapp):
    from telescope.widgets.composite_editor import CompositeEditor
    _two_streams(plugin)
    plugin._main_frame = (_solid((255, 0, 0), (90, 160)), time.monotonic() + 3600)
    ed = CompositeEditor(plugin)
    ed.resize(1100, 640)
    ed.show()
    qapp.processEvents()
    yield ed
    ed.close()
    ed.deleteLater()


def _at(view, x, y):
    from PyQt6.QtCore import QPoint
    a = view.area()
    return QPoint(round(a.x() + x * a.width()), round(a.y() + y * a.height()))


def test_dragging_a_stream_moves_it_and_snaps_to_the_middle(editor, plugin):
    from PyQt6.QtCore import Qt
    from PyQt6.QtTest import QTest
    plugin.set_layout(Layout(tiles=(Tile("a", "", 0.0, 0.0, 1.0, 1.0), Tile("b", "", 0.1, 0.1, 0.3, 0.3))))
    view = editor.view
    QTest.mousePress(view, Qt.MouseButton.LeftButton, pos=_at(view, 0.2, 0.2))
    assert view.selected == 1
    QTest.mouseMove(view, _at(view, 0.448, 0.2))  # its centre ends up just short of the middle
    QTest.mouseRelease(view, Qt.MouseButton.LeftButton, pos=_at(view, 0.448, 0.2))
    tile = plugin.layout().tiles[1]
    assert tile.x + tile.w / 2 == pytest.approx(0.5)


def _drag(view, start, end, modifiers=None):
    from PyQt6.QtCore import QPointF, Qt
    from PyQt6.QtGui import QMouseEvent
    modifiers = modifiers or Qt.KeyboardModifier.NoModifier
    left = Qt.MouseButton.LeftButton

    def send(kind, at, buttons, mods):
        pos = QPointF(_at(view, *at))
        view.event(QMouseEvent(kind, pos, view.mapToGlobal(pos), left, buttons, mods))

    send(QMouseEvent.Type.MouseButtonPress, start, left, Qt.KeyboardModifier.NoModifier)
    send(QMouseEvent.Type.MouseMove, end, left, modifiers)
    send(QMouseEvent.Type.MouseButtonRelease, end, Qt.MouseButton.NoButton, Qt.KeyboardModifier.NoModifier)


def test_dragging_a_corner_keeps_the_shape(editor, plugin):
    plugin.set_layout(Layout(tiles=(Tile("b", "", 0.1, 0.1, 0.4, 0.2),)))
    editor.view.select(0)
    _drag(editor.view, (0.5, 0.3), (0.7, 0.32))
    tile = plugin.layout().tiles[0]
    assert tile.w == pytest.approx(0.6, abs=0.01) and tile.w / tile.h == pytest.approx(2.0) and not tile.stretch


def test_shift_stretches_and_alt_crops_from_the_editor(editor, plugin):
    from PyQt6.QtCore import Qt
    plugin.set_layout(Layout(tiles=(Tile("b", "", 0.1, 0.1, 0.4, 0.4),)))
    editor.view.select(0)
    _drag(editor.view, (0.5, 0.3), (0.7, 0.3), Qt.KeyboardModifier.ShiftModifier)
    tile = plugin.layout().tiles[0]
    assert tile.stretch and tile.w == pytest.approx(0.6, abs=0.01) and tile.h == pytest.approx(0.4)
    plugin.set_layout(Layout(tiles=(Tile("b", "", 0.1, 0.1, 0.4, 0.4),)))
    _drag(editor.view, (0.1, 0.3), (0.2, 0.3), Qt.KeyboardModifier.AltModifier)
    tile = plugin.layout().tiles[0]
    assert tile.crop_l == pytest.approx(0.25, abs=0.02) and tile.x == pytest.approx(0.2, abs=0.01) and not tile.stretch


def test_ctrl_stops_snapping(editor, plugin):
    from PyQt6.QtCore import Qt
    plugin.set_layout(Layout(tiles=(Tile("a", "", 0.0, 0.0, 1.0, 1.0), Tile("b", "", 0.1, 0.1, 0.3, 0.3))))
    _drag(editor.view, (0.2, 0.2), (0.448, 0.2), Qt.KeyboardModifier.ControlModifier)
    tile = plugin.layout().tiles[1]
    assert tile.x + tile.w / 2 == pytest.approx(0.498, abs=0.003) and tile.x + tile.w / 2 != pytest.approx(0.5)
    editor.view.select(1)
    edge = tile.x + tile.w
    _drag(editor.view, (edge, 0.4), (0.745, 0.4), Qt.KeyboardModifier.ControlModifier)  # the right edge, near 0.75
    assert plugin.layout().tiles[1].x + plugin.layout().tiles[1].w != pytest.approx(0.75, abs=1e-4)


def test_scrolling_over_a_stream_zooms_into_it(editor, plugin):
    from PyQt6.QtCore import QPoint, QPointF, Qt
    from PyQt6.QtGui import QWheelEvent
    plugin.set_layout(Layout(tiles=(Tile("b", "", 0.1, 0.1, 0.4, 0.4),)))
    pos = QPointF(_at(editor.view, 0.3, 0.3))
    event = QWheelEvent(pos, editor.view.mapToGlobal(pos), QPoint(), QPoint(0, 240), Qt.MouseButton.NoButton,
                        Qt.KeyboardModifier.NoModifier, Qt.ScrollPhase.NoScrollPhase, False)
    editor.view.wheelEvent(event)
    assert plugin.layout().tiles[0].zoom == pytest.approx(1.21) and editor._zoom.value() == 121


def test_the_streams_list_puts_streams_in_and_takes_them_out(editor, plugin):
    plugin.set_layout(Layout(tiles=(Tile("a", "", 0.0, 0.0, 1.0, 1.0), Tile("gone", "Old phone"))))
    editor.reload()
    editor.reload()  # twice: the list is rebuilt, not added to
    texts = [c.text() for c in editor._stream_checks]
    assert texts == ["Phone A", "Phone B", "Old phone (not streaming)"]
    assert editor._streams_box.count() == 3
    editor._stream_checks[1].setChecked(True)
    assert [t.source_id for t in plugin.layout().tiles] == ["a", "gone", "b"] and editor.view.selected == 2
    editor._stream_checks[0].setChecked(False)
    assert [t.source_id for t in plugin.layout().tiles] == ["gone", "b"]


def test_starting_from_a_layout_and_saving_it(editor, plugin, monkeypatch):
    from telescope.widgets import composite_editor
    editor._start_btns[0].click()  # side by side
    assert [round(t.y + t.h / 2, 3) for t in plugin.layout().tiles] == [0.5, 0.5]
    monkeypatch.setattr(composite_editor.QInputDialog, "getText", lambda *a: ("Talk", True))
    editor._save_btn.click()
    assert plugin.names() == ["Talk"] and editor._layout_combo.currentText() == "Talk"
    assert editor._rename_btn.isEnabled() and editor._delete_btn.isEnabled()


def test_renaming_onto_another_layouts_name_asks_first(editor, plugin, monkeypatch):
    from telescope.widgets import composite_editor
    from PyQt6.QtWidgets import QMessageBox
    plugin.save_layout("One")
    plugin.set_layout(Layout("", (Tile("a"),)))
    plugin.save_layout("Two")
    editor.reload()
    asked = []
    monkeypatch.setattr(composite_editor.QInputDialog, "getText", lambda *a: ("One", True))
    monkeypatch.setattr(composite_editor.QMessageBox, "question",
                        lambda *a: asked.append(a) or QMessageBox.StandardButton.No)
    editor._rename_btn.click()
    assert asked and plugin.names() == ["One", "Two"] and plugin.layout().name == "Two"
    monkeypatch.setattr(composite_editor.QMessageBox, "question", lambda *a: QMessageBox.StandardButton.Yes)
    editor._rename_btn.click()
    assert plugin.names() == ["One"] and len(plugin.layout().tiles) == 1


def test_the_picked_streams_border_corners_and_order_change_from_the_side(editor, plugin):
    plugin.set_layout(Layout(tiles=(Tile("a", "", 0.0, 0.0, 1.0, 1.0), Tile("b", "", 0.6, 0.6, 0.3, 0.3))))
    editor.view.select(1)
    editor._border.setValue(6)
    editor._radius.setValue(30)
    tile = plugin.layout().tiles[1]
    assert (tile.border, tile.radius) == (6, 30) and editor._border_lbl.text() == "6 px"
    editor._back_btn.click()
    assert [t.source_id for t in plugin.layout().tiles] == ["b", "a"] and editor.view.selected == 0


def test_through_real_workers_the_main_stream_sends_the_picture_and_the_other_its_own(plugin):
    import telescope.stream as stream
    from telescope.frames import Frames
    frames = Frames()
    plugin._host.stream_frames = frames.frames
    plugin._host.frame_source = frames.source_of_current
    plugin._host.is_streaming_from = lambda sid: frames.frame(sid) is not None
    a = stream.StreamWorker("url", None, None, 30, [plugin.frame_step()])
    b = stream.StreamWorker("url", None, None, 30, [plugin.frame_step()])
    frames.set_streams([("a", "Phone A", a), ("b", "Phone B", b)])
    plugin.set_layout(Layout(tiles=(Tile("a", "", 0.0, 0.0, 0.5, 1.0), Tile("b", "", 0.5, 0.0, 0.5, 1.0))))
    plugin._switch.setChecked(True)
    b._publish(1, _solid((0, 255, 0), (90, 160)))
    a._publish(1, _solid((255, 0, 0), (90, 160)))
    assert b.latest_frame().shape == (90, 160, 3)  # its own camera still gets its own picture
    out = a.latest_frame()
    assert out.shape == (180, 320, 3) and out[90, 40].tolist() == [255, 0, 0] and out[90, 280].tolist() == [0, 255, 0]
    # a stalls and b moves on: the clock's picture goes out on a's camera, but not over a frame a sent since
    plugin._host.stream_frame = frames.frame
    plugin._host.replace_frame = frames.replace_frame
    number = frames.frame("a").number
    b._publish(2, _solid((0, 255, 255), (90, 160)))
    assert plugin.draw_stalled(time.monotonic() + 1.0)
    assert a.latest_frame()[90, 280].tolist() == [0, 255, 255] and frames.frame("a").number == number
    assert not frames.replace_frame("a", out, number - 1) and not frames.replace_frame("gone", out, number)
