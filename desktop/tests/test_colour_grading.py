import os

import numpy as np
import pytest
from PyQt6.QtCore import QEvent, QPoint, QPointF, Qt
from PyQt6.QtGui import QMouseEvent
from PyQt6.QtTest import QTest
from PyQt6.QtWidgets import QApplication, QFileDialog

from telescope import grade as g
from telescope.colour import ColourStage, lattice, read_cube, write_cube
from telescope.plugin import EventBus
from telescope.plugins import colour_grading
from telescope.plugins.colour_grading import OWNER, ColourGradingPlugin
from telescope.widgets.colour_wheel import ColourWheel
from telescope.widgets.curve_editor import CurveEditor


class _Host:
    def __init__(self):
        self.stages, self.splits, self.saves = {}, {}, 0

    def set_colour_stage(self, owner, stage, order=0, source_id=None):
        self.stages[owner] = stage

    def colour_split(self, owner, position):
        self.splits[owner] = position

    def schedule_save(self):
        self.saves += 1


@pytest.fixture
def card(qt_widgets):
    host, bus = _Host(), EventBus()
    plugin = ColourGradingPlugin()
    plugin.setup(host, bus)
    panel = qt_widgets(plugin.create_panel())
    bus.phones_changed.emit(1)
    plugin.set_config({})
    return plugin, host, panel


def _stage(host) -> ColourStage:
    return host.stages.get(OWNER)


def test_it_is_an_optional_built_in_that_rides_in_presets_per_phone():
    from telescope.plugins import builtin_plugins
    plugin = next(p for p in builtin_plugins() if p.name == "colour_grading")
    assert plugin.optional and plugin.in_presets and plugin.device_local and not plugin.needs_vision


def test_a_neutral_grade_puts_nothing_in_the_pipeline(card):
    plugin, host, _panel = card
    assert _stage(host) is None
    plugin._sliders["saturation"].setValue(150)
    stage = _stage(host)
    assert stage is not None and not stage.per_channel and stage.size == 33
    assert abs(float(stage.fn(np.array([[1.0, 0.0, 0.0]], np.float32))[0, 1])) < 0.01
    plugin._sliders["saturation"].setValue(100)
    assert _stage(host) is None


def test_switching_the_grade_off_takes_it_out_and_back(card):
    plugin, host, _panel = card
    plugin._sliders["contrast"].setValue(140)
    assert _stage(host).per_channel
    plugin._switch.setChecked(False)
    assert _stage(host) is None and plugin.get_config()["on"] is False
    plugin._switch.setChecked(True)
    assert _stage(host) is not None


def test_compare_draws_the_split_on_the_preview(card):
    plugin, host, _panel = card
    assert plugin._split_row.isHidden()
    plugin._compare.setChecked(True)
    assert host.splits[OWNER] == 0.5 and not plugin._split_row.isHidden()
    plugin._split.setValue(30)
    assert host.splits[OWNER] == 0.3
    plugin._compare.setChecked(False)
    assert host.splits[OWNER] is None


def test_settings_survive_a_save_and_load(card):
    plugin, _host, _panel = card
    plugin._wheels["lift"].set_value(0.2, -0.3, emit=True)
    plugin._masters["gain"].setValue(130)
    plugin._sliders["temperature"].setValue(25)
    plugin._sliders["pivot"].setValue(500)
    plugin._show_page("curves")
    plugin._pick_channel("r")
    plugin._on_curve(((0.0, 0.0), (0.4, 0.5), (1.0, 1.0)))
    plugin._pick_curve("hue_hue")
    plugin._on_curve(((0.0, 0.6), (0.5, 0.5)))
    cfg = plugin.get_config()
    assert cfg["wheels"]["lift"] == [0.2, -0.3] and cfg["masters"]["gain"] == 1.3 and cfg["temperature"] == 25
    assert cfg["curves"]["r"] == [[0.0, 0.0], [0.4, 0.5], [1.0, 1.0]] and cfg["hue_hue"] == [[0.0, 0.6], [0.5, 0.5]]

    plugin.set_config({})
    assert plugin._grade.is_identity() and plugin._sliders["temperature"].value() == 0
    plugin.set_config(cfg)
    assert plugin.get_config() == cfg
    assert plugin._wheels["lift"].value() == (0.2, -0.3) and plugin._masters["gain"].value() == 130
    assert plugin._master_lbls["gain"].text() == "1.30" and plugin._slider_lbls["temperature"].text() == "+25"


