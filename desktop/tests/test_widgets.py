import math

import pytest
from PyQt6.QtCore import Qt
from PyQt6.QtWidgets import (
    QCheckBox, QDoubleSpinBox, QLabel, QRadioButton, QSizePolicy, QSpinBox,
    QWidget,
)

from telescope.widgets.common import (
    FlowLayout,
    LogSliderRow,
    NoScrollSlider,
    PanSliderRow,
    control_row,
    control_row_widget,
    create_separator,
    create_vector_icon,
    log_pos_to_val,
    make_segmented,
    ns_to_display,
    quality_label,
    segmented_row,
    stretch_slider,
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


def test_make_segmented_marks_ends_and_middles(qapp):
    buttons = [QRadioButton(str(i)) for i in range(3)]

    make_segmented(*buttons)

    assert [b.property("segPos") for b in buttons] == ["first", "mid", "last"]
    assert all(b.property("segmented") for b in buttons)


def test_make_segmented_handles_a_lone_button(qapp):
    button = QCheckBox("only")
    make_segmented(button)
    assert button.property("segPos") == "only"


def test_segmented_row_packs_buttons_without_gaps(qapp):
    a, b = QRadioButton("a"), QRadioButton("b")

    row = segmented_row(a, b)

    assert row.spacing() == 0
    assert row.count() == 2
    assert row.itemAt(0).widget() is a
    assert row.itemAt(1).widget() is b


def test_stretch_slider_sets_a_floor_not_a_fixed_width(qapp):
    slider = NoScrollSlider()

    stretch_slider(slider, 90)

    assert slider.minimumWidth() == 90
    assert slider.maximumWidth() > 90
    assert slider.sizePolicy().horizontalPolicy() == QSizePolicy.Policy.Expanding


def test_control_row_anchors_the_control_to_the_right_edge(qapp):
    control = QLabel("x")
    tight = control_row("Label", control)
    # Without stretch, spacer goes before control, sitting flush right.
    assert tight.count() == 3
    assert tight.itemAt(1).widget() is None
    assert tight.itemAt(2).widget() is control

    wide = control_row("Label", QLabel("x"), stretch=True)
    assert wide.count() == 2
    assert wide.stretch(1) == 1


def test_form_labels_are_left_aligned(qapp):
    from telescope.widgets.common import form_label
    assert form_label("Label").alignment() & Qt.AlignmentFlag.AlignLeft


def test_flow_layout_wraps_and_reports_height_for_width(qapp):
    from PyQt6.QtWidgets import QPushButton
    host = QWidget()
    flow = FlowLayout(host, spacing=4)
    for _ in range(4):
        btn = QPushButton("wide-ish")
        btn.setFixedSize(100, 20)
        flow.addWidget(btn)

    # Two per row at 220px, four per row at 460px.
    assert flow.heightForWidth(220) > flow.heightForWidth(460)
    assert flow.count() == 4

    taken = flow.takeAt(0)
    assert taken is not None
    assert flow.count() == 3


def test_control_row_widget_is_hideable(qapp):
    row = control_row_widget("Label", QLabel("x"))
    row.setVisible(False)
    assert row.isHidden()


def test_uniform_flow_divides_each_row_evenly(qapp):
    from PyQt6.QtWidgets import QPushButton
    host = QWidget()
    flow = FlowLayout(host, spacing=10, uniform=True)
    for _ in range(3):
        btn = QPushButton("x")
        btn.setFixedSize(100, 20)
        flow.addWidget(btn)
    host.resize(320, 200)
    host.show()
    qapp.processEvents()

    widths = [flow.itemAt(i).geometry().width() for i in range(3)]
    # Pills same width; row ends flush with container, not trailing.
    assert len(set(widths)) == 1
    assert flow.itemAt(2).geometry().right() + 1 == 320


def test_eliding_label_keeps_its_full_text_available(qapp):
    from telescope.widgets.common import ElidingLabel
    from PyQt6.QtWidgets import QVBoxLayout
    host = QWidget()
    label = ElidingLabel("a considerably longer piece of status text")
    QVBoxLayout(host).addWidget(label)
    host.resize(90, 40)
    host.show()
    qapp.processEvents()

    assert label.text() != label.fullText()
    assert label.toolTip() == label.fullText()
    assert label.minimumWidth() == 1
