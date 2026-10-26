from types import SimpleNamespace

import pytest

from telescope.plugin import EventBus
from telescope.plugins.photos import DEFAULTS, PhotosPlugin, build_options, offered
from telescope.stills import StillCaps, StillProgress, StillResult
from telescope.view import ROTATIONS

PLAIN = StillCaps(size=(1920, 1080))  # an emulator: JPEG and nothing else
FULL = StillCaps(formats=("jpeg", "heic", "ultrahdr", "raw"), size=(4080, 3072), max_res=(8160, 6144),
                 max_res_raw=True, extensions=("night", "hdr", "bokeh"), exposure_max_ns=30_000_000_000,
                 bracketing=True, focus_bracketing=True, noise_reduction_hq=True, sharpening_hq=True,
                 hot_pixel_hq=True, distortion=True, burst_max=60)


def choices(**kw):
    return {**DEFAULTS, **kw}


# ── What's offered ───────────────────────────────────────────────────────────

def test_a_plain_lens_offers_only_a_single_jpeg():
    can = offered(PLAIN, choices())
    assert can == {"formats": ["jpeg"], "modes": ["normal"], "shots": ["single"], "exposures": [0]}


def test_a_flagship_offers_everything_it_has():
    can = offered(FULL, choices())
    assert can["formats"] == ["jpeg", "heic", "ultrahdr", "raw", "raw+jpeg"]
    assert can["modes"] == ["normal", "max_res", "night", "hdr", "bokeh"]
    assert can["shots"] == ["single", "burst", "bracket", "focus"]
    assert can["exposures"][-1] == 30 and 60 not in can["exposures"]


def test_full_resolution_only_saves_jpeg_and_raw():
    assert offered(FULL, choices(mode="max_res"))["formats"] == ["jpeg", "raw", "raw+jpeg"]
    no_raw = StillCaps(formats=("jpeg", "raw"), max_res=(8000, 6000), max_res_raw=False)
    assert offered(no_raw, choices(mode="max_res"))["formats"] == ["jpeg"]


def test_the_makers_modes_take_jpeg_singles_or_bursts_at_their_own_exposure():
    can = offered(FULL, choices(mode="night"))
    assert can["formats"] == ["jpeg"] and can["shots"] == ["single", "burst"] and can["exposures"] == [0]


def test_a_mode_this_lens_lacks_falls_back_to_normal():
    assert offered(PLAIN, choices(mode="night", format="raw"))["formats"] == ["jpeg"]


def test_bracketing_has_no_long_exposure_of_its_own():
    assert offered(FULL, choices(shots="bracket"))["exposures"] == [0]


# ── The options sent ─────────────────────────────────────────────────────────

def test_a_plain_jpeg():
    assert build_options(PLAIN, choices(quality=88)) == {"format": "jpeg", "rotation": 0, "quality": 88}


def test_picks_a_lens_cant_do_are_left_out_rather_than_sent():
    opts = build_options(PLAIN, choices(format="raw+jpeg", mode="night", shots="bracket", exposure_s=8,
                                        distortion="off"))
    assert opts == {"format": "jpeg", "rotation": 0, "quality": 95}


def test_raw_alone_has_no_quality():
    opts = build_options(FULL, choices(format="raw"))
    assert opts["format"] == "none" and opts["raw"] == 1 and "quality" not in opts


def test_raw_and_jpeg_at_full_resolution_with_a_long_exposure_and_the_best_processing():
    opts = build_options(FULL, choices(format="raw+jpeg", mode="max_res", exposure_s=4, timer=10, distortion="off"),
                         rotation=90)
    assert opts == {"format": "jpeg", "raw": 1, "quality": 95, "max_res": 1, "exposure_ns": 4_000_000_000,
                    "timer_s": 10, "nr_hq": 1, "edge_hq": 1, "hot_pixel_hq": 1, "distortion": 0, "rotation": 90}


def test_night_mode_bursts():
    opts = build_options(FULL, choices(mode="night", shots="burst", burst=3, best=True, distortion="on"))
    assert opts == {"format": "jpeg", "quality": 95, "extension": "night", "count": 3, "rotation": 0}


def test_brackets_and_focus_steps():
    assert build_options(FULL, choices(shots="bracket", bracket="5x1"))["bracket_ev"] == "-2,-1,0,1,2"
    assert build_options(FULL, choices(shots="focus", focus_steps=8))["focus_steps"] == 8
    assert build_options(StillCaps(burst_max=4, focus_bracketing=True), choices(shots="focus", focus_steps=8))[
        "focus_steps"] == 4


# ── The card ─────────────────────────────────────────────────────────────────

class _Host:
    def __init__(self, caps=None, streaming=True):
        self.caps, self.streaming, self.busy, self.taken, self.saves, self.notes = caps, streaming, False, [], 0, []
        self.rotation = None

    def still_caps(self, source_id=None):
        return self.caps

    def still_busy(self, source_id=None):
        return self.busy

    def is_streaming(self):
        return self.streaming

    def take_still(self, options, folder, on_done, on_progress=None, source_id=None):
        self.taken.append((options, folder, on_done, on_progress))
        return True

    def view(self):
        return SimpleNamespace(rotation=self.rotation)

    def schedule_save(self):
        self.saves += 1

    def send_notification(self, title, body, urgent=True):
        self.notes.append((title, body))


