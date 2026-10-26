import cv2
import numpy as np
import pytest

from telescope.plugin import EventBus
from telescope.plugins.transforms import TransformsPlugin
from telescope.view import PhoneZoom, View, apply_zoom, rest_within
from telescope.view_controller import ViewController


class _Host:
    def schedule_save(self):
        pass

    def focused_source_id(self):
        return None

    def is_streaming(self):
        return False

    def is_streaming_from(self, _source_id):
        return False


class _Ctrl:
    def __init__(self):
        self.sent = []

    def send(self, **params):
        self.sent.append(params)


_ZOOMS = {"cameras": [{"id": "0", "current": True, "zoomRatioMax": 10.0, "cropZoomMax": 4.0, "freeformCrop": True}]}


def _grid():
    return np.arange(4 * 6 * 3, dtype=np.uint8).reshape(4, 6, 3)


@pytest.fixture
def view(qapp):
    bus = EventBus()
    vc = ViewController()
    vc.setup(_Host(), bus)
    return vc, bus


def test_without_anything_setting_a_view_the_phone_is_left_alone_and_frames_go_through(view):
    vc, _bus = view
    ctrl = _Ctrl()
    vc.on_stream_start("url", ctrl)
    vc.on_phone_state(_ZOOMS)
    frame = _grid()
    assert vc.process_frame(frame) is frame
    assert ctrl.sent == []


def test_a_picked_point_reaches_camera_control_without_the_transforms_card(view):
    vc, bus = view
    seen = []
    bus.focus_point.connect(lambda x, y: seen.append((x, y)))
    vc.process_frame(np.zeros((90, 160, 3), np.uint8))
    bus.focus_point_picked.emit(0.25, 0.5)
    assert seen == [(0.25, 0.5)]
    metered = []
    bus.meter_point.connect(lambda x, y: metered.append((x, y)))
    bus.meter_point_picked.emit(0.25, 0.5)
    assert metered == [(0.25, 0.5)] and len(seen) == 1


def test_taking_the_view_moves_zoom_and_pan_but_leaves_flip_and_rotation_manual(view):
    vc, bus = view
    taken, changed = [], []
    bus.view_taken.connect(taken.append)
    bus.view_changed.connect(changed.append)
    vc.set_view(zoom=2.0, pan_x=0.5)
    assert vc.take_view("Auto framing")
    assert not vc.take_view("Something else")
    vc.steer_view("Auto framing", 3.0, -0.2, 0.1)
    vc.steer_view("Something else", 1.0, 0.0, 0.0)  # ignored: it hasn't got the view
    vc.set_view(zoom=1.5, flip_h=True)  # by hand while taken: the flip goes through, the zoom waits
    assert vc.view == View(zoom=3.0, pan_x=-0.2, pan_y=0.1, flip_h=True)
    vc.release_view("Auto framing")
    assert vc.view == View(zoom=1.5, pan_x=0.5, flip_h=True)
    assert taken == ["Auto framing", ""]
    assert changed[-1] == vc.view


def test_steering_alone_wakes_the_phone_up_to_its_share(view):
    vc, _bus = view
    ctrl = _Ctrl()
    vc.on_stream_start("url", ctrl)
    vc.on_phone_state(_ZOOMS)
    vc.take_view("Auto framing")
    vc.steer_view("Auto framing", 2.0, 0.0, 0.0)
    assert ctrl.sent == [{"action": "zoom", "ratio": 1.0, "crop": 1.4286, "x": 0.5, "y": 0.5}]  # with room to move
    assert vc._desktop_crop[0] == pytest.approx(1.4, abs=1e-3)


@pytest.fixture
def steered(view, monkeypatch):
    vc, _bus = view
    clock = [100.0]
    monkeypatch.setattr("telescope.view_controller.time.monotonic", lambda: clock[0])
    ctrl = _Ctrl()
    vc.on_stream_start("url", ctrl)
    vc.on_phone_state(_ZOOMS)
    vc.take_view("Auto framing")
    vc.steer_view("Auto framing", 2.0, 0.0, 0.0)
    return vc, ctrl, clock


