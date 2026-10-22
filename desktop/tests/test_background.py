import cv2
import numpy as np
import pytest
from PyQt6.QtWidgets import QPushButton

from telescope import vision
from telescope.plugin import EventBus
from telescope.plugins.background import BackgroundPlugin, blurred, compose, cover, read_picture, soften


class _Host:
    def __init__(self, mask=None, state=vision.READY):
        self.mask, self.state, self.asked, self.saves = mask, state, 0, 0

    def person_mask(self, frame):
        self.asked += 1
        return None if self.mask is None else np.ascontiguousarray(self.mask[:frame.shape[0], :frame.shape[1]])

    def vision_state(self):
        return self.state

    def schedule_save(self):
        self.saves += 1


def _frame(w=64, h=36):
    return np.full((h, w, 3), 200, np.uint8)


def _half_mask(w=64, h=36):
    mask = np.zeros((h, w), np.uint8)
    mask[:, : w // 2] = 255  # the person on the left half
    return mask


@pytest.fixture
def card(qapp):
    def make(**host_kw):
        host, bus = _Host(**host_kw), EventBus()
        plugin = BackgroundPlugin()
        plugin.setup(host, bus)
        panel = plugin.create_panel()
        bus.phones_changed.emit(1)
        plugin.set_config({})
        return plugin, host, panel
    return make


def test_the_person_stays_and_the_rest_is_the_background():
    frame = _frame()
    background = np.zeros_like(frame)
    out = compose(frame, _half_mask(), background)
    assert (out[:, :32] == 200).all() and (out[:, 32:] == 0).all()
    halfway = compose(frame, np.full((36, 64), 128, np.uint8), background)
    assert abs(int(halfway[0, 0, 0]) - 100) <= 1


def test_a_picture_covers_the_frame_and_is_cut_evenly():
    tall = np.zeros((400, 100, 3), np.uint8)
    tall[:150] = 255  # only the middle 100 rows are black
    covered = cover(tall, 160, 90)
    assert covered.shape == (90, 160, 3)
    assert covered.mean() < 255 * 0.5


def test_a_picture_with_a_non_ascii_path_opens(tmp_path):
    path = tmp_path / "Bakgrund åäö.png"
    cv2.imencode(".png", np.full((10, 20, 3), 7, np.uint8))[1].tofile(str(path))
    assert read_picture(str(path)).shape == (10, 20, 3)
    assert read_picture(str(tmp_path / "missing.png")) is None
    (tmp_path / "broken.png").write_bytes(b"not a picture")
    assert read_picture(str(tmp_path / "broken.png")) is None


def test_softer_edges_feather_the_mask_and_zero_leaves_it():
    mask = _half_mask(256, 144)
    assert soften(mask, 0) is mask
    soft = soften(mask, 8)
    assert 0 < soft[0, 128] < 255 and soft[0, 0] == 255 and soft[0, 255] == 0


def test_off_leaves_frames_alone_without_asking_for_a_mask(card):
    plugin, host, _panel = card(mask=_half_mask())
    frame = _frame()
    assert plugin.process_frame(frame) is frame
    assert host.asked == 0
    assert plugin.frame_step() is None


def test_blur_blurs_only_behind_the_person(card):
    plugin, host, _panel = card(mask=_half_mask(320, 180))
    plugin._set_mode("blur")
    plugin._edges_slider.setValue(0)
    frame = np.zeros((180, 320, 3), np.uint8)
    frame[::2] = 255  # stripes: a blur turns them grey
    out = plugin.process_frame(frame)
    assert np.array_equal(out[:, :150], frame[:, :150])
    assert 60 < out[:, 200:].mean() < 200 and out[:, 200:].std() < 40
    assert np.array_equal(blurred(frame, 6).shape, frame.shape)


def test_without_a_mask_yet_the_frame_goes_out_as_it_is(card):
    plugin, _host, _panel = card(mask=None, state=vision.LOADING)
    plugin._set_mode("blur")
    frame = _frame()
    assert plugin.process_frame(frame) is frame
    assert plugin._status.text() == "Getting ready…" and not plugin._status.isHidden()


def test_a_frozen_step_keeps_its_settings(card):
    plugin, _host, _panel = card(mask=np.zeros((36, 64), np.uint8))
    plugin._set_mode("blur")
    step = plugin.frame_step()
    plugin._set_mode("off")
    frame = np.zeros((36, 64, 3), np.uint8)
    frame[::2] = 255
    assert not np.array_equal(step(frame), frame)


def test_picture_mode_puts_the_picture_behind_and_survives_a_bad_file(card, tmp_path):
    plugin, _host, _panel = card(mask=_half_mask())
    path = tmp_path / "beach.png"
    cv2.imencode(".png", np.full((10, 20, 3), (255, 0, 0), np.uint8))[1].tofile(str(path))
    plugin._load_picture(str(path))
    plugin._set_mode("picture")
    plugin._edges_slider.setValue(0)
    out = plugin.process_frame(_frame())
    assert (out[:, 40:] == (255, 0, 0)).all() and (out[:, :30] == 200).all()
    assert plugin.get_config()["picture"] == str(path)
    plugin.set_config({"mode": "picture", "picture": str(tmp_path / "gone.png")})
    frame = _frame()
    assert plugin.process_frame(frame) is frame
    assert plugin._status.text() == "Telescope can't open that picture."


def test_settings_round_trip_and_damaged_ones_fall_back(card):
    plugin, _host, _panel = card()
    plugin.set_config({"mode": "blur", "blur": 9, "edges": 0})
    assert plugin.get_config() == {"mode": "blur", "blur": 9, "edges": 0, "picture": ""}
    assert not plugin._blur_row.isHidden() and plugin._picture_row.isHidden()
    plugin.set_config({"mode": "sparkles", "blur": "lots", "edges": 99, "picture": 3})
    assert plugin.get_config() == {"mode": "off", "blur": 6, "edges": 10, "picture": ""}


def test_without_the_download_the_card_offers_it(card):
    plugin, _host, panel = card(state=vision.MISSING)
    plugin._set_mode("blur")
    assert plugin._status.text().startswith("Needs a one-time download (about ")
    buttons = [b for b in panel.findChildren(QPushButton) if b.text() == "Download"]
    assert buttons and not buttons[0].isHidden()


def test_the_person_doesnt_smear_into_a_halo_around_them():
    frame = np.full((180, 320, 3), 240, np.uint8)
    frame[:, 100:220] = 10  # a dark person in the middle of a bright room
    mask = np.zeros((180, 320), np.uint8)
    mask[:, 100:220] = 255
    plain, without = blurred(frame, 8), blurred(frame, 8, mask)
    assert plain[90, 90, 0] < 200  # just outside: the plain blur pulls the person in
    assert without[90, 90, 0] > 225
