"""First-run checklist: what each step shows, and when the checklist steps aside."""

import pytest

import telescope.plugins.onboarding as onboarding_module
from telescope.plugin import EventBus
from telescope.plugins.onboarding import OnboardingPlugin
from telescope.plugins.preview import PreviewPlugin


class _Host:
    def installEventFilter(self, _filter):
        pass

    def is_streaming(self):
        return False

    def schedule_save(self):
        pass


@pytest.fixture
def env(qapp, monkeypatch):
    monkeypatch.setattr(OnboardingPlugin, "check_virtual_camera", lambda self: None)
    monkeypatch.setattr(onboarding_module, "bundled_apk_path", lambda: None)
    monkeypatch.setattr(onboarding_module, "IS_LINUX", True)
    bus = EventBus()
    needed = []
    bus.setup_needed.connect(needed.append)
    plugin = OnboardingPlugin()
    plugin.setup(_Host(), bus)
    card = plugin.create_panel()
    return plugin, bus, card, needed


def test_starts_as_a_checklist_with_nothing_done(env):
    plugin, _bus, card, needed = env
    assert not card.isHidden()
    assert needed[-1] is True
    assert plugin._vcam.text.text() == "Checking…"
    assert plugin._pair.button.text() == "Add phone"
    assert not plugin._qr_row.isHidden()


def test_missing_v4l2loopback_says_which_package(env):
    plugin, _bus, _card, _needed = env
    plugin._on_vcam(False, "")
    assert "RPM Fusion" in plugin._vcam.text.text()
    assert plugin._vcam.button.text() == "Check again"


def test_windows_offers_to_install_the_driver(env, monkeypatch):
    plugin, _bus, _card, _needed = env
    monkeypatch.setattr(onboarding_module, "IS_LINUX", False)
    plugin._on_vcam(False, "")
    assert plugin._vcam.button.text() == "Install driver"
    plugin._on_vcam(False, "access denied")
    assert "access denied" in plugin._vcam.text.text()
    assert plugin._vcam.button.text() == "Try again"


def test_add_phone_goes_through_the_bus(env):
    plugin, bus, _card, _needed = env
    asked = []
    bus.add_phone_requested.connect(lambda: asked.append(True))
    plugin._pair.button.click()
    assert asked == [True]


def test_checklist_steps_aside_after_the_first_stream(env):
    plugin, bus, card, needed = env
    plugin._on_vcam(True, "")
    bus.phones_changed.emit(1)
    assert not card.isHidden()  # paired and ready, but step 4 still points at Start Streaming
    assert plugin._pair.badge.text() == "✓"
    assert "top right" in plugin._start.text.text()
    bus.stream_started.emit("url")
    bus.stream_connected.emit()
    bus.stream_stopped.emit()
    assert card.isHidden()
    assert needed[-1] is False
    assert plugin.get_config() == {"streamed": True}


def test_a_saved_first_stream_keeps_the_checklist_away(env):
    plugin, bus, card, _needed = env
    plugin.set_config({"streamed": True})
    plugin._on_vcam(True, "")
    bus.phones_changed.emit(1)
    assert card.isHidden()
    bus.phones_changed.emit(0)  # every phone removed: back to the checklist
    assert not card.isHidden()


def test_a_paired_phone_alone_is_not_enough_without_the_camera(env):
    plugin, bus, card, _needed = env
    bus.phones_changed.emit(1)
    plugin._on_vcam(False, "")
    assert not card.isHidden()
    assert plugin._app.badge.text() == "✓"
    assert plugin._qr_row.isHidden()


def test_streaming_hides_the_checklist(env):
    _plugin, bus, card, needed = env
    bus.stream_started.emit("url")
    assert card.isHidden() and needed[-1] is False
    bus.stream_stopped.emit()
    assert not card.isHidden()


def test_usb_install_is_offered_only_with_a_bundled_apk_and_adb(env, monkeypatch, tmp_path):
    plugin, _bus, _card, _needed = env
    assert plugin._app.button.isHidden()
    monkeypatch.setattr(onboarding_module, "bundled_apk_path", lambda: tmp_path / "Telescope.apk")
    monkeypatch.setattr(onboarding_module, "adb_available", lambda: True)
    plugin._render()
    assert not plugin._app.button.isHidden()
    assert plugin._app.button.text() == "Install over USB"


def test_usb_install_reports_a_missing_phone(env):
    plugin, _bus, _card, _needed = env
    plugin._on_apk(False, "No phone found over USB.")
    assert plugin._app.text.text() == "No phone found over USB."


def test_the_video_stage_makes_room_for_the_checklist(qapp):
    bus = EventBus()
    preview = PreviewPlugin()
    preview.setup(_Host(), bus)
    stage = preview.create_panel()
    bus.setup_needed.emit(True)
    assert stage.isHidden()
    bus.setup_needed.emit(False)
    assert not stage.isHidden()