def test_small_steers_move_on_this_computer_and_leave_the_phone_alone(steered):
    vc, ctrl, _clock = steered
    for pan in (0.05, 0.1, -0.1, 0.0):
        vc.steer_view("Auto framing", 2.1, pan, pan)
    assert len(ctrl.sent) == 1


def test_a_steady_new_framing_gets_a_new_phone_share_after_a_while(steered):
    vc, ctrl, clock = steered
    vc.steer_view("Auto framing", 3.0, 0.0, 0.0)  # still fits in what the phone sends, just tighter
    assert len(ctrl.sent) == 1
    clock[0] += 2.5
    vc.steer_view("Auto framing", 3.0, 0.0, 0.0001)
    assert len(ctrl.sent) == 2 and ctrl.sent[-1]["crop"] == pytest.approx(3.0 / 1.4, abs=1e-3)


def test_a_framing_the_phone_share_cant_show_gets_one_at_once(steered):
    vc, ctrl, _clock = steered
    vc.steer_view("Auto framing", 1.0, 0.0, 0.0)
    assert ctrl.sent[-1]["crop"] == 1.0 and vc._desktop_crop == (1.0, 0.0, 0.0)


def test_releasing_puts_back_the_manual_zoom(steered):
    vc, ctrl, _clock = steered
    vc.release_view("Auto framing")
    assert vc.view.zoom == 1.0 and ctrl.sent[-1]["crop"] == 1.0


def test_a_frozen_step_keeps_the_view_it_was_taken_with(view):
    vc, _bus = view
    vc.set_view(flip_h=True)
    step = vc.frame_step()
    vc.set_view(flip_h=False, rotation=cv2.ROTATE_90_CLOCKWISE)
    assert np.array_equal(step(_grid()), cv2.flip(_grid(), 1))


def test_the_transforms_card_greys_out_zoom_and_pan_while_something_else_moves_them(qapp):
    bus = EventBus()

    class Host(_Host):
        def set_view(self, **changes):
            vc.set_view(**changes)

    vc = ViewController()
    host = Host()
    vc.setup(host, bus)
    card = TransformsPlugin()
    card.setup(host, bus)
    _panel = card.create_panel()
    bus.phones_changed.emit(1)  # the card is greyed out until a phone is paired
    card.set_config({"zoom": 2.0})
    assert card._zoom_slider.isEnabled() and card._pan_pad.isEnabled()
    vc.take_view("Auto framing")
    assert not card._zoom_slider.isEnabled() and not card._pan_pad.isEnabled()
    assert card._flip_h.isEnabled()
    assert card._zoom_slider.toolTip().startswith("Auto framing is moving the zoom and pan")
    bus.view_dragged.emit(0.1, 0.0)  # dragging the preview doesn't fight it
    assert card.pan_x == 0.0
    vc.release_view("Auto framing")
    assert card._zoom_slider.isEnabled() and card._pan_pad.isEnabled()
    assert vc.view.zoom == 2.0


def test_rest_within_says_what_this_computer_crops_from_a_phone_share():
    centre = PhoneZoom(ratio=1.0, crop=2.0, x=0.5, y=0.5)
    assert rest_within(2.0, 0.0, 0.0, centre) == (1.0, 0.0, 0.0)
    rest = rest_within(4.0, 0.3, 0.0, centre)
    assert rest[0] == pytest.approx(2.0) and rest[1] > 0 and rest[2] == pytest.approx(0.0)
    assert rest_within(1.5, 0.0, 0.0, centre) is None  # wider than what the phone sends
    assert rest_within(4.0, 1.0, 0.0, centre) is None  # off its edge


class _Vision:
    def __init__(self):
        self.fed = []

    def wants_people(self):
        return True

    def feed_raw(self, frame, prepare, to_shown):
        self.fed.append((prepare(frame), to_shown))