@pytest.mark.parametrize("cfg", [
    {"temperature": "hot", "contrast": 99, "wheels": {"lift": [5, 5]}, "masters": {"gain": None}},
    {"curves": {"all": [[0.2, 0.2], [1, 1]], "r": "x"}, "hue_sat": [[0, 0.1], "bad", [0.5]], "wheels": [1, 2]},
    {"lut": 7, "lut_strength": "x", "on": "maybe"},
])
def test_odd_saved_settings_load_as_something_sensible(card, cfg):
    plugin, _host, _panel = card
    plugin.set_config(cfg)
    out = plugin.get_config()
    assert -100 <= out["temperature"] <= 100 and 0 <= out["contrast"] <= 2
    assert out["curves"]["all"][0][0] == 0.0 and out["curves"]["all"][-1][0] == 1.0
    x, y = out["wheels"]["lift"]
    assert -1 <= x <= 1 and -1 <= y <= 1
    plugin._grade(lattice(3))  # still a working grade


def test_a_lut_file_loads_shows_its_strength_and_comes_off_again(card, tmp_path):
    plugin, host, _panel = card
    path = tmp_path / "Invert.cube"
    write_cube(path, (1 - lattice(3)).reshape(3, 3, 3, 3))
    plugin._show_page("lut")
    assert plugin._strength_row.isHidden()
    plugin._load_lut(str(path))
    assert plugin._lut_lbl.fullText() == "Invert.cube" and not plugin._strength_row.isHidden()
    assert plugin._lut_note.isHidden()
    stage = _stage(host)
    assert np.allclose(stage.fn(np.array([[0.2, 0.4, 0.6]], np.float32)), [[0.8, 0.6, 0.4]], atol=1e-4)
    plugin._strength.setValue(0)
    assert _stage(host) is None
    plugin._strength.setValue(100)
    assert plugin.get_config()["lut"] == str(path)
    plugin._clear_btn.click()
    assert _stage(host) is None and plugin._lut_lbl.fullText() == "No LUT loaded"


def test_a_lut_that_cant_be_used_says_why(card, tmp_path):
    plugin, host, _panel = card
    bad = tmp_path / "Big.cube"
    bad.write_text("LUT_3D_SIZE 129\n")
    plugin._load_lut(str(bad))
    assert "129 points per side" in plugin._lut_note.text() and not plugin._lut_note.isHidden()
    assert _stage(host) is None
    plugin._load_lut(str(tmp_path / "gone.cube"))
    assert plugin._lut_note.text() == "Telescope can't open that file."


def test_a_preset_brings_its_lut_back_from_the_file(card, tmp_path):
    plugin, host, _panel = card
    path = tmp_path / "Look.cube"
    write_cube(path, (lattice(3) * 0.5).reshape(3, 3, 3, 3))
    plugin.apply_preset({"lut": str(path), "lut_strength": 0.5})
    assert plugin._grade.lut is not None and plugin._strength.value() == 50 and _stage(host) is not None


@pytest.mark.parametrize("filter_index, size", [(0, 33), (1, 65)])
def test_export_writes_the_grade_at_the_size_picked(card, tmp_path, monkeypatch, filter_index, size):
    plugin, _host, _panel = card
    plugin._sliders["saturation"].setValue(140)
    plugin._sliders["temperature"].setValue(-30)
    chosen = {}

    def save(_parent, _caption, start, filters):
        chosen["start"], chosen["filters"] = start, filters.split(";;")
        return str(tmp_path / "My look"), chosen["filters"][filter_index]
    monkeypatch.setattr(QFileDialog, "getSaveFileName", staticmethod(save))
    plugin._export()
    assert chosen["start"].endswith("Telescope grade.cube")
    back = read_cube(tmp_path / "My look.cube")
    assert back.size == size and back.title == "Telescope grade"
    assert np.abs(back.cube.reshape(-1, 3) - plugin._grade(lattice(size))).max() < 1e-5
    assert plugin._export_note.text() == "Saved My look.cube."


