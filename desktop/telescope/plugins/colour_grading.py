import math
from pathlib import Path

from PyQt6.QtCore import QSignalBlocker, Qt
from PyQt6.QtWidgets import (
    QButtonGroup, QFileDialog, QGridLayout, QHBoxLayout, QLabel, QMessageBox, QPushButton, QSizePolicy, QVBoxLayout,
    QWidget,
)

from telescope import grade as g
from telescope.colour import ColourStage, CubeError, lattice, read_cube, write_cube
from telescope.grade import Grade
from telescope.i18n import N_, number, percent, tr
from telescope.plugin import TelescopePlugin
from telescope.shortcuts import button_action, slider_action
from telescope.widgets.colour_wheel import ColourWheel
from telescope.widgets.common import (
    ElidingLabel, NoScrollSlider, SegmentButton, Switch, add_card_header, card_hue, card_layout, control_row,
    control_row_widget, create_card, dim_until_paired, icon_button, segmented_row, set_status_kind, slider_row, ui_px,
    value_label, wrapped_note,
)
from telescope.widgets.curve_editor import CurveEditor

OWNER = "colour_grading"
_LUT_FILES = N_("LUT files (*.cube)")
_EXPORT_SIZES = ((33, N_("33-point LUT (*.cube)")), (65, N_("65-point LUT (*.cube)")))
PAGES = (("primaries", N_("Primaries")), ("curves", N_("Curves")), ("lut", N_("LUT")))
CURVES = (("tone", N_("RGB")), ("hue_sat", N_("Hue vs sat")), ("hue_hue", N_("Hue vs hue")))
CHANNELS = (("all", N_("All")), ("r", N_("Red")), ("g", N_("Green")), ("b", N_("Blue")))
_CHANNEL_COLOURS = {"all": "#e7eaef", "r": "#ef6f6b", "g": "#5fc98c", "b": "#8cc4f2"}
WHEEL_NAMES = {"lift": N_("Lift"), "gamma": N_("Gamma"), "gain": N_("Gain"), "offset": N_("Offset")}
_WHEEL_TIPS = {"lift": N_("Shadows: colour and level, with white staying put"),
               "gamma": N_("Midtones: colour and brightness, with black and white staying put"),
               "gain": N_("Highlights: colour and level, with black staying put"),
               "offset": N_("Everything: colour and level")}
# key: (label, lowest, highest, slider steps per unit, decimals, signed, tooltip)
SLIDERS = {
    "temperature": (N_("Temperature"), -100, 100, 1, 0, True, N_("Warmer or cooler")),
    "tint": (N_("Tint"), -100, 100, 1, 0, True, N_("More magenta or more green")),
    "contrast": (N_("Contrast"), 0.0, 2.0, 100, 2, False, N_("Spreads the tones apart around the pivot")),
    "pivot": (N_("Pivot"), 0.0, 1.0, 1000, 3, False, N_("The level contrast turns around")),
    "saturation": (N_("Saturation"), 0.0, 2.0, 100, 0, False, N_("How colourful the picture is")),
    "shadows": (N_("Shadows"), -100, 100, 1, 0, True, N_("Brightens or darkens the darker parts")),
    "highlights": (N_("Highlights"), -100, 100, 1, 0, True, N_("Brightens or darkens the brighter parts")),
}
_DEFAULTS = Grade()
_MASTER_STEPS = {"lift": 1000, "gamma": 100, "gain": 100, "offset": 1000}
_KEEP_PARSED = 4  # .cube files kept parsed, so switching phones doesn't read a 65-point LUT again (about 0.3 s)
_parsed: dict = {}  # (path, mtime, size): CubeLut, the newest last


def _number(value, default: float, lo: float, hi: float) -> float:
    try:
        value = float(value)
    except (TypeError, ValueError):
        return default
    return min(hi, max(lo, value)) if value == value else default


def _points(raw, default: tuple, tone: bool) -> tuple:
    """Saved curve points, cleaned up: pairs of numbers 0..1, sorted, a tone curve's ends at 0 and 1, a hue curve's x
    below 1."""
    if not isinstance(raw, (list, tuple)):
        return default
    pts = []
    for item in raw[:64]:
        if isinstance(item, (list, tuple)) and len(item) == 2:
            pts.append((_number(item[0], 0.0, 0.0, 1.0), _number(item[1], 0.0, 0.0, 1.0)))
    if not tone:  # a hue of 1 is the hue at 0 again, and one hue gets one point
        pts = list({x % 1.0: (x % 1.0, y) for x, y in pts}.values())
    pts.sort()
    if tone and (len(pts) < 2 or pts[0][0] != 0.0 or pts[-1][0] != 1.0):
        return default
    return tuple(pts) if pts else default


