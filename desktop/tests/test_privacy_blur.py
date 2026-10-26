import numpy as np
import pytest

from telescope import vision
from telescope.people import PeopleTracker, Track
from telescope.plugin import EventBus
from telescope.plugins import privacy_blur
from telescope.plugins.privacy_blur import OFF_S, PrivacyBlurPlugin, Trigger, boxes_mask, shown_box
from telescope.view import View
from telescope.vision import Person

_EVERY = 1 / 12


def _person(x):
    return Person({"nose": (x, 0.3, 0.99), "left_shoulder": (x + 0.05, 0.45, 0.99),
                   "right_shoulder": (x - 0.05, 0.45, 0.99), "left_hip": (x + 0.03, 0.8, 0.99)})


def test_it_blurs_once_a_second_person_is_there_two_updates_in_a_row():
    tracker, trigger, t = PeopleTracker(), Trigger(), 0.0
    for _ in range(10):
        trigger.update(len(tracker.update([_person(0.3)], t)), t)
        t += _EVERY
    tracker.update([_person(0.3), _person(0.8)], t)
    assert not trigger.update(tracker.count(), t)  # one update could be the model seeing things
    t += _EVERY
    tracker.update([_person(0.3), _person(0.8)], t)
    assert trigger.update(tracker.count(), t)


def test_it_stops_only_once_one_person_has_been_there_for_a_while():
    tracker, trigger, t = PeopleTracker(), Trigger(), 0.0
    for _ in range(12):
        tracker.update([_person(0.3), _person(0.8)], t)
        trigger.update(tracker.count(), t)
        t += _EVERY
    left = t
    while t - left < 20:
        tracker.update([_person(0.3)], t)
        if not trigger.update(tracker.count(), t):
            break
        t += _EVERY
    # The tracker lets the second person go after a few seconds, then it waits OFF_S on top
    assert t - left >= OFF_S + 2.5
    assert not trigger.on


def test_a_second_person_flickering_in_and_out_keeps_it_on():
    trigger = Trigger()
    trigger.update(2, 0.0)
    for i in range(100):
        assert trigger.update(2 if i % 30 == 0 else 1, i * 0.1)  # back every 3 s, before OFF_S runs out


def test_boxes_follow_the_view_and_fade_at_the_edges():
    box = shown_box((0.4, 0.4, 0.6, 0.6), View(zoom=2.0))
    assert box == pytest.approx((0.3, 0.3, 0.7, 0.7))
    mirrored = shown_box((0.1, 0.0, 0.3, 1.0), View(flip_h=True))
    assert mirrored == pytest.approx((0.1, 0.0, 0.3, 1.0))  # people are already found in the mirrored picture
    mask = boxes_mask([(0.5, 0.0, 1.0, 1.0)], 640, 360)
    assert mask[10, 5] < 0.05 and mask[10, -5] > 0.95 and 0.2 < mask[10, 20] < 0.8


class _Host:
    def __init__(self):
        self.wanted, self.mask = set(), None
        self.shown, self.tracked = "phone", {}

    def tracked_people(self, source_id=None):
        return self.tracked.get(source_id, ([], 0))

    def is_streaming(self):
        return True

    def view(self):
        return View()

    def want_people(self, owner, on):
        (self.wanted.add if on else self.wanted.discard)(owner)

    def person_mask(self, frame):
        return self.mask

    def vision_state(self):
        return vision.READY

    def focused_source_id(self):
        return self.shown

    def schedule_save(self):
        pass


def _frame():
    """Stripes, so a blur shows: the left half and the right half can be checked apart."""
    frame = np.zeros((360, 640, 3), np.uint8)
    frame[:, ::4] = 255
    return frame


def _sharp(frame, x0, x1):
    return frame[100:260, x0:x1].std() > 60


@pytest.fixture
def card(qt_widgets, monkeypatch):
    clock = [100.0]
    monkeypatch.setattr(privacy_blur.time, "monotonic", lambda: clock[0])
    host, bus = _Host(), EventBus()
    plugin = PrivacyBlurPlugin()
    plugin.setup(host, bus)
    qt_widgets(plugin.create_panel())
    plugin.set_config({})
    bus.phones_changed.emit(1)
    you = Track(1, (0.1, 0.2, 0.4, 0.9), (0.25, 0.3), 30.0, 0.0)
    other = Track(2, (0.65, 0.3, 0.85, 0.9), (0.75, 0.35), 1.0, 0.0)

    def see(tracks, seconds=0.5, source="phone"):
        for _ in range(round(seconds * 12)):
            clock[0] += 1 / 12
            bus.people_tracked.emit(source, tracks, len(tracks))
    yield plugin, host, bus, see, you, other
    plugin.shutdown()


def test_switched_on_it_blurs_the_other_person_but_not_you(card):
    plugin, host, _bus, see, you, other = card
    plugin._switch.setChecked(True)
    assert host.wanted == {"Privacy blur"}
    see([you])
    assert plugin.process_frame(_frame()) is not None and _sharp(plugin.process_frame(_frame()), 450, 560)
    see([you, other])
    out = plugin.process_frame(_frame())
    assert _sharp(out, 100, 220) and not _sharp(out, 450, 560)
    see([you], seconds=OFF_S - 1)
    assert not _sharp(plugin.process_frame(_frame()), 450, 560)  # still on: one person for a little while
    see([you], seconds=1.5)
    assert _sharp(plugin.process_frame(_frame()), 450, 560)