def test_export_says_when_it_cant_write(card, tmp_path, monkeypatch):
    plugin, _host, _panel = card
    monkeypatch.setattr(QFileDialog, "getSaveFileName",
                        staticmethod(lambda *_a: (str(tmp_path / "missing" / "x.cube"), "")))
    plugin._export()
    assert plugin._export_note.text() == "Telescope couldn't save the LUT there."


def test_reset_clears_the_grade_but_keeps_the_lut(card, tmp_path):
    plugin, host, _panel = card
    path = tmp_path / "Look.cube"
    write_cube(path, (lattice(3) * 0.5).reshape(3, 3, 3, 3))
    plugin._load_lut(str(path))
    plugin._sliders["tint"].setValue(40)
    plugin._wheels["gain"].set_value(0.5, 0.5, emit=True)
    plugin._reset_btn.click()
    assert plugin._sliders["tint"].value() == 0 and plugin._wheels["gain"].value() == (0.0, 0.0)
    assert plugin._grade.lut is not None and _stage(host) is not None


def test_pages_show_one_at_a_time_and_the_channel_row_only_for_rgb(card):
    plugin, _host, _panel = card
    plugin._page_btns["curves"].click()
    assert plugin._pages["primaries"].isHidden() and not plugin._pages["curves"].isHidden()
    assert not plugin._channel_row.isHidden()
    plugin._curve_btns["hue_sat"].click()
    assert plugin._channel_row.isHidden() and plugin._editor.points() == g.NEUTRAL_HUE
    assert not plugin._curve_note.isHidden()


def test_shortcuts_reach_the_main_controls(card):
    plugin, _host, _panel = card
    ids = {a.id for a in plugin.create_actions()}
    assert {"colour_grading.on", "colour_grading.compare", "colour_grading.temperature",
            "colour_grading.saturation"} <= ids


def test_diagnostics_say_whether_a_grade_is_on(card):
    plugin, _host, _panel = card
    assert plugin.diagnostics() == {"Colour grading": "neutral"}
    plugin._sliders["tint"].setValue(10)
    assert plugin.diagnostics() == {"Colour grading": "on"}


def test_shutting_down_takes_the_stage_and_split_out(card):
    plugin, host, _panel = card
    plugin._sliders["tint"].setValue(10)
    plugin._compare.setChecked(True)
    plugin.shutdown()
    assert _stage(host) is None and host.splits[OWNER] is None


# ── The wheel ────────────────────────────────────────────────────────────


@pytest.fixture
def wheel(qt_widgets):
    w = qt_widgets(ColourWheel())
    w.resize(140, 124)
    w.show()
    moves = []
    w.moved.connect(lambda x, y: moves.append((x, y)))
    return w, moves


def test_arrow_keys_move_the_puck_and_home_puts_it_back(wheel):
    w, moves = wheel
    QTest.keyClick(w, Qt.Key.Key_Right)
    QTest.keyClick(w, Qt.Key.Key_Up, Qt.KeyboardModifier.ShiftModifier)
    assert w.value() == (0.01, 0.05) and moves[-1] == (0.01, 0.05)
    QTest.keyClick(w, Qt.Key.Key_Home)
    assert w.value() == (0.0, 0.0)


def test_dragging_moves_the_puck_without_jumping_and_stays_in_the_circle(wheel):
    w, _moves = wheel
    start = QPoint(30, 30)
    QTest.mousePress(w, Qt.MouseButton.LeftButton, Qt.KeyboardModifier.NoModifier, start)
    assert w.value() == (0.0, 0.0)  # pressing alone doesn't move it
    QTest.mouseMove(w, start + QPoint(20, 0))
    x, y = w.value()
    assert x > 0.3 and y == 0.0
    QTest.mouseMove(w, start + QPoint(400, -400))
    x, y = w.value()
    assert abs(np.hypot(x, y) - 1) < 1e-3 and y > 0
    QTest.mouseRelease(w, Qt.MouseButton.LeftButton, Qt.KeyboardModifier.NoModifier, start)


