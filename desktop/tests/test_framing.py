import pytest

from telescope import vision
from telescope.plugin import EventBus
from telescope.plugins import framing
from telescope.plugins.framing import (
    FramingPlugin, Spring, Target, _Latch, fit, follow, hands_out, smoothed, target_box, tuning,
)
from telescope.view import View, window_centre
from telescope.vision import Person

_ASPECT = 16 / 9


def _person(x=0.5, y=0.4, width=0.1, left_wrist=None, hips=True, left_elbow=None, seen=0.99):
    """Someone facing the camera: shoulders width apart (of the picture's width) at height y. Their left is on the
    right of the picture; left_wrist and left_elbow are (x, y), seen how sure the model is of those two."""
    points = {
        "nose": (x, y - 0.12, 0.99), "left_eye": (x + 0.01, y - 0.14, 0.99), "right_eye": (x - 0.01, y - 0.14, 0.99),
        "left_shoulder": (x + width / 2, y, 0.99), "right_shoulder": (x - width / 2, y, 0.99),
        "right_wrist": (x - width / 2, y + 0.3, 0.99),
    }
    if hips:
        points.update({"left_hip": (x + width / 3, y + 0.35, 0.9), "right_hip": (x - width / 3, y + 0.35, 0.9)})
    if left_wrist:
        points["left_wrist"] = (*left_wrist, seen)
    if left_elbow:
        points["left_elbow"] = (*left_elbow, seen)
    return Person(points)


def test_the_box_takes_in_the_head_and_shoulders_and_upper_body_takes_more():
    head = target_box([_person()], _ASPECT, 1.0)
    body = target_box([_person()], _ASPECT, 2.0)
    assert head[1] < 0.26 and head[0] < 0.45 < 0.55 < head[2]
    assert body[3] > head[3] and body[2] - body[0] > head[2] - head[0]
    assert target_box([], _ASPECT) is None
    assert target_box([Person({"nose": (0.5, 0.5, 0.99)})], _ASPECT) is None  # no shoulders to go by


def test_framing_goes_smoothly_from_the_face_to_the_whole_body():
    feet = _person()
    feet = Person({**feet.points, "left_ankle": (0.53, 0.95, 0.9), "right_ankle": (0.47, 0.95, 0.9)})
    heights = [target_box([feet], _ASPECT, f / 10)[3] for f in range(31)]
    assert all(a <= b + 1e-9 for a, b in zip(heights, heights[1:]))
    assert heights[0] < 0.4 < 0.75 <= heights[20] and heights[30] > 0.95
    assert target_box([feet], _ASPECT, 0.0)[1] == target_box([feet], _ASPECT, 3.0)[1]  # the top stays above the head


def test_two_people_get_one_box_around_both():
    box = target_box([_person(x=0.3), _person(x=0.7)], _ASPECT)
    assert box[0] < 0.25 and box[2] > 0.75


def test_the_window_keeps_the_box_and_stays_in_the_picture():
    target = fit((0.4, 0.2, 0.6, 0.5))
    assert target.zoom == pytest.approx(1 / 0.3) and target.x == pytest.approx(0.5)
    corner = fit((0.0, 0.0, 0.1, 0.1))
    assert corner.zoom == framing._MAX_ZOOM and corner.x == pytest.approx(0.125) and corner.y == pytest.approx(0.125)
    assert fit((-0.2, 0.0, 1.2, 1.0)).zoom == 1.0


def test_a_hand_held_out_widens_the_box_on_its_side_once_it_counts():
    out = _person(left_wrist=(0.66, 0.45), left_elbow=(0.6, 0.47))
    assert hands_out([out], _ASPECT) == {"left"} and hands_out([_person()], _ASPECT) == set()
    assert hands_out([_person(left_wrist=(0.75, 0.8))], _ASPECT) == set()  # down by the hip: not showing anything
    assert target_box([out], _ASPECT, hands={"left"})[2] > 0.66 > target_box([out], _ASPECT)[2]


