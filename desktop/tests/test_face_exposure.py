from dataclasses import replace

import pytest

from telescope import vision
from telescope.people import Track
from telescope.plugin import EventBus
from telescope.plugins import face_exposure
from telescope.plugins.face_exposure import DEAD_ZONE, EVERY_S, SETTLE_S, FaceExposurePlugin, FaceMeter
from telescope.view import View


def _run(meter, face, start, seconds, every=1 / 12):
    """Feeds the same face for seconds; returns [(time, point)] for each move."""
    moves, t = [], start
    while t < start + seconds - 1e-9:
        point = meter.update(face, t)
        if point is not None:
            moves.append((t, point))
        t += every
    return moves


def test_the_first_move_waits_for_the_face_to_settle():
    meter = FaceMeter()
    moves = _run(meter, (0.4, 0.3), 0.0, 2.0)
    assert len(moves) == 1 and moves[0][0] >= SETTLE_S - 1e-9 and moves[0][1] == pytest.approx((0.4, 0.3))


def test_small_head_moves_dont_move_the_point():
    meter = FaceMeter()
    _run(meter, (0.4, 0.3), 0.0, 2.0)
    wobble = []
    for i in range(100):
        face = (0.4 + (0.04 if i % 2 else -0.04), 0.3 + (0.03 if i % 3 else 0.0))
        if meter.update(face, 2.0 + i / 12) is not None:
            wobble.append(face)
    assert wobble == [] and meter.point == pytest.approx((0.4, 0.3))


def test_a_real_move_is_followed_slowly_and_not_too_often():
    meter = FaceMeter()
    _run(meter, (0.3, 0.4), 0.0, 2.0)
    moves = _run(meter, (0.7, 0.4), 2.0, 10.0)
    assert moves, "it follows in the end"
    assert moves[0][0] >= 2.0 + SETTLE_S - 1e-9  # not before the face has settled
    assert all(b[0] - a[0] >= EVERY_S - 1e-9 for a, b in zip(moves, moves[1:]))
    assert meter.point[0] == pytest.approx(0.7, abs=DEAD_ZONE)


def test_walking_in_moves_the_point_once_you_settle():
    meter = FaceMeter()
    _run(meter, (0.4, 0.3), 0.0, 2.0)
    walk = [meter.update((0.4 + i * 0.02, 0.3), 2.0 + i / 12) for i in range(15)]  # across the room
    assert walk == [None] * 15
    moves = _run(meter, (0.7, 0.3), 2.0 + 15 / 12, 5.0)
    assert len(moves) == 1 and moves[0][1][0] == pytest.approx(0.7, abs=0.02)


def test_leaving_the_picture_holds_the_point():
    meter = FaceMeter()
    _run(meter, (0.4, 0.3), 0.0, 2.0)
    assert _run(meter, None, 2.0, 20.0) == []
    assert meter.point == pytest.approx((0.4, 0.3))
    back = _run(meter, (0.42, 0.31), 22.0, 3.0)  # back in the same chair: nothing to do
    assert back == []


class _Host:
    def __init__(self):
        self.wanted = set()
        self._view = View()

    def view(self):
        return self._view

    def want_people(self, owner, on):
        (self.wanted.add if on else self.wanted.discard)(owner)

    def vision_state(self):
        return vision.READY

    def focused_source_id(self):
        return "phone"

    def is_streaming(self):
        return True

    def schedule_save(self):
        pass


@pytest.fixture
def card(qt_widgets, monkeypatch):
    clock = [100.0]
    monkeypatch.setattr(face_exposure.time, "monotonic", lambda: clock[0])
    host, bus = _Host(), EventBus()
    plugin = FaceExposurePlugin()
    plugin.setup(host, bus)
    qt_widgets(plugin.create_panel())
    plugin.set_config({})
    picked = []
    bus.focus_point_picked.connect(lambda x, y: picked.append((x, y)))
    bus.focus_point_available.emit(True)
    bus.phones_changed.emit(1)

    def see(face, seconds=1.0, source="phone"):
        for _ in range(round(seconds * 12)):
            clock[0] += 1 / 12
            track = Track(1, (face[0] - 0.1, face[1] - 0.1, face[0] + 0.1, face[1] + 0.5), face, 5.0, 0.0)
            bus.people_tracked.emit(source, [track], 1)
    yield plugin, host, bus, picked, see
    plugin.shutdown()