def test_a_double_click_resets_the_wheel(wheel):
    w, moves = wheel
    w.set_value(0.4, 0.2)
    QTest.mouseDClick(w, Qt.MouseButton.LeftButton, Qt.KeyboardModifier.NoModifier, QPoint(70, 62))
    assert w.value() == (0.0, 0.0) and moves[-1] == (0.0, 0.0)


# ── The curve ────────────────────────────────────────────────────────────


@pytest.fixture
def editor(qt_widgets):
    e = qt_widgets(CurveEditor())
    e.resize(216, 216)
    e.set_curve("tone", g.LINEAR, g.LINEAR)
    e.show()
    edits = []
    e.edited.connect(lambda pts: edits.append(pts))
    return e, edits


def _at(e, x, y) -> QPoint:
    p = e._to_px(x, y)
    return QPoint(round(p.x()), round(p.y()))


def test_a_click_adds_a_point_on_the_line_and_a_drag_moves_it(editor):
    e, edits = editor
    QTest.mousePress(e, Qt.MouseButton.LeftButton, Qt.KeyboardModifier.NoModifier, _at(e, 0.5, 0.5))
    assert len(e.points()) == 3 and abs(e.points()[1][1] - 0.5) < 0.02
    QTest.mouseMove(e, _at(e, 0.5, 0.7))
    QTest.mouseRelease(e, Qt.MouseButton.LeftButton, Qt.KeyboardModifier.NoModifier, _at(e, 0.5, 0.7))
    assert abs(e.points()[1][1] - 0.7) < 0.02 and edits[-1] == e.points()


def test_the_ends_of_a_tone_curve_stay_at_the_edges(editor):
    e, _edits = editor
    QTest.mousePress(e, Qt.MouseButton.LeftButton, Qt.KeyboardModifier.NoModifier, _at(e, 0, 0))
    QTest.mouseMove(e, _at(e, 0.3, 0.2))
    QTest.mouseRelease(e, Qt.MouseButton.LeftButton, Qt.KeyboardModifier.NoModifier, _at(e, 0.3, 0.2))
    assert e.points()[0][0] == 0.0 and abs(e.points()[0][1] - 0.2) < 0.02
    QTest.mouseDClick(e, Qt.MouseButton.LeftButton, Qt.KeyboardModifier.NoModifier, _at(e, *e.points()[0]))
    assert e.points() == g.LINEAR  # an end point can't be removed, so the double-click resets the curve


def test_keys_pick_move_and_remove_points(editor):
    e, edits = editor
    e.set_curve("tone", ((0.0, 0.0), (0.5, 0.5), (1.0, 1.0)), g.LINEAR)
    QTest.keyClick(e, Qt.Key.Key_BracketRight)
    QTest.keyClick(e, Qt.Key.Key_BracketRight)
    assert e.picked() == 1
    QTest.keyClick(e, Qt.Key.Key_Up, Qt.KeyboardModifier.ShiftModifier)
    assert e.points()[1] == (0.5, 0.55)
    QTest.keyClick(e, Qt.Key.Key_Delete)
    assert e.points() == g.LINEAR
    e.set_curve("tone", ((0.0, 0.0), (0.5, 0.5), (1.0, 1.0)), g.LINEAR)
    QTest.keyClick(e, Qt.Key.Key_Home)
    assert e.points() == g.LINEAR and edits[-1] == g.LINEAR


def test_a_point_on_a_hue_curve_comes_off_with_a_double_click(editor):
    e, _edits = editor
    e.set_curve("hue", g.NEUTRAL_HUE, g.NEUTRAL_HUE)
    QTest.mouseDClick(e, Qt.MouseButton.LeftButton, Qt.KeyboardModifier.NoModifier, _at(e, 1 / 3, 0.5))
    assert len(e.points()) == 5
    QTest.mouseDClick(e, Qt.MouseButton.LeftButton, Qt.KeyboardModifier.NoModifier, _at(e, 0.6, 0.9))
    assert e.points() == g.NEUTRAL_HUE