def test_all_but_me_blurs_the_room_and_leaves_you(card):
    plugin, host, _bus, see, you, other = card
    plugin._switch.setChecked(True)
    plugin._set_mode("background")
    see([you, other])
    out = plugin.process_frame(_frame())
    assert _sharp(out, 100, 220) and not _sharp(out, 450, 560) and not _sharp(out, 580, 640)
    mask = np.zeros((360, 640), np.uint8)
    mask[:, 64:256] = 255  # the person mask: you, where your box is
    host.mask = mask
    out = plugin.process_frame(_frame())
    assert _sharp(out, 100, 220) and not _sharp(out, 450, 560)


def test_blur_now_works_without_anyone_found(card):
    plugin, host, _bus, _see, _you, _other = card
    assert not plugin._switch.isChecked() and host.wanted == set()
    plugin._now_btn.click()
    out = plugin.process_frame(_frame())
    assert not _sharp(out, 100, 220) and not _sharp(out, 450, 560)  # nobody to tell apart: all of it
    plugin._now_btn.click()
    assert _sharp(plugin.process_frame(_frame()), 100, 220)


def test_the_wait_screen_is_asked_for_and_the_picture_blurred_until_it_answers(card):
    plugin, _host, bus, see, you, other = card
    asks = []
    bus.cover_requested.connect(lambda owner, on: asks.append(on))
    plugin._switch.setChecked(True)
    plugin._set_mode("wait")
    see([you, other])
    assert asks == [True]
    assert not _sharp(plugin.process_frame(_frame()), 100, 220)  # nothing answered: Wait screen is off
    assert "Wait screen" in plugin._status.text()
    bus.stream_covered.emit(True)
    assert _sharp(plugin.process_frame(_frame()), 100, 220)  # the wait screen goes over it further on
    see([you], seconds=OFF_S + 1)
    assert asks == [True, False]


def test_a_stream_left_blurred_stays_blurred_behind_the_panels(card):
    plugin, _host, _bus, see, you, other = card
    assert plugin.frame_step() is None
    plugin._switch.setChecked(True)
    see([you, other])
    step = plugin.frame_step()
    see([you], seconds=OFF_S + 1)
    assert not _sharp(step(_frame()), 450, 560)


def test_blur_now_with_only_you_found_blurs_all_but_you(card):
    plugin, _host, _bus, see, you, _other = card
    plugin._switch.setChecked(True)
    see([you])
    plugin._now_btn.click()
    out = plugin.process_frame(_frame())
    assert _sharp(out, 100, 220) and not _sharp(out, 450, 560)


def test_the_person_mask_blurs_all_of_the_other_person_not_just_their_box(card):
    plugin, host, _bus, see, you, other = card
    plugin._switch.setChecked(True)
    see([you, other])
    mask = np.zeros((360, 640), np.uint8)
    mask[:, 64:256] = 255  # you
    mask[:, 400:640] = 255  # them, wider than the landmarks found
    host.mask = mask
    out = plugin.process_frame(_frame())
    assert _sharp(out, 100, 220) and not _sharp(out, 405, 430) and not _sharp(out, 600, 640)


def test_coming_back_to_a_blurred_stream_keeps_it_blurred(card):
    plugin, host, _bus, see, you, other = card
    plugin._switch.setChecked(True)
    see([you, other])
    plugin.on_focus_lost()
    host.shown = "laptop"
    plugin.on_focus_gained("url", None)
    assert _sharp(plugin.process_frame(_frame()), 450, 560)  # nobody else on this one
    plugin.on_focus_lost()
    host.shown = "phone"
    plugin.on_focus_gained("url", None)  # back, long after the pose model was closed: no update yet
    assert not _sharp(plugin.process_frame(_frame()), 450, 560)


def test_a_stream_seen_for_the_first_time_starts_from_what_tracking_knows(card):
    plugin, host, _bus, _see, you, other = card
    plugin._switch.setChecked(True)
    host.tracked["laptop"] = ([you, other], 2)
    plugin.on_focus_lost()
    host.shown = "laptop"
    plugin.on_focus_gained("url", None)
    assert not _sharp(plugin.process_frame(_frame()), 450, 560)


def test_you_stay_you_when_your_track_is_lost_and_found_again(card):
    plugin, _host, _bus, see, you, other = card
    plugin._switch.setChecked(True)
    see([you, other])
    # The model lost both and found them again with new ids; the other one is closer and bigger now
    you_again = Track(7, you.box, you.face, 0.5, 0.0)
    closer = Track(8, (0.55, 0.1, 0.95, 1.0), (0.75, 0.2), 0.5, 0.0)
    see([closer, you_again])
    out = plugin.process_frame(_frame())
    assert _sharp(out, 100, 220) and not _sharp(out, 600, 640)


def test_someone_sitting_right_next_to_you_is_blurred_too(card):
    plugin, host, _bus, see, you, _other = card
    plugin._switch.setChecked(True)
    beside = Track(2, (0.42, 0.2, 0.6, 0.9), (0.51, 0.3), 1.0, 0.0)  # inside your grown box
    see([you, beside])
    mask = np.zeros((360, 640), np.uint8)
    mask[:, 64:390] = 255  # the mask runs the two of you together
    host.mask = mask
    out = plugin.process_frame(_frame())
    assert _sharp(out, 100, 220) and not _sharp(out, 300, 370)


def test_blur_now_asks_for_people_so_it_can_tell_you_apart(card):
    plugin, host, _bus, see, you, other = card
    assert host.wanted == set()
    plugin._now_btn.click()
    assert host.wanted == {"Privacy blur"}
    see([you, other])
    out = plugin.process_frame(_frame())
    assert _sharp(out, 100, 220) and not _sharp(out, 450, 560)
    plugin._now_btn.click()
    assert host.wanted == set()