def test_people_are_found_in_the_picture_as_shown_through_the_phone_share(qapp):
    vision = _Vision()
    vc = ViewController()
    vc.setup(_Host(), EventBus(), vision)
    ctrl = _Ctrl()
    vc.on_stream_start("url", ctrl)
    vc.on_phone_state(_ZOOMS)
    vc.take_view("Auto framing")
    vc.steer_view("Auto framing", 2.0, 0.0, 0.0)  # the phone sends the middle 1/1.4286
    vc.set_view(rotation=cv2.ROTATE_90_CLOCKWISE)
    vc.process_frame(np.zeros((40, 60, 3), np.uint8))
    upright, to_shown = vision.fed[0]
    assert upright.shape == (60, 40, 3) and vc.aspect() == pytest.approx(40 / 60)
    x, y = to_shown(0.5, 0.5)
    assert (x, y) == (pytest.approx(0.5), pytest.approx(0.5))
    x, y = to_shown(1.0, 0.0)  # the top right of the upright frame, inside the share
    assert x == pytest.approx(0.5 + 0.35, abs=1e-3) and y == pytest.approx(0.5 - 0.35, abs=1e-3)


def _scene():
    rng = np.random.default_rng(1)
    small = rng.integers(0, 255, (18, 32, 3), np.uint8)
    return cv2.resize(small, (1600, 900), interpolation=cv2.INTER_CUBIC)  # blobs, so a shift is easy to see


def _from_phone(scene, share):
    """What the phone sends for a share: that part of the full view, at the stream's size."""
    from telescope.view import phone_window
    return cv2.resize(apply_zoom(scene, *phone_window(share)), (320, 180), interpolation=cv2.INTER_AREA)


def test_a_new_phone_share_is_cropped_for_only_once_its_frames_arrive(view):
    vc, _bus = view
    ctrl = _Ctrl()
    vc.on_stream_start("url", ctrl)
    vc.on_phone_state(_ZOOMS)
    vc.take_view("Auto framing")
    vc.steer_view("Auto framing", 3.0, 0.0, 0.0)
    scene, old = _scene(), vc._sent_zoom
    vc.process_frame(_from_phone(scene, old))
    vc.steer_view("Auto framing", 3.0, 0.22, 0.0)  # just past the side of what the phone sends: a new share at once
    new = vc._sent_zoom
    assert new != old and vc._handover is not None
    truth = cv2.resize(apply_zoom(scene, 3.0, 0.22, 0.0), (320, 180), interpolation=cv2.INTER_AREA)
    for _ in range(3):  # the phone hasn't caught up yet
        late = _from_phone(scene, old)
        out = vc.process_frame(late)
        assert not vc._handover.arrived
    naive = apply_zoom(late, *vc._desktop_crop)  # the new share's crop on a frame from the old one
    assert np.abs(out.astype(int) - truth).mean() < 0.5 * np.abs(naive.astype(int) - truth).mean()
    out = vc.process_frame(_from_phone(scene, new))
    assert vc._handover.arrived
    assert np.abs(out.astype(int) - truth).mean() < 12


def test_a_handover_gives_up_waiting_after_a_while(view, monkeypatch):
    vc, _bus = view
    clock = [100.0]
    monkeypatch.setattr("telescope.view_controller.time.monotonic", lambda: clock[0])
    ctrl = _Ctrl()
    vc.on_stream_start("url", ctrl)
    vc.on_phone_state(_ZOOMS)
    vc.take_view("Auto framing")
    vc.steer_view("Auto framing", 3.0, 0.0, 0.0)
    scene, old = _scene(), vc._sent_zoom
    vc.steer_view("Auto framing", 3.0, 0.6, 0.0)
    vc.process_frame(_from_phone(scene, old))
    assert not vc._handover.arrived
    clock[0] += 1.5
    vc.process_frame(_from_phone(scene, old))
    assert vc._handover.arrived
