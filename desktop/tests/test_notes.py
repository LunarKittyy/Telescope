import pytest

from telescope.plugin import EventBus
from telescope.plugins import notes as notes_module
from telescope.plugins.notes import NOTES_MAX_BYTES, NotesPlugin, body_size, front_facing, notes_status, set_name


class _Host:
    def __init__(self):
        self.saves = 0

    def schedule_save(self):
        self.saves += 1


class _Ctrl:
    def __init__(self, state=None):
        self.sent = []
        self.state = state

    def send(self, **params):
        self.sent.append(params)

    def get_state(self):
        return self.state


def _state(takes_notes=True, notes=False, shown=False, facing="front", **more):
    state = {"cameras": [{"id": "1", "current": True, "facing": facing}], "notes": notes, "notes_shown": shown, **more}
    if takes_notes:
        state["takes_notes"] = True
    return state


@pytest.fixture
def card(qt_widgets):
    host, bus = _Host(), EventBus()
    plugin = NotesPlugin()
    plugin.setup(host, bus)
    panel = qt_widgets(plugin.create_panel())
    bus.phones_changed.emit(1)
    plugin.set_config({})
    yield plugin, host, bus, panel
    plugin.shutdown()


def _streaming(plugin, state=None):
    ctrl = _Ctrl()
    plugin.on_stream_start("https://phone/v1/video", ctrl)
    plugin.on_phone_state(state if state is not None else _state())
    return ctrl


def test_a_set_is_named_by_its_first_line():
    assert set_name("\n  Welcome everyone  \nSecond") == "Welcome everyone"
    assert set_name("   \n") == ""
    long = set_name("x" * 100)
    assert len(long) == notes_module.NAME_CHARS and long.endswith("…")


def test_the_front_camera_is_read_from_the_current_lens():
    assert front_facing(_state(facing="front")) is True
    assert front_facing(_state(facing="back")) is False
    assert front_facing({"cameras": [{"current": True}]}) is None  # an older phone leaves facing out
    assert front_facing({}) is None


def test_the_status_says_what_stands_in_the_way_first():
    base = dict(streaming=True, phone=True, takes_notes=True, too_long=False, sent=False, shown=False, front=True,
                edited=False)
    assert notes_status(**{**base, "phone": False})[1] == "dim"
    assert "Update Telescope" in notes_status(**{**base, "takes_notes": False})[0]
    assert notes_status(**{**base, "too_long": True})[1] == "status_err"
    assert "Start streaming" in notes_status(**{**base, "streaming": False})[0]
    assert "front camera" in notes_status(**{**base, "front": False})[0]
    assert "Open Telescope" in notes_status(**{**base, "sent": True})[0]
    assert notes_status(**{**base, "sent": True, "shown": True}) == ("The notes are on the phone's screen.", "status_ok")
    assert "back camera" in notes_status(**{**base, "sent": True, "shown": True, "front": False})[0]
    assert "Send again" in notes_status(**{**base, "sent": True, "shown": True, "edited": True})[0]
    assert "closed on the phone" in notes_status(**{**base, "sent": True, "closed": True})[0]
    assert "preview" in notes_status(**{**base, "sent": True, "preview": True})[0]


def test_text_in_any_script_counts_its_utf8_bytes():
    assert body_size("Привет") == len('{"action": "notes", "text": "Привет"}'.encode("utf-8"))
    assert body_size("नमस्ते" * 1000) < NOTES_MAX_BYTES  # 18,000 bytes, not six times that


def test_nothing_can_be_sent_without_a_stream(card):
    plugin, _host, _bus, _panel = card
    plugin._editor.setPlainText("Hello")
    assert not plugin._send_btn.isEnabled()
    assert "Start streaming" in plugin._status.text()


def test_send_puts_the_style_then_the_text_on_the_phone(card):
    plugin, _host, _bus, _panel = card
    ctrl = _streaming(plugin)
    plugin._editor.setPlainText("Hello everyone\nThanks for coming")
    plugin._send_btn.click()
    assert ctrl.sent == [
        {"action": "notes_style", "size": 44, "speed": 3, "playing": 0},
        {"action": "notes", "text": "Hello everyone\nThanks for coming"},
    ]
    assert plugin._next_btn.isEnabled() and plugin._clear_btn.isEnabled()


