"""Preview overlays: layers drawn on the preview only, and points and areas picked there through the view."""

import time

import cv2
import numpy as np
import pytest
from PyQt6.QtCore import QRectF, Qt
from PyQt6.QtGui import QImage, QPainter

from telescope.overlays import Pick, PreviewLayer, PreviewOverlays
from telescope.view import View, inverse_map, to_full, to_shown

SIZE = (640.0, 360.0)  # the frame's size on screen, px


def _flat(points):
    return [c for point in points for c in point]


class _Views:
    def __init__(self, view=View()):
        self.view = view

    def __call__(self):
        return self.view


@pytest.fixture
def overlays(qapp):
    views = _Views()
    made = PreviewOverlays(views)
    made.views = views
    made.attach()
    yield made
    made.shutdown()


def _picker(overlays, kind, **kw):
    got = []
    assert overlays.start_pick("Test", kind, got.append, **kw)
    return got


VIEWS = [View(), View(zoom=2.0, pan_x=0.5, pan_y=-0.3), View(flip_h=True), View(flip_v=True),
         View(rotation=cv2.ROTATE_90_CLOCKWISE), View(rotation=cv2.ROTATE_180),
         View(zoom=1.6, pan_x=-1.0, flip_h=True, rotation=cv2.ROTATE_90_COUNTERCLOCKWISE)]


@pytest.mark.parametrize("view", VIEWS)
def test_a_point_comes_back_where_it_was_clicked_and_in_the_whole_picture(overlays, view):
    overlays.views.view = view
    got = _picker(overlays, "point")
    overlays.press(0.3, 0.7, SIZE)
    overlays.release(0.3, 0.7)
    (pick,) = got
    assert pick.kind == "point"
    assert pick.shown[0] == pytest.approx((0.3, 0.7))
    assert pick.full[0] == pytest.approx(to_full(view, 0.3, 0.7))
    assert to_shown(view, *pick.full[0]) == pytest.approx((0.3, 0.7))


@pytest.mark.parametrize("view", [v for v in VIEWS if v.zoom == 1.0])
def test_without_zoom_the_whole_picture_is_the_phone_frame_focus_uses(overlays, view):
    w, h = 1920, 1080
    assert to_full(view, 0.2, 0.9) == pytest.approx(inverse_map(0.2, 0.9, w, h, flip_h=view.flip_h, flip_v=view.flip_v,
                                                                rotation=view.rotation))


def test_zoom_maps_onto_the_part_of_the_picture_shown(overlays):
    overlays.views.view = View(zoom=2.0)
    got = _picker(overlays, "point")
    overlays.press(0.0, 0.0, SIZE)
    overlays.release(0.0, 0.0)
    assert got[0].full[0] == pytest.approx((0.25, 0.25))


def test_a_rectangle_is_dragged_out_and_comes_back_sorted(overlays):
    got = _picker(overlays, "rect")
    assert overlays.press(0.8, 0.6, SIZE)
    overlays.move(0.2, 0.1, SIZE)
    overlays.release(0.2, 0.1)
    (pick,) = got
    assert _flat(pick.shown) == pytest.approx([0.2, 0.1, 0.8, 0.6])


def test_a_rotated_rectangle_is_sorted_in_each_space_of_its_own(overlays):
    view = View(rotation=cv2.ROTATE_90_CLOCKWISE, flip_h=True)
    overlays.views.view = view
    got = _picker(overlays, "rect")
    overlays.press(0.1, 0.2, SIZE)
    overlays.move(0.5, 0.9, SIZE)
    overlays.release(0.5, 0.9)
    (pick,) = got
    (x0, y0), (x1, y1) = pick.full
    assert x0 < x1 and y0 < y1
    corners = [to_full(view, u, v) for u in (0.1, 0.5) for v in (0.2, 0.9)]
    assert (x0, y0) == pytest.approx((min(c[0] for c in corners), min(c[1] for c in corners)))


def test_a_click_is_not_an_area(overlays):
    got = _picker(overlays, "rect")
    overlays.press(0.5, 0.5, SIZE)
    overlays.release(0.5, 0.5)
    assert got == [] and overlays.picking() == "Test"  # still waiting for a real drag
    assert not overlays.finish_pick("Test")