@pytest.fixture
def card(qt_widgets):
    def make(caps=None, **kw):
        host, bus = _Host(caps, **kw), EventBus()
        plugin = PhotosPlugin()
        plugin.setup(host, bus)
        panel = qt_widgets(plugin.create_panel())
        bus.phones_changed.emit(1)
        plugin.set_config({"folder": "/tmp/photos"})
        return plugin, host, bus, panel
    return make


def test_without_photos_the_button_is_off_and_the_card_says_why(card):
    plugin, host, bus, _ = card(None, streaming=False)
    assert not plugin._take_btn.isEnabled() and "Start streaming" in plugin._note.text()
    host.streaming = True
    bus.phone_state_updated.emit({})
    assert "Update the app" in plugin._note.text()


def test_the_card_shows_only_what_the_lens_has(card):
    plugin, host, bus, _ = card(PLAIN)
    assert plugin._take_btn.isEnabled() and plugin._note.isHidden()
    assert plugin._format_row.isHidden() and plugin._mode_row.isHidden() and plugin._shots_row.isHidden()
    assert plugin._exposure_row.isHidden() and plugin._best_row.isHidden() and plugin._distortion_row.isHidden()
    host.caps = FULL
    bus.phone_state_updated.emit({})
    assert not plugin._format_row.isHidden() and not plugin._mode_row.isHidden()
    assert plugin._format_combo.count() == 5 and plugin._mode_combo.count() == 5


def test_picking_a_night_mode_hides_what_it_cant_do(card):
    plugin, *_ = card(FULL)
    plugin._mode_combo.setCurrentIndex(plugin._mode_combo.findData("night"))
    assert plugin._format_row.isHidden() and plugin._exposure_row.isHidden() and plugin._best_row.isHidden()
    assert [plugin._shots_combo.itemData(i) for i in range(plugin._shots_combo.count())] == ["single", "burst"]


def test_a_pick_for_another_lens_is_kept_when_this_one_cant_do_it(card):
    plugin, host, bus, _ = card(FULL)
    plugin._format_combo.setCurrentIndex(plugin._format_combo.findData("raw"))
    host.caps = PLAIN
    bus.phone_state_updated.emit({})
    host.caps = FULL
    bus.phone_state_updated.emit({})
    assert plugin._format_combo.currentData() == "raw" and plugin.get_config()["format"] == "raw"


def test_take_photo_sends_the_options_with_the_views_rotation(card):
    plugin, host, *_ = card(FULL)
    host.rotation = ROTATIONS["90 CCW"]
    plugin._take_btn.click()
    options, folder, on_done, on_progress = host.taken[0]
    assert folder == "/tmp/photos" and options["rotation"] == 270 and options["format"] == "jpeg"
    on_progress(StillProgress("waiting", countdown_ms=2400))
    assert "3 s" in plugin._status.text()
    on_progress(StillProgress("capturing", shots=5, taken=1))
    assert "2 of 5" in plugin._status.text()
    on_done(StillResult(True, ["/tmp/photos/IMG_1.jpg", "/tmp/photos/IMG_1.dng"]))
    assert "IMG_1.jpg" in plugin._status.text() and "1 more" in plugin._status.text()
    assert not plugin._open_btn.isHidden()


def test_a_failed_photo_shows_why(card):
    plugin, host, *_ = card(PLAIN)
    plugin._take_btn.click()
    host.taken[0][2](StillResult.failed("camera_off"))
    assert "camera is off" in plugin._status.text()


def test_the_button_waits_while_any_plugin_takes_a_photo(card):
    plugin, host, bus, _ = card(PLAIN)
    host.busy = True
    bus.still_busy_changed.emit("phone-1", True)
    assert not plugin._take_btn.isEnabled()
    host.busy = False
    bus.still_busy_changed.emit("phone-1", False)
    assert plugin._take_btn.isEnabled()


def test_config_round_trips_and_odd_values_go_back_to_defaults(card):
    plugin, *_ = card(FULL)
    plugin.set_config({"format": "heic", "timer": 3, "best": False, "folder": "/x"})
    assert plugin.get_config()["format"] == "heic" and plugin.get_config()["folder"] == "/x"
    plugin.set_config({"timer": 7, "best": "yes", "burst": True, "folder": 5})
    cfg = plugin.get_config()
    assert cfg["timer"] == 0 and cfg["best"] is True and cfg["burst"] == DEFAULTS["burst"] and cfg["folder"]


def test_the_shortcut_takes_a_photo(card):
    plugin, host, *_ = card(PLAIN)
    action = next(a for a in plugin.create_actions() if a.id == "photos.take")
    assert action.run({}) is not None and len(host.taken) == 1
