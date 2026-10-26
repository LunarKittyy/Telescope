import numpy as np
import pytest

from telescope import fun_effects as fx, vision
from telescope.fun_effects import Clones, Look
from telescope.plugin import EventBus
from telescope.plugins.fun_pack import FunPackPlugin, stick_figure
from telescope.view import View
from telescope.vision import Person

W, H = 160, 90


def _room():
    """A room with a bit of texture, so differences from it are real ones."""
    room = np.zeros((H, W, 3), np.uint8)
    room[:, :, 0] = np.linspace(40, 120, W, dtype=np.uint8)[None]
    room[:, :, 1] = np.linspace(60, 100, H, dtype=np.uint8)[:, None]
    room[:, :, 2] = 70
    return room


def _with_person(room, x0, x1, colour=(20, 220, 240)):
    frame = room.copy()
    frame[20:80, x0:x1] = colour
    return frame


def _mask(x0, x1):
    mask = np.zeros((H, W), np.uint8)
    mask[20:80, x0:x1] = 255
    return mask


def _run(look, frames):
    runner = fx.Runner()
    return [runner(f, look) for f in frames]


def test_the_empty_room_difference_finds_whoever_stands_in_front_of_it():
    room = _room()
    mask = fx.PlateMask()(_with_person(room, 60, 100), room)
    assert mask[50, 80] == 255 and mask[50, 10] == 0 and mask[5, 80] == 0


def test_the_cloak_shows_the_empty_room_where_you_are():
    room = _room()
    frame = _with_person(room, 60, 100)
    out, = _run(Look("cloak", plate=room, edges=0), [frame])
    assert np.abs(out[50, 80].astype(int) - room[50, 80]).max() < 10
    assert (out[50, 10] == frame[50, 10]).all()
    nothing, = _run(Look("cloak"), [frame])
    assert nothing is frame  # no empty room yet: left alone


def test_the_cloak_uses_the_person_mask_when_the_vision_download_is_there():
    room = _room()
    frame = _with_person(room, 60, 100)
    out, = _run(Look("cloak", plate=room, edges=0, mask=lambda f: _mask(60, 100)), [frame])
    assert np.abs(out[50, 80].astype(int) - room[50, 80]).max() < 10


def test_clones_stay_where_they_were_taken_and_you_stay_in_front():
    room = _room()
    first = _with_person(room, 10, 40, colour=(0, 0, 255))
    clones = Clones.add(None, first, _mask(10, 40))
    second = _with_person(room, 60, 90, colour=(255, 0, 0))
    clones = Clones.add(clones, second, _mask(60, 90))
    assert clones.count == 2
    now = _with_person(room, 70, 120, colour=(0, 255, 0))
    out, = _run(Look("clones", plate=room, clones=clones, edges=0), [now])
    assert tuple(out[50, 20]) == (0, 0, 255)  # the first clone
    assert tuple(out[50, 65]) == (255, 0, 0)  # the second, where you aren't
    assert tuple(out[50, 80]) == (0, 255, 0)  # you, in front of it
    assert (out[5, 5] == room[5, 5]).all()


def test_light_painting_leaves_a_trail_that_fades_and_clear_starts_again():
    dark = np.full((H, W, 3), 10, np.uint8)
    lit = dark.copy()
    lit[40:44, 20:24] = 255
    look = Look("light", fade=5)
    runner = fx.Runner()
    runner(lit, look)
    after = runner(dark, look)
    assert after[42, 22, 0] > 150  # still there a frame later
    for _ in range(60):
        after = runner(dark, look)
    assert after[42, 22, 0] < 40  # and gone after two seconds
    runner(lit, look)
    cleared = runner(dark, Look("light", fade=5, reset=1))
    assert cleared[42, 22, 0] == 10


def test_slit_scan_shows_the_bottom_of_the_picture_from_further_back():
    frames = [np.full((H, W, 3), i * 10, np.uint8) for i in range(25)]
    out = _run(Look("slit", spread=20), frames)[-1]
    assert out[1, 80, 0] == 240  # the top: now
    assert abs(int(out[H - 1, 80, 0]) - 40) <= 5  # the bottom: 20 frames back
    across = _run(Look("slit", spread=20, across=True), frames)[-1]
    assert across[45, 0, 0] == 240 and abs(int(across[45, W - 1, 0]) - 40) <= 5