def test_quad_corners_start_inset_move_and_finish_with_enter(overlays):
    got = _picker(overlays, "quad")
    assert _flat(overlays._shown_points()) == pytest.approx(_flat([(0.2, 0.2), (0.8, 0.2), (0.8, 0.8), (0.2, 0.8)]))
    assert overlays.press(0.205, 0.2, SIZE)  # near the first corner grabs it
    overlays.move(0.05, 0.1, SIZE)
    overlays.release(0.05, 0.1)
    assert got == []  # a quad waits to be finished
    assert overlays.key(Qt.Key.Key_Return)
    (pick,) = got
    assert pick.kind == "quad" and pick.shown[0] == pytest.approx((0.05, 0.1))
    assert overlays.picking() == ""


def test_dragging_inside_a_quad_moves_all_of_it_and_outside_is_left_to_the_preview(overlays):
    got = _picker(overlays, "quad")
    assert not overlays.press(0.05, 0.05, SIZE)  # outside: the preview pans
    assert overlays.press(0.5, 0.5, SIZE)
    overlays.move(0.6, 0.55, SIZE)
    overlays.release(0.6, 0.55)
    overlays.finish_pick("Test")
    assert _flat(got[0].shown) == pytest.approx([0.3, 0.25, 0.9, 0.25, 0.9, 0.85, 0.3, 0.85])


def test_dragging_a_quad_to_the_edge_stops_it_there_whole(overlays):
    got = _picker(overlays, "quad")
    assert overlays.press(0.5, 0.5, SIZE)
    overlays.move(1.5, 0.9, SIZE)  # far past the corner of the picture
    overlays.move(1.6, 0.95, SIZE)
    overlays.release(1.6, 0.95)
    overlays.finish_pick("Test")
    assert _flat(got[0].shown) == pytest.approx([0.4, 0.4, 1.0, 0.4, 1.0, 1.0, 0.4, 1.0])  # moved, not squashed


def test_a_quad_reaching_past_the_picture_does_not_jump_when_dragged(overlays):
    start = ((0.1, 0.1), (0.9, 0.1), (0.9, 0.9), (0.1, 0.9))
    overlays.views.view = View(zoom=2.0)  # shown as -0.3..1.3, past every edge
    got = _picker(overlays, "quad", start=start)
    assert overlays.press(0.5, 0.5, SIZE)
    overlays.move(0.51, 0.5, SIZE)  # right would push its right corners further out
    overlays.release(0.51, 0.5)
    overlays.finish_pick("Test")
    assert _flat(got[0].full) == pytest.approx(_flat(start))


def test_a_quad_past_one_edge_still_moves_away_from_it(overlays):
    start = ((0.3, 0.3), (1.0, 0.3), (1.0, 0.7), (0.3, 0.7))
    overlays.views.view = View(zoom=2.0)  # shown as 0.1..1.5 across
    got = _picker(overlays, "quad", start=start)
    assert overlays.press(0.5, 0.5, SIZE)
    overlays.move(0.4, 0.5, SIZE)
    overlays.release(0.4, 0.5)
    overlays.finish_pick("Test")
    assert got[0].full[0] == pytest.approx((0.25, 0.3))


def test_quad_corners_stay_on_the_picture_when_the_view_moves(overlays):
    start = ((0.1, 0.1), (0.9, 0.1), (0.9, 0.9), (0.1, 0.9))
    got = _picker(overlays, "quad", start=start)
    overlays.views.view = View(zoom=2.0, flip_h=True)
    overlays.finish_pick("Test")
    (pick,) = got
    assert _flat(pick.full) == pytest.approx(_flat(start))
    assert pick.shown[0] == pytest.approx((1.0, 0.0))  # off the zoomed-in picture: held at its edge


def test_escape_another_pick_or_the_owner_cancels(overlays):
    first = _picker(overlays, "point")
    second = []
    overlays.start_pick("Other", "rect", second.append)
    assert first == [None] and overlays.picking() == "Other"
    overlays.cancel_pick("Test")
    assert overlays.picking() == "Other"  # only its owner's pick
    assert overlays.key(Qt.Key.Key_Escape)
    assert second == [None] and overlays.picking() == ""
    assert not overlays.key(Qt.Key.Key_Escape)


