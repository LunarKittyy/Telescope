import math

import pytest
from PyQt6.QtWidgets import QDoubleSpinBox, QSpinBox

from telescope.widgets.common import (
    LogSliderRow,
    PanSliderRow,
    create_separator,
    create_vector_icon,
    log_pos_to_val,
    ns_to_display,
    quality_label,
    val_to_log_pos,
)
from telescope.widgets.lens_panel import LensPanel


@pytest.mark.parametrize(
    "value,expected",
    [
        (0, "?"),
        (-1, "?"),
        (1_000_000_000, "1.0 s"),
        (2_500_000_000, "2.5 s"),
        (500_000_000, "1/2"),
        (1_000_000, "1/1,000"),
    ],
)
def test_ns_to_display(value, expected):
    assert ns_to_display(value) == expected


@pytest.mark.parametrize(
    "quality,suffix",
    [(100, "High"), (95, "High"), (94, "Balanced"), (80, "Balanced"),
     (79, "Low"), (60, "Low"), (59, "Very low")],
)
def test_quality_label_boundaries(quality, suffix):
    assert quality_label(quality) == f"{quality}%  {suffix}"


def test_log_scale_endpoints_midpoint_and_clamping():
    assert log_pos_to_val(0, 100, 10, 1000) == pytest.approx(10)
    assert log_pos_to_val(100, 100, 10, 1000) == pytest.approx(1000)
    assert log_pos_to_val(50, 100, 10, 1000) == pytest.approx(100)
    assert log_pos_to_val(-20, 100, 10, 1000) == pytest.approx(10)
    assert log_pos_to_val(120, 100, 10, 1000) == pytest.approx(1000)
    assert log_pos_to_val(0, 0, 0, 100) == pytest.approx(1)


def test_value_to_log_position_endpoints_and_invalid_values():
    assert val_to_log_pos(10, 100, 10, 1000) == 0
    assert val_to_log_pos(1000, 100, 10, 1000) == 100
    assert val_to_log_pos(100, 100, 10, 1000) == 50
    assert val_to_log_pos(-1, 100, 10, 1000) == 0
    assert val_to_log_pos(50, 100, 0, 1000) == 0


def test_log_scale_round_trip_is_close_across_range():
    for value in (50, 100, 400, 1600, 6400):
        pos = val_to_log_pos(value, 2000, 50, 6400)
        assert log_pos_to_val(pos, 2000, 50, 6400) == pytest.approx(value, rel=0.003)


def test_separator_and_all_known_vector_icons_are_constructible(qapp):
    assert create_separator().objectName() == "separator"
    for name in ("connection", "camera", "stream", "gear", "status", "qr", "unknown"):
        assert not create_vector_icon(name, "#518cc6").isNull()


def test_integer_log_slider_syncs_slider_spin_and_signal(qapp):
    row = LogSliderRow(10, 1000, display_fn=lambda value: f"v={value}")
    emitted = []
    row.value_changed.connect(emitted.append)

    row.set_value(100)
    assert isinstance(row._spin, QSpinBox)
    assert row.get_value() == pytest.approx(100, rel=0.01)
    assert row._spin.value() == 100
    assert row._val_lbl.text() == "v=100"

    row._slider.setValue(row.STEPS)
    assert emitted[-1] == pytest.approx(1000)
    assert row._spin.value() == 1000


def test_double_log_slider_converts_scaled_spin_value(qapp):
    row = LogSliderRow(
        100_000,
        1_000_000_000,
        spinbox_scale=1e-6,
        spinbox_decimals=2,
        display_fn=lambda value: f"{value:.0f}",
    )
    emitted = []
    row.value_changed.connect(emitted.append)

    row._spin.setValue(20.5)
    row._on_spin()

    assert isinstance(row._spin, QDoubleSpinBox)
    assert emitted[-1] == pytest.approx(20_500_000)
    assert row.get_value() == pytest.approx(20_500_000, rel=0.01)


def test_log_slider_range_and_enabled_state(qapp):
    row = LogSliderRow(10, 1000)
    row.set_value(100)
    row.set_range(20, 2000)
    row.set_enabled(False)

    assert row.v_min == 20
    assert row.v_max == 2000
    assert row._spin.minimum() == 20
    assert row._spin.maximum() == 2000
    assert not row._slider.isEnabled()
    assert not row._spin.isEnabled()


def test_pan_slider_clamps_resets_and_emits(qapp):
    row = PanSliderRow()
    emitted = []
    row.value_changed.connect(emitted.append)

    row._slider.setValue(100)
    assert row.get_value() == 0.5
    assert emitted == [0.5]

    row.set_value(2)
    assert row.get_value() == 1
    row.reset()
    assert row.get_value() == 0
    row.set_enabled(False)
    assert not row._slider.isEnabled()


def test_lens_panel_load_select_placeholder_and_clear(qapp):
    panel = LensPanel()
    selected = []
    panel.lens_selected.connect(selected.append)
    cameras = [
        {"id": "wide", "label": "Wide", "current": False},
        {"id": "tele", "label": "Tele", "current": True},
    ]

    panel.load(cameras)

    assert panel._btns[0].text() == "Wide"
    assert panel._btns[1].isChecked()
    panel._btns[0].click()
    assert selected == [cameras[0]]
    assert panel._btns[0].isChecked()

    panel.set_placeholder("Unavailable")
    assert panel.layout().itemAt(0).widget().text() == "Unavailable"
    panel.clear()
    assert panel._btns == []
    assert panel._ph.text() == "Start streaming to load lenses"
