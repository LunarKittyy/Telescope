import pytest
from PyQt6.QtCore import Qt
from PyQt6.QtTest import QTest

from telescope.widgets.level_meter import CLIP_HOLD_S, FLOOR_DB, HOLD_S, LevelMeter, to_db


class _Clock:
    def __init__(self):
        self.t = 100.0

    def __call__(self):
        return self.t


def _meter():
    clock = _Clock()
    return LevelMeter(clock=clock), clock


def test_to_db_floors_silence():
    assert to_db(0) == FLOOR_DB and to_db(1e-9) == FLOOR_DB
    assert to_db(1.0) == 0 and to_db(0.5) == pytest.approx(-6.02, abs=0.01)


def test_peak_holds_then_sinks(qapp):
    m, clock = _meter()
    m.set_level(0.5, 0.1)
    held = m.held_db()
    for _ in range(int(HOLD_S / 0.1) - 1):
        clock.t += 0.1
        m.set_level(0.01, 0.01)
    assert m.held_db() == held  # still holding
    for _ in range(5):
        clock.t += 0.1
        m.set_level(0.01, 0.01)
    assert m.held_db() < held


def test_a_louder_peak_moves_the_hold_up_at_once(qapp):
    m, clock = _meter()
    m.set_level(0.1, 0.05)
    clock.t += 0.03
    m.set_level(0.8, 0.3)
    assert m.held_db() == pytest.approx(to_db(0.8))


def test_clip_light_latches_for_a_while_and_a_click_clears_it(qapp):
    m, clock = _meter()
    m.set_level(0.9, 0.3)
    assert not m.clipping()
    m.set_level(1.0, 0.5)
    clock.t += CLIP_HOLD_S - 0.1
    m.set_level(0.01, 0.01)
    assert m.clipping()
    clock.t += 0.2
    assert not m.clipping()
    m.set_level(1.0, 0.5)
    QTest.mouseClick(m, Qt.MouseButton.LeftButton)
    assert not m.clipping()



def test_limiting_lights_the_dot_without_counting_as_a_clip(qapp):
    m, clock = _meter()
    m.set_level(0.89, 0.3, limited=True)
    assert m.limiting() and not m.clipping()
    clock.t += CLIP_HOLD_S + 0.1
    assert not m.limiting()