def test_nothing_to_pick_on_without_a_preview(qapp):
    bare = PreviewOverlays(View)
    assert not bare.start_pick("Test", "point", lambda _pick: None)
    with pytest.raises(ValueError):
        bare.attach() or bare.start_pick("Test", "circle", lambda _pick: None)


def test_pick_changed_says_who_picks(overlays):
    heard = []
    overlays.pick_changed.connect(heard.append)
    _picker(overlays, "point")
    overlays.cancel_pick()
    assert heard == ["Test", ""]


def test_a_failing_on_done_does_not_break_the_next_pick(overlays):
    def broken(_pick):
        raise RuntimeError("plugin bug")
    overlays.start_pick("Test", "point", broken)
    overlays.press(0.5, 0.5, SIZE)
    overlays.release(0.5, 0.5)
    assert overlays.picking() == ""
    assert _picker(overlays, "point") == []


class _Tint(PreviewLayer):
    def __init__(self, value, order=0, analyse_fps=0.0):
        self.value, self.order, self.analyse_fps = value, order, analyse_fps
        self.analysed, self.painted = [], 0

    def tint(self, frame):
        frame[0, 0] = self.value
        return frame

    def analyse(self, frame):
        self.analysed.append(frame[0, 0, 0])
        return True

    def paint(self, painter, rect):
        self.painted += 1


class _Broken(PreviewLayer):
    def tint(self, frame):
        raise RuntimeError("plugin bug")

    def paint(self, painter, rect):
        raise RuntimeError("plugin bug")


def test_layers_tint_in_order_and_a_broken_one_leaves_the_frame_alone(overlays):
    overlays.add_layer("late", _Tint(200, order=5))
    overlays.add_layer("early", _Tint(100, order=-5))
    overlays.add_layer("broken", _Broken())
    out = overlays.tint(np.zeros((4, 4, 3), np.uint8))
    assert out[0, 0, 0] == 200  # the later layer tints over the earlier one
    overlays.remove_layer("late")
    assert overlays.tint(np.zeros((4, 4, 3), np.uint8))[0, 0, 0] == 100


def test_paint_reaches_every_layer_even_past_a_broken_one(overlays):
    good = _Tint(1)
    overlays.add_layer("broken", _Broken())
    overlays.add_layer("good", good)
    image = QImage(64, 36, QImage.Format.Format_ARGB32)
    painter = QPainter(image)
    overlays.paint(painter, QRectF(0, 0, 64, 36))
    painter.end()
    assert good.painted == 1


def test_analyse_gets_the_frame_before_tint_on_its_own_thread(overlays, qapp):
    layer = _Tint(77, analyse_fps=1000)
    overlays.add_layer("scope", layer)
    repaints = []
    overlays.changed.connect(lambda: repaints.append(1))
    frame = np.full((4, 4, 3), 5, np.uint8)
    overlays.tint(frame)
    deadline = time.monotonic() + 2
    while not layer.analysed and time.monotonic() < deadline:
        time.sleep(0.01)
    assert layer.analysed == [5]
    deadline = time.monotonic() + 2
    while not repaints and time.monotonic() < deadline:
        qapp.processEvents()
    assert repaints


def test_no_analysis_while_no_layer_wants_it(overlays):
    overlays.add_layer("plain", _Tint(1))
    overlays.tint(np.zeros((4, 4, 3), np.uint8))
    assert overlays._thread is None


def test_a_pick_carries_the_picture_shown_before_any_tint(overlays):
    overlays.add_layer("red", _Tint(255))
    got = _picker(overlays, "point")
    overlays.tint(np.full((4, 4, 3), 7, np.uint8))
    overlays.press(0.5, 0.5, SIZE)
    overlays.release(0.5, 0.5)
    assert got[0].picture is not None and (got[0].picture == 7).all()
    assert overlays._picture is None  # nothing kept once it's over


def test_a_pick_result_is_frozen(overlays):
    pick = Pick("point", ((0.5, 0.5),), ((0.5, 0.5),))
    with pytest.raises(Exception):
        pick.kind = "rect"