def test_scrolling_and_style_follow_while_the_notes_are_up(card):
    plugin, _host, _bus, _panel = card
    ctrl = _streaming(plugin)
    plugin._editor.setPlainText("One\nTwo")
    plugin._send_btn.click()
    ctrl.sent.clear()
    plugin._next_btn.click()
    plugin._next_btn.click()
    plugin._back_btn.click()
    plugin._play_btn.click()
    plugin._size_slider.setValue(60)
    plugin._scroll_btns["hand"].click()
    assert ctrl.sent == [
        {"action": "notes_scroll", "by": 1}, {"action": "notes_scroll", "by": 1}, {"action": "notes_scroll", "by": -1},
        {"action": "notes_style", "size": 44, "speed": 3, "playing": 1},
        {"action": "notes_style", "size": 60, "speed": 3, "playing": 1},
        {"action": "notes_style", "size": 60, "speed": 0, "playing": 0},  # by hand: Play goes and so does scrolling
        {"action": "notes_style", "size": 60, "speed": 0, "playing": 0},
    ]
    assert plugin._play_btn.isHidden() and plugin._speed_row.isHidden()


def test_style_changes_before_sending_stay_on_the_desktop(card):
    plugin, _host, _bus, _panel = card
    ctrl = _streaming(plugin)
    plugin._size_slider.setValue(80)
    plugin._next_btn.click()
    assert ctrl.sent == []


def test_clear_takes_the_notes_down(card):
    plugin, _host, _bus, _panel = card
    ctrl = _streaming(plugin)
    plugin._editor.setPlainText("Hello")
    plugin._send_btn.click()
    plugin._clear_btn.click()
    assert {"action": "notes", "text": ""} in ctrl.sent
    assert not plugin._clear_btn.isEnabled() and not plugin._next_btn.isEnabled()


def test_an_older_phone_is_asked_to_update_instead(card):
    plugin, _host, _bus, _panel = card
    ctrl = _streaming(plugin, _state(takes_notes=False))
    plugin._editor.setPlainText("Hello")
    assert not plugin._send_btn.isEnabled()
    assert "Update Telescope on the phone" in plugin._status.text()
    plugin._send()
    assert ctrl.sent == []


def test_a_browser_camera_has_no_screen_for_notes(card):
    plugin, _host, bus, _panel = card
    _streaming(plugin)
    plugin._editor.setPlainText("Hello")
    bus.source_selected.emit("browser:abc")
    assert not plugin._send_btn.isEnabled()
    assert "phone as the camera" in plugin._status.text()


