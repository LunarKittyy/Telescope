import cv2
import numpy as np
import pytest

from telescope.plugin import EventBus
from telescope.plugins.transforms import TransformsPlugin
from telescope.view import View
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
    assert ctrl.sent[-1] == {"action": "zoom", "ratio": 1.0, "crop": 2.0, "x": 0.5, "y": 0.5}


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