def test_the_stick_figure_is_drawn_on_paper_or_the_empty_room():
    look = Look("stick", lines=(((0.2, 0.2), (0.8, 0.8)),), heads=(((0.5, 0.3), 0.1),))
    out, = _run(look, [_room()])
    assert tuple(out[2, W - 3]) == fx.StickFigure.PAPER
    assert tuple(out[45, 80]) == fx.StickFigure.INK
    room = _room()
    on_room, = _run(Look("stick", plate=room), [_with_person(room, 60, 100)])
    assert (on_room == room).all()  # nobody seen: just the room


def test_stick_figure_lines_come_from_the_pose_through_the_view():
    person = Person({"left_shoulder": (0.4, 0.4, 1.0), "right_shoulder": (0.6, 0.4, 1.0),
                     "left_hip": (0.42, 0.7, 1.0), "nose": (0.5, 0.25, 1.0), "left_eye": (0.48, 0.24, 1.0),
                     "right_eye": (0.52, 0.24, 1.0), "right_hip": (0.58, 0.7, 0.1)})
    lines, heads = stick_figure([person], lambda x, y: (x, y), 16 / 9)
    assert ((0.4, 0.4), (0.6, 0.4)) in lines and ((0.4, 0.4), (0.42, 0.7)) in lines
    assert not any((0.58, 0.7) in line for line in lines)  # not sure enough about that hip
    assert len(heads) == 1 and heads[0][0] == (0.5, 0.25)
    flipped, _h = stick_figure([person], lambda x, y: (1 - x, y), 16 / 9)
    assert ((0.6, 0.4), (0.4, 0.4)) in flipped


@pytest.mark.parametrize("effect", ["game_boy", "webcam", "vhs", "crt", "ascii"])
def test_each_retro_look_keeps_the_size_and_changes_the_picture(effect):
    frame = _with_person(_room(), 60, 100)
    big = np.ascontiguousarray(np.repeat(np.repeat(frame, 4, axis=0), 4, axis=1))
    out = _run(Look(effect), [big, big])[-1]
    assert out.shape == big.shape and out.dtype == np.uint8
    assert np.abs(out.astype(int) - big).mean() > 5


def test_the_game_boy_has_four_shades():
    out, = _run(Look("game_boy"), [np.repeat(np.linspace(0, 255, 640, dtype=np.uint8)[None, :, None], 360, 0).repeat(3, 2)])
    assert len(np.unique(out.reshape(-1, 3), axis=0)) == 4


def test_off_and_datamosh_leave_frames_alone():
    frame = _room()
    assert _run(Look("off"), [frame])[0] is frame
    assert _run(Look("mosh"), [frame])[0] is frame


class _Host:
    def __init__(self, problem=""):
        self.problem, self.packets, self.people, self.saves = problem, {}, {}, 0
        self.mask = None

    def person_mask(self, frame):
        return self.mask

    def vision_state(self):
        return vision.MISSING

    def schedule_save(self):
        self.saves += 1

    def packets_problem(self):
        return self.problem

    def change_packets(self, owner, make_step):
        self.packets[owner] = make_step

    def want_people(self, owner, on):
        self.people[owner] = on

    def focused_source_id(self):
        return "phone"

    def view(self):
        return View()

    def view_aspect(self):
        return 16 / 9


@pytest.fixture
def card(qt_widgets):
    def make(**host_kw):
        host, bus = _Host(**host_kw), EventBus()
        plugin = FunPackPlugin()
        plugin.setup(host, bus)
        panel = qt_widgets(plugin.create_panel())
        bus.phones_changed.emit(1)
        plugin.set_config({})
        return plugin, host, bus, panel
    return make


def _pick(plugin, effect):
    plugin._combo.setCurrentIndex(plugin._combo.findData(effect))


def test_off_by_default_with_nothing_on_the_stream(card):
    plugin, host, _bus, _panel = card()
    assert plugin.get_config()["effect"] == "off"
    assert plugin.frame_step() is None
    assert not host.packets.get("fun_pack") and not host.people.get("fun_pack")
    frame = _room()
    assert plugin.process_frame(frame) is frame