def test_a_right_click_removes_a_point(editor):
    e, _edits = editor
    e.set_curve("tone", ((0.0, 0.0), (0.5, 0.6), (1.0, 1.0)), g.LINEAR)
    QTest.mouseClick(e, Qt.MouseButton.RightButton, Qt.KeyboardModifier.NoModifier, _at(e, 0.5, 0.6))
    assert e.points() == g.LINEAR


def test_tone_points_keep_their_order_while_dragged(editor):
    e, _edits = editor
    e.set_curve("tone", ((0.0, 0.0), (0.4, 0.4), (0.6, 0.6), (1.0, 1.0)), g.LINEAR)
    QTest.mousePress(e, Qt.MouseButton.LeftButton, Qt.KeyboardModifier.NoModifier, _at(e, 0.4, 0.4))
    QTest.mouseMove(e, _at(e, 0.9, 0.4))
    QTest.mouseRelease(e, Qt.MouseButton.LeftButton, Qt.KeyboardModifier.NoModifier, _at(e, 0.9, 0.4))
    xs = [p[0] for p in e.points()]
    assert xs == sorted(xs) and xs[1] < 0.6


def test_the_curve_drawn_is_the_one_applied(editor):
    e, _edits = editor
    pts = ((0.0, 0.1), (0.3, 0.2), (1.0, 0.9))
    e.set_curve("tone", pts, g.LINEAR)
    xs = np.linspace(0, 1, 11, dtype=np.float32)
    assert np.allclose(e._curve(xs), g.monotone_curve(pts, xs))
    e.grab()  # paints without trouble


def _mouse(e, kind, point: QPoint, buttons=Qt.MouseButton.LeftButton):
    """Sends one mouse event as the window system does, for the press, release, double-click order of a real one."""
    pos = QPointF(point)
    event = QMouseEvent(kind, pos, e.mapToGlobal(pos), Qt.MouseButton.LeftButton, buttons,
                        Qt.KeyboardModifier.NoModifier)
    QApplication.sendEvent(e, event)


@pytest.mark.parametrize("second_press", [False, True])
def test_a_real_double_click_on_the_curve_resets_it(editor, second_press):
    e, edits = editor
    e.set_curve("tone", ((0.0, 0.0), (0.3, 0.5), (1.0, 1.0)), g.LINEAR)
    where = _at(e, 0.7, float(e._curve(np.array([0.7], np.float32))[0]))
    _mouse(e, QEvent.Type.MouseButtonPress, where)
    assert len(e.points()) == 4  # the first press adds a point
    _mouse(e, QEvent.Type.MouseButtonRelease, where, Qt.MouseButton.NoButton)
    if second_press:  # some platforms send the second press too
        _mouse(e, QEvent.Type.MouseButtonPress, where)
    _mouse(e, QEvent.Type.MouseButtonDblClick, where)
    _mouse(e, QEvent.Type.MouseButtonRelease, where, Qt.MouseButton.NoButton)
    assert e.points() == g.LINEAR and edits[-1] == g.LINEAR


def test_a_real_double_click_on_a_point_still_removes_it(editor):
    e, _edits = editor
    e.set_curve("tone", ((0.0, 0.0), (0.3, 0.5), (0.7, 0.6), (1.0, 1.0)), g.LINEAR)
    where = _at(e, 0.3, 0.5)
    _mouse(e, QEvent.Type.MouseButtonPress, where)
    _mouse(e, QEvent.Type.MouseButtonRelease, where, Qt.MouseButton.NoButton)
    _mouse(e, QEvent.Type.MouseButtonDblClick, where)
    assert e.points() == ((0.0, 0.0), (0.7, 0.6), (1.0, 1.0))


