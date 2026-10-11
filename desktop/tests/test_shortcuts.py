import pytest
from PyQt6.QtCore import Qt
from PyQt6.QtWidgets import QButtonGroup, QCheckBox, QComboBox, QLabel, QPushButton, QWidget

from telescope.shortcuts import (
    Param, button_action, choice_action, clashes, clean_bindings, control_ready, has_command_modifier,
    keys_from_event, log_slider_action, normalize_keys, slider_action, split_keys,
)
from telescope.widgets.common import LogSliderRow, NoScrollSlider, SegmentButton


@pytest.fixture
def panel(qapp):
    w = QWidget()
    yield w
    w.deleteLater()


# ── Keys ─────────────────────────────────────────────────────────────────────

@pytest.mark.parametrize("raw, want", [
    ("Ctrl+Alt+M", "Ctrl+Alt+M"),
    ("ctrl+alt+m", "Ctrl+Alt+M"),
    ("Meta+F5", "Meta+F5"),
    ("Shift+Up", "Shift+Up"),
    ("Ctrl+Alt+M, Ctrl+X", ""),  # two chords
    ("Ctrl", ""),                 # a bare modifier
    ("", ""),
    (None, ""),
    (42, ""),
    ("Nonsense+Key", ""),
])
def test_normalize_keys(raw, want):
    assert normalize_keys(raw) == want


def test_keys_from_event_drops_keypad_and_bare_modifiers():
    mods = Qt.KeyboardModifier.ControlModifier | Qt.KeyboardModifier.KeypadModifier
    assert keys_from_event(int(Qt.Key.Key_M), mods) == "Ctrl+M"
    assert keys_from_event(int(Qt.Key.Key_Control), Qt.KeyboardModifier.ControlModifier) == ""
    assert keys_from_event(int(Qt.Key.Key_Backtab), Qt.KeyboardModifier.ShiftModifier) == "Shift+Tab"


def test_split_keys_and_command_modifiers():
    assert split_keys("Ctrl+Shift+F5")[:2] == (["Ctrl", "Shift"], "F5")
    assert has_command_modifier("Alt+M")
    assert not has_command_modifier("Shift+M")
    assert not has_command_modifier("F9")


# ── Bindings ─────────────────────────────────────────────────────────────────

def test_clean_bindings_keeps_good_ones_and_fixes_ids():
    raw = [
        {"id": "b1", "keys": "ctrl+m", "action": "microphone.mute", "params": {"mode": "toggle"}, "global": True},
        {"id": "b1", "keys": "Ctrl+N", "action": "presets.cycle"},  # repeated id gets a new one
        {"keys": "Ctrl", "action": "x"},  # no key
        {"keys": "Ctrl+Q"},  # no action
        "junk",
        {"id": "../evil", "keys": "F2", "action": "camera.torch", "params": {"mode": "on", "bad": [1], 3: "x"},
         "global": "yes"},
    ]
    out = clean_bindings(raw)
    assert [b["keys"] for b in out] == ["Ctrl+M", "Ctrl+N", "F2"]
    assert out[0] == {"id": "b1", "keys": "Ctrl+M", "action": "microphone.mute", "params": {"mode": "toggle"},
                      "global": True}
    assert out[1]["id"] != "b1" and out[1]["id"].isalnum()
    assert out[2]["id"] != "../evil"
    assert out[2]["params"] == {"mode": "on"}
    assert out[2]["global"] is False
    assert clean_bindings("nope") == []


def test_clashes_marks_later_bindings_on_the_same_keys():
    bindings = clean_bindings([{"id": "a", "keys": "F1", "action": "x"}, {"id": "b", "keys": "F2", "action": "x"},
                               {"id": "c", "keys": "F1", "action": "y"}])
    assert clashes(bindings) == {"c"}