def test_datamosh_puts_a_packet_step_on_and_takes_it_off(card):
    plugin, host, _bus, _panel = card()
    _pick(plugin, "mosh")
    make = host.packets["fun_pack"]
    assert make is not None and callable(make())
    assert plugin.frame_step() is None  # it's all in the packets
    _pick(plugin, "vhs")
    assert host.packets["fun_pack"] is None


def test_datamosh_on_heavy_says_why_and_changes_no_packets(card):
    plugin, host, bus, _panel = card(problem="Needs Light")
    _pick(plugin, "mosh")
    assert "fun_pack" not in host.packets
    assert plugin._note.text() == "Needs Light" and not plugin._note.isHidden()
    host.problem = ""
    bus.stream_transport_changed.emit()
    assert host.packets["fun_pack"] is not None and plugin._note.isHidden()


def test_clean_up_lets_a_keyframe_through(card):
    plugin, host, _bus, _panel = card()
    calls = []
    host.change_packets = lambda owner, make: calls.append(make is not None)
    _pick(plugin, "mosh")
    plugin._clean_btn.click()
    assert calls == [True, False, True]  # off and on again, which asks the phone for a keyframe


def test_the_stick_figure_asks_for_people_and_follows_them(card):
    plugin, host, bus, _panel = card()
    _pick(plugin, "stick")
    assert host.people["fun_pack"] is True
    person = Person({"left_shoulder": (0.4, 0.4, 1.0), "right_shoulder": (0.6, 0.4, 1.0)})
    bus.people_seen.emit("other", [person])
    assert plugin._look.lines == ()
    bus.people_seen.emit("phone", [person])
    assert len(plugin._look.lines) == 1
    _pick(plugin, "off")
    assert host.people["fun_pack"] is False


def test_asking_for_people_once_even_when_that_loads_vision_and_the_stream_stops(card):
    plugin, host, bus, _panel = card()
    asked = []

    def want_people(owner, on):  # the real one loads MediaPipe and emits its new state before returning
        asked.append(on)
        bus.vision_state_changed.emit("loading")
        bus.stream_stopped.emit()
    host.want_people = want_people
    _pick(plugin, "stick")
    bus.stream_stopped.emit()
    assert asked == [True]


def test_the_empty_room_and_a_clone_are_taken_from_the_next_frame(card, qapp):
    plugin, host, _bus, _panel = card()
    _pick(plugin, "clones")
    room = _room()
    plugin._start_countdown("plate")
    for _ in range(3):
        plugin._tick()
    assert plugin._grab == "plate"
    plugin.process_frame(room)
    qapp.processEvents()
    assert plugin._look.plate is not None and plugin._plate_btn.text() != "Take the empty room"
    plugin._start_countdown("clone")
    for _ in range(3):
        plugin._tick()
    plugin.process_frame(_with_person(room, 20, 50))
    qapp.processEvents()
    assert plugin._look.clones is not None and plugin._look.clones.count == 1
    out = plugin.process_frame(room)
    assert np.abs(out[50, 35].astype(int) - room[50, 35]).max() > 50  # the clone stands there now
    plugin._clear_btn.click()
    assert plugin._look.clones is None


def test_a_clone_without_the_empty_room_or_vision_says_what_to_do(card, qapp):
    plugin, host, _bus, _panel = card()
    _pick(plugin, "clones")
    plugin._grab = "clone"
    plugin.process_frame(_room())
    qapp.processEvents()
    assert plugin._look.clones is None and plugin._note.text()


def test_a_frozen_step_keeps_its_settings(card):
    plugin, host, _bus, _panel = card()
    _pick(plugin, "game_boy")
    step = plugin.frame_step()
    _pick(plugin, "off")
    frame = np.ascontiguousarray(np.repeat(np.repeat(_room(), 4, 0), 4, 1))
    assert step(frame) is not frame


def test_settings_come_back(card):
    plugin, host, _bus, _panel = card()
    plugin.set_config({"effect": "slit", "spread": 30, "across": True, "fade": 99})
    cfg = plugin.get_config()
    assert cfg["effect"] == "slit" and cfg["spread"] == 30 and cfg["across"] is True and cfg["fade"] == 10
    plugin.set_config({"effect": "nonsense"})
    assert plugin.get_config()["effect"] == "off"