def _read_lut(path: str) -> tuple:
    """(the CubeLut at path or None, the problem with it in words or ""), from _parsed while the file is unchanged."""
    try:
        info = Path(path).stat()
        key = (path, info.st_mtime_ns, info.st_size)
        if key in _parsed:
            _parsed[key] = _parsed.pop(key)
            return _parsed[key], ""
        lut = read_cube(path)
    except CubeError as err:
        return None, tr(err.text, **err.fields)
    except (OSError, UnicodeError):
        return None, tr("Telescope can't open that file.")
    while len(_parsed) >= _KEEP_PARSED:
        del _parsed[next(iter(_parsed))]
    _parsed[key] = lut
    return lut, ""


class ColourGradingPlugin(TelescopePlugin):
    name = "colour_grading"
    title = N_("Colour grading")
    about = N_("Colour wheels, curves and LUT files for the look of the picture.")
    category = "look"
    icon = "colour"
    when_off = N_("Without it there's no Colour grading card and the picture keeps the camera's colours.")
    optional = True
    in_presets = True
    device_local = True  # each phone has its own look to fix
    panel_region = "right"

    def setup(self, host, bus):
        self._host, self._bus = host, bus
        self._grade = Grade()
        self._on = True
        self._lut_path = ""
        self._lut_problem = ""
        self._curve = "tone"
        self._channel = "all"

    # ── Card ──────────────────────────────────────────────────────────────

    def create_panel(self) -> QWidget:
        card = create_card()
        lay = card_layout(card)
        self._switch = Switch("")
        self._switch.setSizePolicy(QSizePolicy.Policy.Fixed, QSizePolicy.Policy.Fixed)
        self._switch.setChecked(True)
        self._switch.setToolTip(tr("Switch the grade off to see the camera's own colours"))
        self._switch.setAccessibleName(tr("Colour grading"))
        self._switch.toggled.connect(self._on_switch)
        add_card_header(lay, tr("Colour grading"), self.icon, action=self._switch, hue=self.category)
        dim_until_paired(card, self._bus)

        self._compare = Switch("")
        self._compare.setToolTip(tr("Left of the line the preview shows the picture without the grade (apps always get "
                                    "it graded)"))
        self._compare.toggled.connect(self._on_compare)
        lay.addLayout(control_row(tr("Compare"), self._compare))
        self._split = self._slider(0, 100, 50, tr("Where the line between before and after is"))
        self._split_lbl = value_label()
        self._split.valueChanged.connect(self._on_split)
        self._split_row = control_row_widget(tr("Split"), slider_row(self._split, self._split_lbl), stretch=True)
        lay.addWidget(self._split_row)

        self._page_btns = {}
        pages = QButtonGroup(card)
        for key, text in PAGES:
            btn = SegmentButton(tr(text), value=key)
            btn.clicked.connect(lambda _c=False, k=key: self._show_page(k))
            pages.addButton(btn)
            self._page_btns[key] = btn
        self._reset_btn = icon_button("reset", tr("Reset the grade"))
        self._reset_btn.clicked.connect(self._reset)
        top = QHBoxLayout()
        top.setSpacing(8)
        top.addLayout(segmented_row(*self._page_btns.values()), 1)
        top.addWidget(self._reset_btn)
        lay.addLayout(top)

        self._pages = {"primaries": self._primaries_page(), "curves": self._curves_page(), "lut": self._lut_page()}
        for page in self._pages.values():
            lay.addWidget(page)
        self._page_btns["primaries"].setChecked(True)
        self._show_page("primaries")
        self._show_controls()
        return card

    def _slider(self, lo: int, hi: int, default: int, tip: str = "") -> NoScrollSlider:
        slider = NoScrollSlider(Qt.Orientation.Horizontal)
        slider.setRange(lo, hi)
        slider.setValue(default)
        slider.set_default(default)
        if tip:
            slider.setToolTip(tip)
        return slider

    def _page(self) -> tuple:
        page = QWidget()
        lay = QVBoxLayout(page)
        lay.setContentsMargins(0, 4, 0, 0)
        lay.setSpacing(8)
        return page, lay

    def _primaries_page(self) -> QWidget:
        page, lay = self._page()
        grid = QGridLayout()
        grid.setHorizontalSpacing(ui_px(12))
        grid.setVerticalSpacing(ui_px(10))
        self._wheels, self._masters, self._master_lbls = {}, {}, {}
        for i, name in enumerate(g.WHEELS):
            grid.addWidget(self._wheel_cell(name), i // 2, i % 2)
        lay.addLayout(grid)
        self._sliders, self._slider_lbls = {}, {}
        for key, (label, lo, hi, steps, _decimals, _signed, tip) in SLIDERS.items():
            default = round(getattr(_DEFAULTS, key) * steps)
            slider = self._slider(round(lo * steps), round(hi * steps), default, tr(tip))
            readout = value_label()
            slider.valueChanged.connect(lambda _v, k=key: self._on_slider(k))
            self._sliders[key], self._slider_lbls[key] = slider, readout
            lay.addLayout(control_row(tr(label), slider_row(slider, readout), stretch=True))
        return page

    def _wheel_cell(self, name: str) -> QWidget:
        cell = QWidget()
        lay = QVBoxLayout(cell)
        lay.setContentsMargins(0, 0, 0, 0)
        lay.setSpacing(4)
        head = QHBoxLayout()
        title = QLabel(tr(WHEEL_NAMES[name]))
        title.setObjectName("form_label")
        readout = value_label()
        head.addWidget(title)
        head.addStretch(1)
        head.addWidget(readout)
        lay.addLayout(head)
        wheel = ColourWheel()
        wheel.setAccessibleName(tr("{wheel} colour", wheel=tr(WHEEL_NAMES[name])))
        wheel.setToolTip("\n".join((tr(_WHEEL_TIPS[name]), tr(ColourWheel.HINT))))
        wheel.moved.connect(lambda x, y, n=name: self._on_wheel(n, x, y))
        lay.addWidget(wheel)
        lo, hi = g.MASTER_RANGE[name]
        steps = _MASTER_STEPS[name]
        master = self._slider(round(lo * steps), round(hi * steps), round(g.MASTER_DEFAULT[name] * steps),
                              tr(_WHEEL_TIPS[name]))
        master.setAccessibleName(tr(WHEEL_NAMES[name]))
        master.valueChanged.connect(lambda _v, n=name: self._on_master(n))
        lay.addWidget(master)
        self._wheels[name], self._masters[name], self._master_lbls[name] = wheel, master, readout
        return cell

    def _curves_page(self) -> QWidget:
        page, lay = self._page()
        self._curve_btns = {}
        group = QButtonGroup(page)
        for key, text in CURVES:
            btn = SegmentButton(tr(text), value=key)
            btn.clicked.connect(lambda _c=False, k=key: self._pick_curve(k))
            group.addButton(btn)
            self._curve_btns[key] = btn
        self._curve_btns["tone"].setChecked(True)
        lay.addLayout(control_row(tr("Curve"), segmented_row(*self._curve_btns.values()), stretch=True))
        self._channel_btns = {}
        channels = QButtonGroup(page)
        for key, text in CHANNELS:
            btn = SegmentButton(tr(text), value=key)
            btn.clicked.connect(lambda _c=False, k=key: self._pick_channel(k))
            channels.addButton(btn)
            self._channel_btns[key] = btn
        self._channel_btns["all"].setChecked(True)
        self._channel_row = control_row_widget(tr("Channel"), segmented_row(*self._channel_btns.values()),
                                               stretch=True)
        lay.addWidget(self._channel_row)
        self._editor = CurveEditor()
        self._editor.setAccessibleName(tr("Curve"))
        self._editor.edited.connect(self._on_curve)
        lay.addWidget(self._editor)
        self._curve_note = wrapped_note("")
        lay.addWidget(self._curve_note)
        return page

    def _lut_page(self) -> QWidget:
        page, lay = self._page()
        self._load_btn = QPushButton(tr("Load…"))
        self._load_btn.setToolTip(tr("Pick a .cube LUT to put on top of the grade"))
        self._load_btn.clicked.connect(self._choose_lut)
        self._lut_lbl = ElidingLabel(tr("No LUT loaded"), mode=Qt.TextElideMode.ElideMiddle)
        self._lut_lbl.setObjectName("setting_sub")
        self._clear_btn = icon_button("x", tr("Take the LUT off"))
        self._clear_btn.clicked.connect(self._clear_lut)
        pick = QHBoxLayout()
        pick.setSpacing(8)
        pick.addWidget(self._load_btn)
        pick.addWidget(self._lut_lbl, 1)
        pick.addWidget(self._clear_btn)
        lay.addLayout(control_row(tr("LUT"), pick, stretch=True))
        self._strength = self._slider(0, 100, 100, tr("How much of the LUT's look goes on the picture"))
        self._strength_lbl = value_label()
        self._strength.valueChanged.connect(self._on_strength)
        self._strength_row = control_row_widget(tr("Strength"), slider_row(self._strength, self._strength_lbl),
                                                stretch=True)
        lay.addWidget(self._strength_row)
        self._lut_note = wrapped_note("", "status_warn")
        lay.addWidget(self._lut_note)
        self._export_btn = QPushButton(tr("Export grade…"))
        self._export_btn.setToolTip(tr("Save the grade, LUT included, as a .cube file for other apps"))
        self._export_btn.clicked.connect(self._export)
        row = QHBoxLayout()
        row.addWidget(self._export_btn)
        row.addStretch(1)
        lay.addLayout(control_row(tr("Export"), row, stretch=True))
        self._export_note = wrapped_note("")
        lay.addWidget(self._export_note)
        return page

    def create_actions(self) -> list:
        group = tr("Colour grading")
        actions = [button_action("colour_grading.on", tr("Colour grading"), group, self._switch),
                   button_action("colour_grading.compare", tr("Compare before and after"), group, self._compare)]
        for key in ("temperature", "tint", "saturation", "contrast"):
            label, _lo, _hi, steps, decimals, signed, _tip = SLIDERS[key]
            scale = 100 / steps if key == "saturation" else 1 / steps
            actions.append(slider_action(f"colour_grading.{key}", tr(label), group, self._sliders[key], scale=scale,
                                         decimals=0 if key == "saturation" else decimals,
                                         suffix="%" if key == "saturation" else "", signed=signed,
                                         readout=self._slider_lbls[key],
                                         step=10.0 if key == "saturation" else (0.05 if key == "contrast" else 5.0)))
        return actions

    # ── Handlers (Qt thread) ──────────────────────────────────────────────

    def _show_page(self, key: str):
        self._page_btns[key].setChecked(True)
        for name, page in self._pages.items():
            page.setVisible(name == key)

    def _pick_curve(self, key: str):
        self._curve = key
        self._curve_btns[key].setChecked(True)
        self._show_curve()

    def _pick_channel(self, key: str):
        self._channel = key
        self._channel_btns[key].setChecked(True)
        self._show_curve()

    def _on_switch(self, on: bool):
        self._on = on
        self._publish()
        self._host.schedule_save()

    def _on_compare(self, on: bool):
        self._split_row.setVisible(on)
        self._host.colour_split(OWNER, self._split.value() / 100 if on else None)

    def _on_split(self, value: int):
        self._split_lbl.setText(percent(value))
        if self._compare.isChecked():
            self._host.colour_split(OWNER, value / 100)

    def _on_wheel(self, name: str, x: float, y: float):
        self._set_grade(wheels={**self._grade.wheels, name: (x, y)})

    def _on_master(self, name: str):
        value = self._masters[name].value() / _MASTER_STEPS[name]
        self._show_master(name, value)
        self._set_grade(masters={**self._grade.masters, name: value})

    def _on_slider(self, key: str):
        value = self._sliders[key].value() / SLIDERS[key][3]
        self._show_slider(key, value)
        self._set_grade(**{key: value})

    def _on_curve(self, points: tuple):
        if self._curve == "tone":
            self._set_grade(curves={**self._grade.curves, self._channel: points})
        else:
            self._set_grade(**{self._curve: points})

    def _on_strength(self, value: int):
        self._strength_lbl.setText(percent(value))
        self._set_grade(lut_strength=value / 100)

    def _set_grade(self, **changes):
        self._grade = self._grade.changed(**changes)
        self._publish()
        self._host.schedule_save()

    def _reset(self):
        lut, strength = self._grade.lut, self._grade.lut_strength
        self._grade = Grade(lut=lut, lut_strength=strength)  # the LUT has its own way off
        self._show_controls()
        self._publish()
        self._host.schedule_save()

    def _publish(self):
        grade = self._grade
        if not self._on or grade.is_identity():
            self._host.set_colour_stage(OWNER, None)
            return
        self._host.set_colour_stage(OWNER, ColourStage(grade, per_channel=grade.per_channel(), size=grade.lut_size()))

    # ── LUT files ─────────────────────────────────────────────────────────

    def _start_folder(self) -> str:
        return str(Path(self._lut_path).parent) if self._lut_path else str(Path.home())

    def _choose_lut(self):
        path, _filter = QFileDialog.getOpenFileName(self._load_btn.window(), tr("Load a LUT"), self._start_folder(),
                                                    tr(_LUT_FILES))
        if path:
            self._load_lut(path)
            self._host.schedule_save()

    def _load_lut(self, path: str):
        lut, problem = _read_lut(path) if path else (None, "")
        self._lut_path, self._lut_problem = path, problem
        self._grade = self._grade.changed(lut=lut)
        self._show_lut()
        self._publish()

    def _clear_lut(self):
        self._load_lut("")
        self._host.schedule_save()

    def _export(self):
        folder = Path(self._lut_path).parent if self._lut_path else Path.home()
        filters = [tr(text) for _size, text in _EXPORT_SIZES]
        path, chosen = QFileDialog.getSaveFileName(self._export_btn.window(), tr("Export the grade"),
                                                   str(folder / "Telescope grade.cube"), ";;".join(filters))
        if not path:
            return
        if not path.lower().endswith(".cube"):
            path += ".cube"  # the dialog only asked about the name without it
            if Path(path).exists() and not self._may_replace(Path(path).name):
                return
        size = next((s for s, text in _EXPORT_SIZES if tr(text) == chosen), 33)
        grade = self._grade if self._on else Grade()
        try:
            write_cube(path, grade(lattice(size)).reshape(size, size, size, 3), "Telescope grade")
        except OSError:
            self._export_note.setText(tr("Telescope couldn't save the LUT there."))
            set_status_kind(self._export_note, "status_warn")
            return
        self._export_note.setText(tr("Saved {file}.", file=Path(path).name))
        set_status_kind(self._export_note, "dim")

    def _may_replace(self, name: str) -> bool:
        box = QMessageBox(QMessageBox.Icon.Question, tr("Export the grade"),
                          tr("{file} is already there. Replace it?", file=name), parent=self._export_btn.window())
        replace = box.addButton(tr("Replace"), QMessageBox.ButtonRole.AcceptRole)
        box.setDefaultButton(box.addButton(tr("Cancel"), QMessageBox.ButtonRole.RejectRole))
        box.exec()
        return box.clickedButton() is replace

    # ── Showing the settings ──────────────────────────────────────────────

    def _show_controls(self):
        """Every control from self._grade, without each one handing it back."""
        grade = self._grade
        for name in g.WHEELS:
            with QSignalBlocker(self._wheels[name]):
                self._wheels[name].set_value(*grade.wheels.get(name, (0.0, 0.0)))
            value = grade.masters.get(name, g.MASTER_DEFAULT[name])
            with QSignalBlocker(self._masters[name]):
                self._masters[name].setValue(round(value * _MASTER_STEPS[name]))
            self._show_master(name, value)
        for key, spec in SLIDERS.items():
            value = getattr(grade, key)
            with QSignalBlocker(self._sliders[key]):
                self._sliders[key].setValue(round(value * spec[3]))
            self._show_slider(key, value)
        with QSignalBlocker(self._strength):
            self._strength.setValue(round(grade.lut_strength * 100))
        self._strength_lbl.setText(percent(grade.lut_strength * 100))
        self._on_split(self._split.value())
        self._split_row.setVisible(self._compare.isChecked())
        self._show_curve()
        self._show_lut()

    def _show_master(self, name: str, value: float):
        self._master_lbls[name].setText(number(value, 2, signed=name != "gain" and round(value, 2) != 0))

    def _show_slider(self, key: str, value: float):
        _label, _lo, _hi, _steps, decimals, signed, _tip = SLIDERS[key]
        if key == "saturation":
            text = percent(value * 100)
        else:
            text = number(value, decimals, signed=signed and value != 0)
        self._slider_lbls[key].setText(text)

    def _show_curve(self):
        tone = self._curve == "tone"
        self._channel_row.setVisible(tone)
        if tone:
            points = self._grade.curves.get(self._channel, g.LINEAR)
            self._editor.set_curve("tone", points, g.LINEAR, _CHANNEL_COLOURS[self._channel])
            self._curve_note.setVisible(False)
            return
        points = getattr(self._grade, self._curve)
        self._editor.set_curve("hue", points, g.NEUTRAL_HUE, card_hue(self._editor))
        self._curve_note.setText(tr("Up gives a hue more colour, down takes it out.") if self._curve == "hue_sat"
                                 else tr("Up turns a hue towards the next one along, down towards the one before."))
        self._curve_note.setVisible(True)

    def _show_lut(self):
        path, lut = self._lut_path, self._grade.lut
        self._lut_lbl.setText(Path(path).name if path else tr("No LUT loaded"))
        self._lut_lbl.setToolTip(path)
        self._clear_btn.setVisible(bool(path))
        self._strength_row.setVisible(lut is not None)
        self._lut_note.setText(self._lut_problem)
        self._lut_note.setVisible(bool(self._lut_problem))

    # ── Config ────────────────────────────────────────────────────────────

    def get_config(self) -> dict:
        gr = self._grade
        cfg = {"on": self._on, "wheels": {k: list(v) for k, v in gr.wheels.items()}, "masters": dict(gr.masters),
               "curves": {k: [list(p) for p in v] for k, v in gr.curves.items()},
               "hue_sat": [list(p) for p in gr.hue_sat], "hue_hue": [list(p) for p in gr.hue_hue],
               "lut": self._lut_path, "lut_strength": gr.lut_strength}
        cfg.update({key: getattr(gr, key) for key in SLIDERS})
        return cfg

    def set_config(self, cfg: dict):
        cfg = cfg if isinstance(cfg, dict) else {}
        wheels_raw = cfg.get("wheels") if isinstance(cfg.get("wheels"), dict) else {}
        masters_raw = cfg.get("masters") if isinstance(cfg.get("masters"), dict) else {}
        curves_raw = cfg.get("curves") if isinstance(cfg.get("curves"), dict) else {}
        wheels = {}
        for name in g.WHEELS:
            raw = wheels_raw.get(name)
            x, y = (raw if isinstance(raw, (list, tuple)) and len(raw) == 2 else (0.0, 0.0))
            x, y = _number(x, 0.0, -1.0, 1.0), _number(y, 0.0, -1.0, 1.0)
            r = math.hypot(x, y)
            wheels[name] = (x / r, y / r) if r > 1 else (x, y)  # on the rim, as the wheel shows it
        masters = {name: _number(masters_raw.get(name), g.MASTER_DEFAULT[name], *g.MASTER_RANGE[name])
                   for name in g.WHEELS}
        values = {}
        for key, (_label, lo, hi, _steps, _d, _s, _tip) in SLIDERS.items():
            values[key] = _number(cfg.get(key), getattr(_DEFAULTS, key), lo, hi)
        curves = {c: _points(curves_raw.get(c), g.LINEAR, True) for c in g.CHANNELS}
        path = cfg.get("lut") if isinstance(cfg.get("lut"), str) else ""
        lut, self._lut_problem = _read_lut(path) if path else (None, "")
        self._lut_path = path
        self._grade = Grade(wheels=wheels, masters=masters, curves=curves,
                            hue_sat=_points(cfg.get("hue_sat"), g.NEUTRAL_HUE, False),
                            hue_hue=_points(cfg.get("hue_hue"), g.NEUTRAL_HUE, False), lut=lut,
                            lut_strength=_number(cfg.get("lut_strength"), 1.0, 0.0, 1.0), **values)
        self._on = cfg.get("on", True) is not False
        with QSignalBlocker(self._switch):
            self._switch.setChecked(self._on)
        self._show_controls()
        self._publish()  # once, with the whole grade, so the stream never sees half of it

    def diagnostics(self) -> dict:
        if not self._on or self._grade.is_identity():
            return {"Colour grading": "off" if not self._on else "neutral"}
        return {"Colour grading": "on" + (", with a LUT" if self._grade.lut is not None else "")}

    def shutdown(self):
        self._host.set_colour_stage(OWNER, None)
        self._host.colour_split(OWNER, None)