def test_param_clean():
    number = Param("amount", "By", "number", 1.0, minimum=0.0, maximum=5.0, decimals=1)
    assert number.clean(9) == 5.0
    assert number.clean("2.26") == 2.3  # from telescope --action
    assert number.clean("lots") == 1.0
    assert number.clean(True) == 1.0
    assert number.clean(float("nan")) == 1.0
    fixed = Param("mode", "Key", "choice", "toggle", (("toggle", "T"), ("on", "On")))
    assert fixed.clean("on") == "on"
    assert fixed.clean("sideways") == "toggle"
    names = ["Desk"]
    dynamic = Param("name", "Preset", "choice", "", lambda: [(n, n) for n in names])
    assert dynamic.clean("Gone") == "Gone"  # a preset that was deleted stays named
    assert dynamic.clean(3) == ""
    assert Param("op", "Key", "choice", "a", (("a", "A"), ("b", "B")), shown_if=None).shown({})
    assert not Param("v", "V", "number", shown_if=("op", ("set",))).shown({"op": "up"})


# ── Controls ─────────────────────────────────────────────────────────────────

def test_control_ready_needs_enabled_and_shown_in_its_card(panel):
    button = QPushButton("x", panel)
    assert control_ready(button)  # the window not being shown doesn't matter
    button.setEnabled(False)
    assert not control_ready(button)
    button.setEnabled(True)
    button.hide()
    assert not control_ready(button)
    assert not control_ready(None)


def test_checkable_button_modes(panel):
    box = QCheckBox("On", panel)
    toggled = []
    box.toggled.connect(toggled.append)
    action = button_action("t.torch", "Torch", "Camera", box)
    run = lambda mode: action.run(action.full_params({"mode": mode}))  # noqa: E731

    assert run("toggle") == "Torch: on"
    assert run("on") == "Torch: on"
    assert run("off") == "Torch: off"
    assert toggled == [True, False]
    assert action.summary({"mode": "on"}) == "Torch: turn on"
    box.setEnabled(False)
    assert run("toggle") is None
    assert toggled == [True, False]


def test_hold_flips_while_held_and_back_on_release(panel):
    mute = QPushButton("Mute", panel)
    mute.setCheckable(True)
    action = button_action("m", "Mute", "Microphone", mute, states=("muted", "unmuted"))
    params = action.full_params({"mode": "hold"})
    mute.setChecked(True)  # muted: holding the key talks
    assert action.run(params) == "Mute: unmuted"
    action.run(params)  # a second press while held changes nothing more
    assert not mute.isChecked()
    action.release(params)
    assert mute.isChecked()
    action.release(params)  # a release without a press does nothing
    assert mute.isChecked()


def test_plain_button_clicks(panel):
    clicked = []
    btn = QPushButton("Reset", panel)
    btn.clicked.connect(lambda: clicked.append(True))
    action = button_action("x.reset", "Reset", "X", btn)
    assert action.params == ()
    assert action.run({}) == "Reset"
    assert clicked == [True]


def test_slider_steps_in_shown_units(panel):
    slider = NoScrollSlider(Qt.Orientation.Horizontal, panel)
    slider.setRange(-8, 8)
    slider.set_default(0)
    step = {"ev": 1 / 3}
    seen = []
    slider.valueChanged.connect(seen.append)
    label = QLabel("0 EV", panel)
    slider.valueChanged.connect(lambda v: label.setText(f"{v * step['ev']:+.1f} EV"))
    action = slider_action("c", "Compensation", "Camera", slider, scale=lambda: step["ev"], suffix=" EV",
                           decimals=1, step=1.0, readout=label, signed=True)
    run = lambda **p: action.run(action.full_params(p))  # noqa: E731

    assert run(op="up", amount=1.0) == "Compensation: +1.0 EV"
    assert slider.value() == 3
    assert run(op="down", amount=0.3) == "Compensation: +0.7 EV"  # never less than one step
    assert slider.value() == 2
    run(op="up", amount=20)
    assert slider.value() == 8  # clamped to the end
    run(op="set", value=-1.0)
    assert slider.value() == -3
    run(op="reset")
    assert slider.value() == 0
    assert seen == [3, 2, 8, -3, 0]
    assert action.repeats({"op": "up"}) and not action.repeats({"op": "set"})
    assert action.summary({"op": "down", "amount": 1.0}) == "Compensation: decrease by 1.0 EV"
    assert action.summary({"op": "set", "value": -1}) == "Compensation: set to -1.0 EV"
    assert action.current() == 0.0
    lo, hi = action.params[2].bounds()
    assert (round(lo, 2), round(hi, 2)) == (-2.67, 2.67)
    slider.setEnabled(False)
    assert run(op="up", amount=1.0) is None