def test_notes_too_long_for_the_phone_are_held_back(card):
    plugin, _host, _bus, _panel = card
    ctrl = _streaming(plugin)
    plugin._editor.setPlainText("é" * (NOTES_MAX_BYTES // 2 + 10))  # two bytes each in UTF-8
    assert body_size(plugin._editor.toPlainText()) > NOTES_MAX_BYTES
    assert not plugin._send_btn.isEnabled()
    plugin._send()
    assert ctrl.sent == []


def test_the_card_says_when_the_phone_has_them_but_not_on_screen(card):
    plugin, _host, _bus, _panel = card
    _streaming(plugin)
    plugin._editor.setPlainText("Hello")
    plugin._send_btn.click()
    plugin.on_phone_state(_state(notes=True, shown=False))
    assert "Open Telescope on the phone" in plugin._status.text()
    plugin.on_phone_state(_state(notes=True, shown=True))
    assert plugin._status.text() == "The notes are on the phone's screen."
    plugin._editor.setPlainText("Hello again")
    assert "Send again" in plugin._status.text()


def test_the_card_says_when_the_notes_were_closed_or_wait_for_preview(card):
    plugin, _host, _bus, _panel = card
    _streaming(plugin)
    plugin._editor.setPlainText("Hello")
    plugin._send_btn.click()
    plugin.on_phone_state(_state(notes=True, notes_closed=True))
    assert "closed on the phone" in plugin._status.text()
    plugin.on_phone_state(_state(notes=True, notes_preview=True))
    assert "close its preview" in plugin._status.text()
    plugin._send_btn.click()
    assert plugin._status.text() == "The notes are on the phone's screen."


def test_notes_the_phone_lost_are_sent_again(card, monkeypatch):
    plugin, _host, _bus, _panel = card
    now = [100.0]
    monkeypatch.setattr(notes_module.time, "monotonic", lambda: now[0])
    ctrl = _streaming(plugin)
    plugin._editor.setPlainText("Hello")
    plugin._send_btn.click()
    ctrl.sent.clear()
    plugin.on_phone_state(_state(notes=False))
    assert ctrl.sent == []  # just sent: the phone may not have read it yet
    now[0] += 30
    plugin.on_phone_state(_state(notes=False))
    assert ctrl.sent[-1] == {"action": "notes", "text": "Hello"}


def test_a_stream_ending_forgets_what_was_sent(card):
    plugin, _host, _bus, _panel = card
    _streaming(plugin)
    plugin._editor.setPlainText("Hello")
    plugin._send_btn.click()
    plugin.on_stream_stop()
    ctrl = _streaming(plugin)
    assert not plugin._clear_btn.isEnabled()
    plugin._next_btn.click()
    assert ctrl.sent == []


def test_moving_to_another_stream_and_back_sends_nothing_again(card):
    plugin, _host, _bus, _panel = card
    ctrl = _streaming(plugin)
    plugin._editor.setPlainText("Hello")
    plugin._send_btn.click()
    sent = len(ctrl.sent)
    plugin.on_focus_lost()
    plugin.on_focus_gained("https://phone/v1/video", ctrl)
    assert len(ctrl.sent) == sent
    assert plugin._clear_btn.isEnabled()


def test_sets_are_saved_and_picked(card):
    plugin, host, _bus, _panel = card
    plugin._editor.setPlainText("Monday standup\n- wins")
    plugin._new_btn.click()
    plugin._editor.setPlainText("Interview")
    assert [plugin._set_combo.itemText(i) for i in range(plugin._set_combo.count())] == ["Monday standup", "Interview"]
    plugin._set_combo.setCurrentIndex(0)
    assert plugin._editor.toPlainText() == "Monday standup\n- wins"
    plugin._size_slider.setValue(70)
    plugin._scroll_btns["hand"].click()
    cfg = plugin.get_config()
    assert cfg == {"sets": ["Monday standup\n- wins", "Interview"], "current": 0, "size": 70, "speed": 3,
                   "scroll": "hand"}
    assert host.saves > 0

    plugin.set_config({})
    assert plugin.get_config()["sets"] == [""]
    plugin.set_config(cfg)
    assert plugin.get_config() == cfg
    plugin._delete_btn.click()
    assert plugin.get_config()["sets"] == ["Interview"]


def test_a_damaged_config_falls_back_to_defaults(card):
    plugin, _host, _bus, _panel = card
    plugin.set_config({"sets": ["a", 5, None], "current": 9, "size": "huge", "speed": 99, "scroll": "sideways"})
    assert plugin.get_config() == {"sets": ["a"], "current": 0, "size": 44, "speed": 10, "scroll": "auto"}
    plugin.set_config({"sets": "not a list"})
    assert plugin.get_config()["sets"] == [""]


def test_shortcuts_can_scroll_and_send(card):
    plugin, _host, _bus, _panel = card
    actions = {a.id: a for a in plugin.create_actions()}
    assert {"notes.send", "notes.clear", "notes.next", "notes.previous", "notes.play", "notes.speed"} <= set(actions)


def test_switching_to_the_front_lens_clears_the_hint(card):
    plugin, _host, bus, _panel = card
    _streaming(plugin, _state(facing="back"))
    plugin._editor.setPlainText("Hello")
    assert "front camera" in plugin._status.text()
    bus.camera_switched.emit({"id": "1", "facing": "front"})
    assert "front camera" not in plugin._status.text()