def test_an_arm_reaching_past_the_edge_of_the_picture_counts():
    # the phone only sends a little more than the framing, so the hand is a guess past the edge, the elbow in view
    reaching = _person(left_wrist=(0.69, 0.44), left_elbow=(0.62, 0.46), seen=0.3)
    assert hands_out([reaching], _ASPECT) == {"left"}
    elbow_only = _person(left_wrist=(0.66, 0.42), left_elbow=(0.61, 0.47), seen=0.3)
    assert hands_out([elbow_only], _ASPECT) == {"left"}


def test_hands_on_hips_or_hanging_down_dont_count():
    on_hips = _person(left_wrist=(0.56, 0.74), left_elbow=(0.62, 0.6))
    assert hands_out([on_hips], _ASPECT) == set()
    hanging = _person(left_wrist=(0.57, 0.7), left_elbow=(0.56, 0.55))
    assert hands_out([hanging], _ASPECT) == set()
    head = target_box([hanging], _ASPECT, hands={"left"})
    assert head[3] < 0.6  # a hand down by the side doesn't stretch the framing to it


def test_the_hand_latch_waits_before_switching_both_ways():
    latch = _Latch(0.3, 0.8)
    assert not latch.update(True, 0.0) and not latch.update(True, 0.2)
    assert latch.update(True, 0.35)
    assert latch.update(False, 0.4) and latch.update(False, 1.0)
    assert not latch.update(False, 1.25)


def test_small_moves_stay_put_and_a_steady_move_is_followed_without_hops():
    goal = Target(0.5, 0.5, 2.0)
    assert follow(goal, Target(0.51, 0.5, 2.1)) == goal
    xs, target = [], 0.5
    for _ in range(60):
        target += 0.005
        goal = follow(goal, Target(target, 0.5, 2.0))
        xs.append(goal.x)
    steps = [b - a for a, b in zip(xs, xs[1:])]
    assert xs[-1] > 0.75 and max(steps) <= 0.005 + 1e-9  # it never catches up in one go
    assert follow(Target(0.5, 0.5, 2.0), Target(0.5, 0.5, 3.0)).zoom == pytest.approx(3.0 / 1.12)


def test_smoothing_takes_the_wobble_out_of_the_target():
    last = None
    for i in range(30):
        last = smoothed(last, Target(0.5 + (0.02 if i % 2 else -0.02), 0.5, 2.0), 1 / 30, 0.3)
    assert abs(last.x - 0.5) < 0.005


def test_speed_goes_from_calm_and_patient_to_quick_and_close():
    calm, middle, quick = tuning(1), tuning(5), tuning(10)
    assert calm[0] < middle[0] < quick[0]
    assert calm[2] > middle[2] > quick[2]  # a calmer framing lets you move further before it follows
    assert middle[0] == pytest.approx(1.6, rel=0.05)


def test_the_spring_settles_without_overshooting():
    spring, goal = Spring(Target(0.5, 0.5, 1.0)), Target(0.7, 0.4, 3.0)
    xs = [spring.step(goal, 1 / 30, 4.0).x for _ in range(150)]
    assert all(a <= b + 1e-9 for a, b in zip(xs, xs[1:])) and max(xs) <= 0.7 + 1e-6
    assert spring.at.x == pytest.approx(0.7, abs=1e-3) and spring.at.zoom == pytest.approx(3.0, abs=1e-2)


class _Host:
    def __init__(self, owner=""):
        self._view = View()
        self.owner, self.wanted, self.steered = owner, set(), []

    def view(self):
        return self._view

    def view_aspect(self):
        return _ASPECT

    def take_view(self, owner):
        if self.owner and self.owner != owner:
            return False
        self.owner = owner
        return True

    def release_view(self, owner):
        if self.owner == owner:
            self.owner = ""

    def steer_view(self, owner, zoom, pan_x, pan_y):
        self.steered.append((zoom, pan_x, pan_y))

    def want_people(self, owner, on):
        (self.wanted.add if on else self.wanted.discard)(owner)

    def vision_state(self):
        return vision.READY

    def focused_source_id(self):
        return "phone"

    def schedule_save(self):
        pass