def test_switching_on_asks_for_people_and_meters_on_the_face(card):
    plugin, host, _bus, picked, see = card
    see((0.4, 0.3))
    assert picked == [] and host.wanted == set()  # off by default
    plugin._switch.setChecked(True)
    assert host.wanted == {"Expose for my face"}
    see((0.4, 0.3))
    assert picked == [pytest.approx((0.4, 0.3))]
    see((0.4, 0.3), source="other")
    assert len(picked) == 1
    plugin._switch.setChecked(False)
    assert host.wanted == set()


def test_the_point_goes_where_the_face_is_in_the_picture_as_shown(card):
    plugin, host, _bus, picked, see = card
    host._view = replace(View(), flip_h=True, zoom=2.0)
    plugin._switch.setChecked(True)
    see((0.4, 0.3))
    # People are found in the picture as shown (mirrored too), so only the zoom moves it: twice as far from the middle
    assert picked == [pytest.approx((0.3, 0.1))]
    picked.clear()
    plugin._meter.reset()
    see((0.1, 0.5))  # outside the window shown: no point there
    assert picked == []


def test_a_point_picked_by_hand_wins_until_switched_off_and_on(card):
    plugin, _host, bus, picked, see = card
    plugin._switch.setChecked(True)
    see((0.4, 0.3))
    bus.focus_point_picked.emit(0.9, 0.9)  # clicked on the preview
    picked.clear()
    see((0.8, 0.3), seconds=5)
    assert picked == [] and plugin._follow_btn.isVisibleTo(plugin._follow_btn.parentWidget())
    plugin._switch.setChecked(False)
    plugin._switch.setChecked(True)
    see((0.8, 0.3))
    assert picked == [pytest.approx((0.8, 0.3))]


def test_picking_a_focus_mode_by_hand_wins_too_and_follow_my_face_comes_back(card):
    plugin, _host, bus, picked, see = card
    plugin._switch.setChecked(True)
    bus.focus_mode_picked.emit("auto")
    see((0.4, 0.3))
    assert picked == []
    plugin._follow_btn.click()
    see((0.4, 0.3))
    assert picked == [pytest.approx((0.4, 0.3))]


def test_picking_manual_by_hand_shows_the_manual_focus_note(card):
    plugin, _host, bus, picked, see = card
    plugin._switch.setChecked(True)
    bus.focus_mode_picked.emit("manual")
    bus.focus_mode_changed.emit("manual")
    see((0.4, 0.3))
    assert picked == [] and "Manual" in plugin._manual_note.text()
    assert plugin.diagnostics()["Expose for my face"] == "waiting (manual focus)"


def test_nothing_is_sent_while_the_lens_cant_meter_on_a_point(card):
    plugin, _host, bus, picked, see = card
    plugin._switch.setChecked(True)
    bus.focus_point_available.emit(False)
    see((0.4, 0.3))
    assert picked == [] and "can't meter" in plugin._status.text()


def test_a_lens_that_can_only_meter_gets_a_meter_point_and_focus_is_left_alone(card):
    plugin, _host, bus, picked, see = card
    metered = []
    bus.meter_point_picked.connect(lambda x, y: metered.append((x, y)))
    plugin._switch.setChecked(True)
    bus.focus_point_available.emit(False)  # a fixed-focus front camera
    bus.meter_point_available.emit(True)
    see((0.4, 0.3))
    assert picked == [] and metered == [pytest.approx((0.4, 0.3))]
    assert "can't meter" not in plugin._status.text()
    bus.focus_point_available.emit(False)  # the phone's next state says the same: the point isn't sent again
    bus.meter_point_available.emit(True)
    see((0.4, 0.3), seconds=3)
    assert len(metered) == 1
    bus.camera_switched.emit({})  # another lens forgets the point
    see((0.4, 0.3))
    assert len(metered) == 2


def test_manual_focus_on_the_camera_card_is_left_alone_until_follow_my_face(card):
    plugin, _host, bus, picked, see = card
    bus.focus_mode_changed.emit("manual")  # restored from the saved settings
    plugin._switch.setChecked(True)
    see((0.4, 0.3))
    assert picked == [] and "Manual" in plugin._manual_note.text()
    assert plugin._follow_btn.isVisibleTo(plugin._follow_btn.parentWidget())
    plugin._follow_btn.click()
    see((0.4, 0.3))
    assert picked == [pytest.approx((0.4, 0.3))]
    bus.focus_mode_changed.emit("point")  # the Camera card followed the point
    bus.focus_mode_changed.emit("manual")  # and someone set Manual again by preset
    picked.clear()
    plugin._meter.reset()
    see((0.6, 0.3))
    assert picked == [pytest.approx((0.6, 0.3))]  # Follow my face still stands until switched off and on