def test_slider_without_default_offers_no_reset(panel):
    slider = NoScrollSlider(Qt.Orientation.Horizontal, panel)
    action = slider_action("s", "S", "G", slider)
    assert "reset" not in [v for v, _ in action.params[0].options()]


def test_log_slider_moves_in_stops(panel):
    row = LogSliderRow(50, 6400, display_fn=lambda v: f"ISO {int(round(v))}", parent=panel)
    row.set_value(400)
    got = []
    row.value_changed.connect(got.append)
    action = log_slider_action("iso", "ISO", "Camera", row)
    assert action.run(action.full_params({"op": "up", "stops": 1})) == "ISO: ISO 800"
    assert round(got[-1]) == 800
    action.run(action.full_params({"op": "down", "stops": 3}))
    assert round(row.get_value()) == 100
    action.run(action.full_params({"op": "set", "value": 99999}))
    assert round(row.get_value()) == 6400
    row.set_enabled(False)
    assert action.run(action.full_params({"op": "up"})) is None


def test_choice_on_a_combo_skips_disabled_items_and_says_it_was_picked(panel):
    combo = QComboBox(panel)
    combo.addItems(["15 fps", "30 fps", "60 fps"])
    combo.model().item(2).setEnabled(False)
    activated = []
    combo.activated.connect(activated.append)
    action = choice_action("fps", "FPS", "Out", combo=combo)
    run = lambda **p: action.run(action.full_params(p))  # noqa: E731

    assert run(op="next") == "FPS: 30 fps"
    assert run(op="next") == "FPS: 15 fps"  # 60 is greyed out, so it wraps round
    assert run(op="previous") == "FPS: 30 fps"
    assert run(op="set", option="15 fps") == "FPS: 15 fps"
    assert run(op="set", option="60 fps") is None
    assert activated == [1, 0, 1, 0]
    assert [v for v, _ in action.params[1].options()] == ["15 fps", "30 fps", "60 fps"]


def test_choice_without_wrap_stops_at_the_end(panel):
    combo = QComboBox(panel)
    combo.addItems(["a", "b"])
    combo.setCurrentIndex(1)
    action = choice_action("x", "X", "G", combo=combo, wrap=False)
    assert action.run(action.full_params({"op": "next"})) == "X: b"
    assert combo.currentIndex() == 1


def test_choice_on_exclusive_buttons_clicks_the_next(panel):
    auto, manual = SegmentButton("Auto", panel), SegmentButton("Manual", panel)
    group = QButtonGroup(panel)
    group.addButton(auto)
    group.addButton(manual)
    auto.setChecked(True)
    clicks = []
    group.buttonClicked.connect(lambda b: clicks.append(b.text()))
    action = choice_action("mode", "Exposure mode", "Camera", buttons=[auto, manual])
    assert action.run(action.full_params({"op": "next"})) == "Exposure mode: Manual"
    assert action.run(action.full_params({"op": "set", "option": "Auto"})) == "Exposure mode: Auto"
    assert clicks == ["Manual", "Auto"]
    assert action.controls() == (auto, manual)


def test_choice_over_buttons_that_come_and_go(panel):
    buttons = []
    action = choice_action("lens", "Lens", "Camera", buttons=lambda: buttons)
    assert action.run(action.full_params({"op": "next"})) is None  # no lenses yet
    for text in ("Wide", "Tele"):
        b = QPushButton(text, panel)
        b.setCheckable(True)
        buttons.append(b)
    buttons[0].setChecked(True)
    buttons[1].clicked.connect(lambda: buttons[0].setChecked(False))
    assert action.run(action.full_params({"op": "next"})) == "Lens: Tele"
    assert buttons[1].isChecked()
    assert action.controls() == tuple(buttons)
