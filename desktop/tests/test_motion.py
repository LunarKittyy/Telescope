import numpy as np
import pytest

from telescope import motion, vision
from telescope.motion import Detector, Motion, clean_zones, grey, moving_zones, thresholds

_W, _H = 640, 360
_LEFT, _RIGHT = (0.0, 0.0, 0.4, 1.0), (0.6, 0.0, 1.0, 1.0)


def _scene(brightness=100, square_x=None, size=60):
    """A grey room with a little texture, and a bright square at square_x (pixels) if there is one."""
    rng = np.random.default_rng(1)
    frame = np.clip(brightness + rng.normal(0, 3, (_H, _W, 3)), 0, 255).astype(np.uint8)
    if square_x is not None:
        frame[150:150 + size, square_x:square_x + size] = 230
    return frame


@pytest.fixture
def watched():
    """A Motion watching the left and right of the picture for "trap", fed as stream "s"; returns (motion, events,
    feed(frame, t))."""
    events = []
    m = Motion(lambda key, owner, zones: events.append((key, owner, zones)))
    m.watch("trap", [_LEFT, _RIGHT], sensitivity=5)

    def feed(frame, t):
        here = vision.current_frame.set(("s", 0))
        try:
            m.feed(frame, now=t)
        finally:
            vision.current_frame.reset(here)
    return m, events, feed


def test_thresholds_get_finer_with_sensitivity():
    low, high = thresholds(1), thresholds(10)
    assert low[0] > high[0] and low[1] > high[1]
    assert thresholds(0) == low and thresholds(99) == high


def test_zones_are_cleaned_up():
    assert clean_zones([(0.5, 0.6, 0.1, 0.2), (0, 0, 0, 1), "nonsense", (-1, 0, 2, 0.5)]) == (
        (0.1, 0.2, 0.5, 0.6), (0.0, 0.0, 1.0, 0.5))
    assert clean_zones(None) == ()


def test_a_square_moving_in_and_out_of_a_zone(watched):
    m, events, feed = watched
    t = 0.0
    for _ in range(20):  # an empty room settles in
        feed(_scene(), t)
        t += 0.1
    assert events == []
    for x in range(400, 600, 20):  # walks across the right zone
        feed(_scene(square_x=x), t)
        t += 0.1
    assert events and events[0] == ("s", "trap", [1])
    assert m.moving("s", "trap") == [1]
    for _ in range(30):  # gone again: the zone calms down once the hold is over
        feed(_scene(), t)
        t += 0.1
    assert events[-1] == ("s", "trap", []) and m.moving("s", "trap") == []
    assert all(0 not in zones for _k, _o, zones in events)  # nothing ever moved on the left


def test_a_slow_brightness_ramp_doesnt_count(watched):
    _m, events, feed = watched
    t = 0.0
    for i in range(300):  # 30 seconds from dim to bright, like the sun coming out
        feed(_scene(brightness=60 + i * 0.4), t)
        t += 0.1
    assert events == []


def test_the_background_takes_in_something_left_behind(watched):
    _m, events, feed = watched
    t = 0.0
    for _ in range(10):
        feed(_scene(), t)
        t += 0.1
    for _ in range(100):  # a box put down on the right and left there
        feed(_scene(square_x=500), t)
        t += 0.1
    assert events[0][2] == [1] and events[-1][2] == []


def test_stopping_watching_stops_the_reports(watched):
    m, events, feed = watched
    m.watch("trap", [])
    assert not m.watching()
    feed(_scene(), 0.0)
    feed(_scene(square_x=500), 1.0)
    assert events == []


def test_small_zones_and_sensitivity():
    detector = Detector()
    detector.change(grey(_scene()), 0.0)
    change = detector.change(grey(_scene(square_x=500, size=12)), 0.1)
    small = (0.75, 0.38, 0.85, 0.5)
    assert moving_zones(change, (small, _LEFT), *thresholds(5)) == {0}
    assert moving_zones(change, (_RIGHT,), *thresholds(1)) == set()  # a tiny thing in a big zone, at the lowest


def test_zones_follow_the_phones_share_of_the_zoom():
    # The phone sends the middle half of the picture: the right zone of the whole picture is the right edge of it
    to_shown = lambda u, v: (0.25 + u * 0.5, 0.25 + v * 0.5)  # noqa: E731
    assert motion._to_frame(((0.6, 0.0, 1.0, 1.0),), to_shown) == ((0.7, 0.0, 1.0, 1.0),)
    assert motion._to_frame(((0.0, 0.0, 0.2, 1.0),), to_shown) == ((0.0, 0.0, 0.0, 0.0),)  # off the frame: never moves


def test_a_dark_coat_filling_part_of_the_picture_doesnt_trip_a_still_wall(watched):
    m, events, feed = watched
    m.watch("trap", [_LEFT, _RIGHT], sensitivity=8)
    t = 0.0
    wall = _scene(brightness=200)
    for _ in range(10):
        feed(wall, t)
        t += 0.1
    coat = wall.copy()
    coat[:, :210] = 100  # someone in a dark coat fills the left third, the right zone is a still white wall
    for _ in range(10):
        feed(coat, t)
        t += 0.1
    assert all(1 not in zones for _k, _o, zones in events)


def test_coming_back_after_a_gap_starts_the_background_over(watched):
    m, events, feed = watched
    feed(_scene(), 0.0)
    feed(_scene(), 0.1)
    feed(_scene(square_x=500), 30.0)  # the panels were on another stream for half a minute
    feed(_scene(square_x=500), 30.1)
    assert events == [] and m.moving("s", "trap") == []


def test_a_lens_switch_is_a_cut_not_motion(watched):
    _m, events, feed = watched
    for i in range(5):
        feed(_scene(brightness=60), i * 0.1)
    for i in range(5, 10):
        feed(255 - _scene(brightness=60), i * 0.1)  # everything changed at once
    assert events == []


def test_watching_again_or_turning_the_picture_starts_over(watched):
    m, events, feed = watched
    for i in range(5):
        feed(_scene(), i * 0.1)
    feed(_scene(square_x=500), 0.5)
    assert m.moving("s", "trap") == [1]
    m.watch("trap", [_LEFT, _RIGHT])
    assert m.moving("s", "trap") == []
    m.reset()
    feed(_scene(), 0.6)  # a turned picture: no difference with the background from before it
    assert m.moving("s", "trap") == []
