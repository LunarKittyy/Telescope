"""Desk and document mode: the corners, the warp that straightens a sheet, the clean-up and the card."""

import cv2
import numpy as np
import pytest

from telescope.overlays import Pick
from telescope.plugin import EventBus
from telescope.plugins.desk import (
    DeskPlugin, Settings, Warp, black_and_white, order_corners, sheet_size, straighten, target_rect, usable_corners,
    whiten,
)
from telescope.view import View, to_full

W, H = 320, 180
CORNERS = [(110, 20), (210, 28), (222, 170), (98, 162)]  # a sheet seen at an angle, in px


def _desk_frame():
    """A dark desk with a white sheet at CORNERS and a black bar across the sheet's middle."""
    sheet = np.full((200, 150, 3), 240, np.uint8)
    sheet[95:105, 20:130] = 0
    matrix = cv2.getPerspectiveTransform(np.float32([[0, 0], [150, 0], [150, 200], [0, 200]]), np.float32(CORNERS))
    frame = np.full((H, W, 3), 40, np.uint8)
    mask = cv2.warpPerspective(np.full((200, 150), 255, np.uint8), matrix, (W, H))
    frame[mask > 0] = cv2.warpPerspective(sheet, matrix, (W, H))[mask > 0]
    return frame


def _full(points, view=View()):
    return tuple(to_full(view, x / W, y / H) for x, y in points)


def test_corners_come_out_clockwise_from_the_top_left_however_they_were_placed():
    shuffled = [CORNERS[2], CORNERS[0], CORNERS[3], CORNERS[1]]
    assert order_corners(shuffled).tolist() == [list(map(float, p)) for p in CORNERS]


def test_corners_bunched_up_crossed_or_in_a_line_are_not_a_sheet():
    assert usable_corners(_full(CORNERS))
    assert usable_corners([(0.9, 0.1), (0.1, 0.1), (0.9, 0.9), (0.1, 0.9)])  # placed in any order
    assert not usable_corners([(0.5, 0.5), (0.51, 0.5), (0.51, 0.51), (0.5, 0.51)])  # touching
    assert not usable_corners([(0.1, 0.5), (0.4, 0.5), (0.6, 0.5), (0.9, 0.5)])  # in a line
    assert not usable_corners([(0.1, 0.1), (0.9, 0.1), (0.9, 0.9), (0.5, 0.12)])  # one nearly on another's edge
    assert not usable_corners([(0.1, 0.1), (0.9, 0.1), (0.5, 0.2), (0.1, 0.9)])  # dented in


def test_the_sheet_keeps_its_shape_inside_the_frame():
    corners = order_corners(CORNERS)
    sw, sh = sheet_size(corners)
    assert sh > sw  # portrait, like the sheet
    x, y, rw, rh = target_rect(corners, W, H, keep_shape=True)
    assert rh == H and rw < W and x == (W - rw) // 2
    assert rw / rh == pytest.approx(sw / sh, abs=0.02)
    assert target_rect(corners, W, H, keep_shape=False) == (0, 0, W, H)