@pytest.fixture
def card(qapp, monkeypatch):
    clock = [10.0]
    monkeypatch.setattr(framing.time, "monotonic", lambda: clock[0])

    def make(**host_kw):
        host, bus = _Host(**host_kw), EventBus()
        plugin = FramingPlugin()
        plugin.setup(host, bus)
        plugin.create_panel()
        plugin.set_config({})
        return plugin, host, bus, clock
    return make


def _run(plugin, clock, seconds, bus=None, people=None):
    for _ in range(round(seconds * 30)):
        clock[0] += 1 / 30
        if bus is not None:
            bus.people_seen.emit("phone", people)
        plugin._tick()


def test_switching_on_takes_the_view_and_switching_off_gives_it_back(card):
    plugin, host, _bus, _clock = card()
    assert host.owner == "" and not host.wanted
    plugin._switch.setChecked(True)
    assert host.owner == "Auto framing" and host.wanted == {"Auto framing"}
    plugin._switch.setChecked(False)
    assert host.owner == "" and not host.wanted and not plugin._timer.isActive()


def test_it_frames_the_person_on_the_shown_stream_and_goes_wide_when_they_leave(card):
    plugin, host, bus, clock = card()
    plugin._switch.setChecked(True)
    bus.people_seen.emit("another phone", [_person(x=0.2)])
    _run(plugin, clock, 3, bus, [_person(x=0.6)])
    zoom, pan_x, _pan_y = host.steered[-1]
    assert zoom > 2.0 and pan_x > 0
    host._view = View(zoom=zoom, pan_x=pan_x)
    _run(plugin, clock, 10)  # nobody seen since
    assert host.steered[-1][0] == pytest.approx(1.0, abs=0.02)


def test_a_flipped_view_pans_the_other_way_to_the_same_person(card):
    plugin, host, bus, clock = card()
    host._view = View(flip_h=True)
    plugin._switch.setChecked(True)
    _run(plugin, clock, 3, bus, [_person(x=0.6)])
    assert host.steered[-1][1] < 0


def test_it_says_so_when_something_else_has_the_view(card):
    plugin, host, _bus, _clock = card(owner="Something")
    plugin._switch.setChecked(True)
    assert host.owner == "Something" and not host.wanted
    assert "Something else" in plugin._status.text()


def test_settings_round_trip_and_damaged_ones_fall_back(card):
    plugin, host, _bus, _clock = card()
    plugin.set_config({"on": True, "framing": 2.5, "speed": 9, "hand": False})
    assert plugin.get_config() == {"on": True, "framing": 2.5, "speed": 9, "hand": False}
    assert plugin._framing_lbl.text() in ("Waist", "Body")
    assert host.owner == "Auto framing"
    plugin.set_config({"on": "yes", "framing": "head", "speed": 30})
    assert plugin.get_config() == {"on": False, "framing": 1.0, "speed": 5, "hand": True}
    assert plugin._framing_lbl.text() == "Head"
    assert host.owner == ""


def test_shutting_down_gives_the_view_back(card):
    plugin, host, _bus, _clock = card()
    plugin._switch.setChecked(True)
    plugin.shutdown()
    assert host.owner == ""


def _right_edge(steered):
    zoom, pan_x, _pan_y = steered
    return window_centre(zoom, pan_x) + 0.5 / zoom


def test_holding_a_hand_out_gets_it_in_the_picture_quickly_even_at_the_calmest_speed(card):
    plugin, host, bus, clock = card()
    plugin._switch.setChecked(True)
    plugin._speed_slider.setValue(1)
    _run(plugin, clock, 20, bus, [_person()])
    assert _right_edge(host.steered[-1]) < 0.69
    _run(plugin, clock, 1.2, bus, [_person(left_wrist=(0.69, 0.44), left_elbow=(0.62, 0.46), seen=0.3)])
    assert _right_edge(host.steered[-1]) > 0.72  # the hand and a bit of room for what's in it
