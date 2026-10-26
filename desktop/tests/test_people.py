import pytest

from telescope.people import DROP_S, PeopleTracker, PeopleTracking, box_of, face_of, main
from telescope.vision import Person

_EVERY = 1 / 12  # how often the pose model reports while something moves


def _person(x=0.5, y=0.4, width=0.1, seen=0.99):
    return Person({
        "nose": (x, y - 0.12, seen), "left_eye": (x + 0.01, y - 0.14, seen), "right_eye": (x - 0.01, y - 0.14, seen),
        "left_shoulder": (x + width / 2, y, seen), "right_shoulder": (x - width / 2, y, seen),
        "left_hip": (x + width / 3, y + 0.35, seen), "right_hip": (x - width / 3, y + 0.35, seen),
    })


def _play(tracker, frames, start=0.0):
    """Feeds each update's people in turn; returns [(tracks, count)] after each."""
    out = []
    for i, people in enumerate(frames):
        tracks = tracker.update(people, start + i * _EVERY)
        out.append((tracks, tracker.count()))
    return out


def test_box_and_face_come_from_the_points_the_model_is_sure_of():
    person = _person()
    box = box_of(person)
    assert box == pytest.approx((0.45, 0.26, 0.55, 0.75))
    assert face_of(person) == pytest.approx((0.5, 0.4 - 0.4 / 3))
    assert box_of(_person(seen=0.2)) is None and face_of(_person(seen=0.2)) is None


def test_someone_new_counts_after_two_updates_in_a_row():
    tracker = PeopleTracker()
    counts = [count for _t, count in _play(tracker, [[_person()]] * 3)]
    assert counts == [0, 1, 1]


def test_one_update_with_a_ghost_doesnt_count():
    tracker = PeopleTracker()
    frames = [[_person(0.3)]] * 4 + [[_person(0.3), _person(0.8)]] + [[_person(0.3)]] * 4
    assert max(count for _t, count in _play(tracker, frames)) == 1


def test_drop_outs_keep_the_same_person_and_the_count():
    tracker = PeopleTracker()
    frames = [[_person()]] * 5 + [[]] * 10 + [[_person(0.52)]] * 3 + [[]] * 5 + [[_person()]] * 2
    results = _play(tracker, frames)
    assert [count for _t, count in results[2:]] == [1] * (len(frames) - 2)
    ids = {track.id for tracks, _c in results for track in tracks}
    assert ids == {1}
    missing = results[14][0][0]
    assert missing.missing_s == pytest.approx(10 * _EVERY)


def test_someone_gone_for_good_is_let_go_slowly():
    tracker = PeopleTracker()
    _play(tracker, [[_person()]] * 5)
    gone_at = 4 * _EVERY
    t = gone_at
    while t - gone_at < DROP_S - 0.1:
        t += _EVERY
        tracker.update([], t)
        assert tracker.count() == 1
    tracker.update([], gone_at + DROP_S + 0.01)
    assert tracker.count() == 0 and tracker.tracks(gone_at + DROP_S + 0.01) == []


def test_a_second_person_walking_behind_keeps_both_ids():
    tracker = PeopleTracker()
    frames = []
    for i in range(40):
        walker = 0.1 + i * 0.02  # left to right, behind someone sitting at 0.5
        hidden = abs(walker - 0.5) < 0.08
        frames.append([_person(0.5, width=0.14)] + ([] if hidden else [_person(walker, width=0.08)]))
    results = _play(tracker, frames)
    assert all(count == 2 for _t, count in results[2:])
    assert {track.id for tracks, _c in results for track in tracks} == {1, 2}
    end = {t.id: t.centre[0] for t in results[-1][0]}
    assert end[1] == pytest.approx(0.5, abs=0.01) and end[2] > 0.8


def test_boxes_are_smoothed_and_the_one_there_longest_is_main():
    tracker = PeopleTracker()
    _play(tracker, [[_person(0.3)]] * 6)
    tracks = tracker.update([_person(0.3), _person(0.7)], 6 * _EVERY)
    tracks = tracker.update([_person(0.4), _person(0.7)], 7 * _EVERY)
    you = main(tracks)
    assert you.id == 1 and 0.3 < you.centre[0] < 0.4  # it heads for 0.4 instead of jumping there
    assert main([]) is None


def test_two_skeletons_on_one_person_count_once():
    tracker = PeopleTracker()
    twin = _person(0.505)
    assert max(count for _t, count in _play(tracker, [[_person(), twin]] * 6)) == 1


def test_tracking_is_kept_per_stream_and_can_start_over():
    tracking = PeopleTracking()
    for i in range(3):
        tracking.update("a", [_person()], i * _EVERY)
    assert tracking.last("a")[1] == 1 and tracking.last("b") == ([], 0)
    tracking.reset()
    assert tracking.last("a") == ([], 0)
    assert tracking.update("a", [_person()], 1.0)[1] == 0