def test_hue_points_stay_below_1_and_apart_round_the_hues(editor):
    e, _edits = editor
    e.set_curve("hue", g.NEUTRAL_HUE, g.NEUTRAL_HUE)
    QTest.mousePress(e, Qt.MouseButton.LeftButton, Qt.KeyboardModifier.NoModifier, _at(e, 5 / 6, 0.5))
    QTest.mouseMove(e, _at(e, 1.0, 0.7))
    QTest.mouseRelease(e, Qt.MouseButton.LeftButton, Qt.KeyboardModifier.NoModifier, _at(e, 1.0, 0.7))
    xs = [x for x, _y in e.points()]
    assert len(xs) == 6 and max(xs) < 1 and 1 - max(xs) >= 0.0099  # still apart from the point at 0
    count = len(e.points())
    QTest.mousePress(e, Qt.MouseButton.LeftButton, Qt.KeyboardModifier.NoModifier, _at(e, 1.0, 0.2))
    QTest.mouseRelease(e, Qt.MouseButton.LeftButton, Qt.KeyboardModifier.NoModifier, _at(e, 1.0, 0.2))
    assert len(e.points()) == count  # a click at 1 would land on the hue at 0


def test_arrow_keys_keep_a_hue_point_in_range(editor):
    e, _edits = editor
    e.set_curve("hue", g.NEUTRAL_HUE, g.NEUTRAL_HUE)
    QTest.keyClick(e, Qt.Key.Key_BracketRight)
    assert e.picked() == 0
    for _ in range(5):
        QTest.keyClick(e, Qt.Key.Key_Left, Qt.KeyboardModifier.ShiftModifier)
    assert e.points()[0][0] == 0.0
    QTest.keyClick(e, Qt.Key.Key_BracketLeft)
    for _ in range(10):
        QTest.keyClick(e, Qt.Key.Key_Right, Qt.KeyboardModifier.ShiftModifier)
    assert all(0 <= x < 1 for x, _y in e.points())
    assert e.points()[-1][0] == 0.99


def test_saved_hue_points_at_1_or_twice_come_back_as_one(card):
    plugin, _host, _panel = card
    plugin.set_config({"hue_sat": [[0.0, 0.5], [0.5, 0.2], [1.0, 0.5], [0.5, 0.3]]})
    assert plugin.get_config()["hue_sat"] == [[0.0, 0.5], [0.5, 0.3]]


def test_switching_phones_publishes_the_lut_once_and_reads_the_file_once(card, tmp_path, monkeypatch):
    plugin, host, _panel = card
    path = tmp_path / "Look.cube"
    write_cube(path, (lattice(3) * 0.5).reshape(3, 3, 3, 3))
    reads, published = [], []
    real = colour_grading.read_cube
    monkeypatch.setattr(colour_grading, "read_cube", lambda p: reads.append(p) or real(p))
    saved = {"lut": str(path), "saturation": 1.3}
    plugin.set_config(saved)
    monkeypatch.setattr(host, "set_colour_stage", lambda owner, stage, *a, **k: published.append(stage))
    plugin.set_config({})  # what switching phones does: the defaults, then the phone's settings
    plugin.set_config(saved)
    assert len(reads) == 1 and len(published) == 2 and published[0] is None
    assert published[1].fn.lut is not None and published[1].fn.saturation == 1.3
    write_cube(path, (lattice(3) * 0.25).reshape(3, 3, 3, 3))
    os.utime(path, ns=(1, 1))  # changed on disk: read again
    plugin.set_config(saved)
    assert len(reads) == 2


def test_export_asks_before_replacing_a_file_the_dialog_didnt_name(card, tmp_path, monkeypatch):
    plugin, _host, _panel = card
    target = tmp_path / "Mine.cube"
    target.write_text("keep me")
    monkeypatch.setattr(QFileDialog, "getSaveFileName", staticmethod(lambda *_a: (str(tmp_path / "Mine"), "")))
    asked = []
    monkeypatch.setattr(plugin, "_may_replace", lambda name: asked.append(name) or False)
    plugin._export()
    assert asked == ["Mine.cube"] and target.read_text() == "keep me"
    monkeypatch.setattr(plugin, "_may_replace", lambda name: True)
    plugin._export()
    assert target.read_text().startswith("TITLE")