def test_straightening_fills_the_sheet_and_puts_the_bar_level_in_the_middle():
    settings = Settings(on=True, corners=_full(CORNERS), keep_shape=False)
    out = straighten(_desk_frame(), settings, Warp())
    assert out.shape == (H, W, 3)
    assert out[5:20, 5:-5].min() > 200  # sheet everywhere near the top, no desk
    row = out[:, W // 2, 0]
    dark = np.flatnonzero(row < 100)
    assert abs(dark.mean() - H / 2) < 6 and len(dark) < 20  # the bar, straight across the middle
    assert out[H // 2, 60:-60, 0].max() < 100  # level: dark the whole way along


def test_a_sheet_that_keeps_its_shape_sits_between_bars():
    out = straighten(_desk_frame(), Settings(on=True, corners=_full(CORNERS)), Warp())
    assert (out[:, :5] == 28).all() and (out[:, -5:] == 28).all()
    assert out[10, W // 2, 0] > 200


def test_corners_stay_on_the_sheet_when_the_view_flips():
    view = View(flip_h=True)
    flipped = cv2.flip(_desk_frame(), 1)
    mirrored = [(W - x, y) for x, y in CORNERS]
    settings = Settings(on=True, corners=_full(mirrored, view), keep_shape=False, view=view)
    straight = straighten(_desk_frame(), Settings(on=True, corners=_full(CORNERS), keep_shape=False), Warp())
    out = straighten(flipped, settings, Warp())
    assert np.abs(out.astype(int) - cv2.flip(straight, 1)).mean() < 8  # the same sheet, mirrored as shown


def test_the_warp_is_worked_out_once_until_something_changes():
    warp, settings = Warp(), Settings(on=True, corners=_full(CORNERS))
    first = warp.get(settings, W, H)
    assert warp.get(settings, W, H) is first
    assert warp.get(Settings(on=True, corners=_full(CORNERS), keep_shape=False), W, H) is not first
    assert warp.get(Settings(on=False, corners=_full(CORNERS)), W, H) is None
    assert warp.get(Settings(on=True), W, H) is None


def test_clean_up_whitens_shaded_paper_and_black_and_white_is_only_black_and_white():
    paper = np.tile(np.linspace(120, 230, 200, dtype=np.uint8), (100, 1))[..., None].repeat(3, axis=2).copy()
    paper[45:55, 50:150] = 30
    clean = whiten(paper)
    assert clean[10, 5, 0] > 200 and clean[10, 195, 0] > 200  # the dim side came up as much as the bright one
    assert clean[50, 100, 0] < 100
    bw = black_and_white(paper)
    assert set(np.unique(bw)) <= {0, 255}
    assert bw[50, 100, 0] == 0 and bw[10, 5, 0] == 255


class _Host:
    def __init__(self, can_pick=True):
        self.can_pick, self.picks, self.finished, self.cancelled, self.saves = can_pick, [], [], [], 0

    def view(self):
        return View()

    def pick_on_preview(self, owner, kind, on_done, start=None, hint=""):
        self.picks.append((owner, kind, on_done, start))
        return self.can_pick

    def finish_pick(self, owner):
        self.finished.append(owner)

    def cancel_pick(self, owner):
        self.cancelled.append(owner)

    def schedule_save(self):
        self.saves += 1

    warp = None

    def set_preview_warp(self, owner, matrix):
        self.warp = matrix


@pytest.fixture
def desk(qt_widgets):
    def make(**kw):
        host, bus = _Host(**kw), EventBus()
        plugin = DeskPlugin()
        plugin.setup(host, bus)
        qt_widgets(plugin.create_panel())
        for section in plugin.create_settings_sections():
            qt_widgets(section.widget)
        plugin.set_config({})
        return plugin, host, bus
    return make


def test_switching_on_without_corners_asks_for_them_and_a_pick_turns_it_on(desk):
    plugin, host, bus = desk()
    plugin._switch.setChecked(True)
    owner, kind, on_done, start = host.picks[-1]
    assert (owner, kind, start) == ("desk", "quad", None)
    bus.preview_pick_changed.emit("desk")
    assert plugin._done_btn.isVisibleTo(plugin._done_btn.parentWidget()) and plugin._picking
    frame = _desk_frame()
    assert plugin.process_frame(frame) is frame  # the real picture to place corners on
    on_done(Pick("quad", (), _full(CORNERS)))
    bus.preview_pick_changed.emit("")
    assert plugin._switch.isChecked() and plugin.get_config()["on"]
    assert plugin.process_frame(frame) is not frame
    plugin._mark()
    assert host.picks[-1][3] == _full(CORNERS)  # marking again starts from where they were


def test_a_cancelled_pick_changes_nothing(desk):
    plugin, host, _bus = desk()
    plugin._mark()
    host.picks[-1][2](None)
    assert plugin.get_config()["corners"] is None


def test_corners_that_make_no_sheet_are_refused_and_the_card_says_why(desk):
    plugin, host, bus = desk()
    plugin.set_config({"on": True, "corners": [list(p) for p in _full(CORNERS)]})
    plugin._mark()
    bus.preview_pick_changed.emit("desk")
    bus.preview_pick_changed.emit("")
    host.picks[-1][2](Pick("quad", (), ((0.1, 0.5), (0.4, 0.5), (0.6, 0.5), (0.9, 0.5))))
    assert plugin.get_config()["corners"] == [list(p) for p in _full(CORNERS)]  # kept what it had
    assert "in a line" in plugin._status.text() and not plugin._status.isHidden()
    plugin._mark()
    bus.preview_pick_changed.emit("desk")
    assert "in a line" not in plugin._status.text()
    plugin.set_config({"on": True, "corners": [[0.1, 0.5], [0.4, 0.5], [0.6, 0.5], [0.9, 0.5]]})
    assert plugin.get_config()["corners"] is None  # saved ones are checked too


def test_without_a_stream_the_card_says_to_start_one(desk):
    plugin, _host, _bus = desk(can_pick=False)
    plugin._mark()
    assert "Start streaming" in plugin._status.text() and not plugin._status.isHidden()


def test_settings_round_trip_and_bad_corners_are_dropped(desk):
    plugin, _host, _bus = desk()
    corners = [[0.1, 0.2], [0.9, 0.2], [0.9, 0.8], [0.1, 0.8]]
    plugin.set_config({"on": True, "corners": corners, "cleanup": "bw", "keep_shape": False})
    assert plugin.get_config() == {"on": True, "corners": corners, "cleanup": "bw", "keep_shape": False}
    assert plugin._clean_combo.currentData() == "bw" and not plugin._shape_switch.isChecked()
    plugin.set_config({"on": True, "corners": [[0, 0], [1, "x"]], "cleanup": "sepia"})
    assert plugin.get_config() == {"on": False, "corners": None, "cleanup": "off", "keep_shape": True}


def test_a_stream_behind_the_panels_keeps_the_straightening_it_had(desk):
    plugin, _host, _bus = desk()
    assert plugin.frame_step() is None
    plugin.set_config({"on": True, "corners": [list(p) for p in _full(CORNERS)]})
    step = plugin.frame_step()
    plugin.set_config({})
    assert step(_desk_frame())[10, W // 2, 0] > 200


def test_the_preview_learns_the_warp_to_map_clicks_back(desk):
    plugin, host, _bus = desk()
    plugin.set_config({"on": True, "corners": [list(p) for p in _full(CORNERS)], "keep_shape": False})
    plugin.process_frame(_desk_frame())
    tl = host.warp @ (CORNERS[0][0] / W, CORNERS[0][1] / H, 1.0)
    assert tl[:2] / tl[2] == pytest.approx((0, 0), abs=0.03)  # the sheet's corner lands in the picture's corner
    plugin.set_config({})
    plugin.process_frame(_desk_frame())
    assert host.warp is None


def test_a_sheet_pushed_off_the_picture_says_so(desk):
    plugin, _host, bus = desk()
    plugin.set_config({"on": True, "corners": [list(p) for p in _full(CORNERS)]})
    assert plugin._status.isHidden()
    bus.view_changed.emit(View(zoom=3.0))
    assert "outside the picture" in plugin._status.text() and not plugin._status.isHidden()
