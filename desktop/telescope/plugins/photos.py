"""Photos: takes full-quality photos on the phone through the host's stills service and saves them in a folder."""
import math
from pathlib import Path
from typing import Optional

from PyQt6.QtCore import QStandardPaths, Qt, QUrl
from PyQt6.QtGui import QDesktopServices
from PyQt6.QtWidgets import QApplication, QButtonGroup, QFileDialog, QHBoxLayout, QPushButton, QWidget

from telescope.plugin import TelescopePlugin
from telescope.shortcuts import SHORTCUT_VALUE_ROLE, button_action, choice_action
from telescope.stills import StillCaps, StillProgress, StillResult
from telescope.view import ROTATIONS
from telescope.widgets.common import (
    ElidingLabel, NoScrollComboBox, NoScrollSlider, NoScrollSpinBox, SegmentButton, Switch, action_button,
    add_card_header, card_layout, control_row, control_row_widget, create_card, dim_until_paired, segmented_row,
    set_status_kind, slider_row, value_label, wrapped_note,
)
from telescope.i18n import N_, tr, trn

FORMATS = (("jpeg", N_("JPEG")), ("heic", N_("HEIC")), ("ultrahdr", N_("Ultra HDR")), ("raw", N_("RAW (DNG)")),
           ("raw+jpeg", N_("RAW and JPEG")))
MODES = (("normal", N_("Normal")), ("max_res", N_("Full resolution")), ("night", N_("Night")), ("hdr", N_("HDR")),
         ("bokeh", N_("Bokeh")), ("auto", N_("Auto")))
EXTENSIONS = ("night", "hdr", "bokeh", "auto")
SHOTS = (("single", N_("Single")), ("burst", N_("Burst")), ("bracket", N_("Exposure bracket")),
         ("focus", N_("Focus bracket")))
# Exposure brackets: (key, label, EV steps)
BRACKETS = (("3x1", N_("3 photos, 1 EV apart"), (-1, 0, 1)), ("3x2", N_("3 photos, 2 EV apart"), (-2, 0, 2)),
            ("5x1", N_("5 photos, 1 EV apart"), (-2, -1, 0, 1, 2)), ("7x1", N_("7 photos, 1 EV apart"),
                                                                       (-3, -2, -1, 0, 1, 2, 3)))
EXPOSURES_S = (0.5, 1, 2, 4, 8, 15, 30, 60, 120)  # long exposures offered, up to what the sensor allows
TIMERS = (0, 3, 10)
DISTORTION = (("camera", N_("As the camera does it")), ("on", N_("On")), ("off", N_("Off")))
DEFAULTS = {"format": "jpeg", "quality": 95, "mode": "normal", "shots": "single", "burst": 5, "bracket": "3x1",
            "focus_steps": 5, "exposure_s": 0, "timer": 0, "best": True, "distortion": "camera"}
_PHONE_ROTATION = {None: 0, ROTATIONS["90 CW"]: 90, ROTATIONS["180"]: 180, ROTATIONS["90 CCW"]: 270}


def offered(caps: StillCaps, choices: dict) -> dict:
    """What the card offers for this lens, given what's picked: {"formats", "modes", "shots", "exposures"} as keys.
    A maker's mode only saves single JPEGs or bursts of them and sets its own exposure; full resolution only
    saves JPEG and RAW."""
    mode = choices.get("mode")
    modes = ["normal"] + (["max_res"] if caps.max_res else []) + [m for m in EXTENSIONS if m in caps.extensions]
    if mode not in modes:
        mode = "normal"
    raw = "raw" in caps.formats and (mode != "max_res" or caps.max_res_raw)
    if mode in EXTENSIONS:
        formats = ["jpeg"]
    else:
        formats = ["jpeg"] + ([f for f in ("heic", "ultrahdr") if f in caps.formats] if mode == "normal" else [])
        formats += ["raw", "raw+jpeg"] if raw else []
    shots = ["single"] + (["burst"] if caps.burst_max > 1 else [])
    if mode not in EXTENSIONS:
        shots += (["bracket"] if caps.bracketing else []) + (["focus"] if caps.focus_bracketing else [])
    long = mode not in EXTENSIONS and choices.get("shots") != "bracket" and caps.exposure_max_ns > 0
    exposures = [0] + ([s for s in EXPOSURES_S if s * 1e9 <= caps.exposure_max_ns] if long else [])
    return {"formats": formats, "modes": modes, "shots": shots, "exposures": exposures}


def pick(value, allowed: list):
    """value when it's allowed, else the first that is."""
    return value if value in allowed else allowed[0]


def build_options(caps: StillCaps, choices: dict, rotation: int = 0) -> dict:
    """The /v1/still options for the picks, fitted to what this lens offers."""
    can = offered(caps, choices)
    mode = pick(choices.get("mode"), can["modes"])
    fmt = pick(choices.get("format"), can["formats"])
    shots = pick(choices.get("shots"), can["shots"])
    opts: dict = {"format": "none" if fmt == "raw" else fmt.replace("raw+", ""), "rotation": rotation}
    if fmt.startswith("raw"):
        opts["raw"] = 1
    if fmt != "raw":
        opts["quality"] = min(100, max(1, int(choices.get("quality", DEFAULTS["quality"]))))
    if mode == "max_res":
        opts["max_res"] = 1
    elif mode in EXTENSIONS:
        opts["extension"] = mode
    if shots == "burst":
        opts["count"] = min(caps.burst_max, max(2, int(choices.get("burst", DEFAULTS["burst"]))))
    elif shots == "bracket":
        steps = dict((k, ev) for k, _label, ev in BRACKETS).get(choices.get("bracket"), BRACKETS[0][2])
        opts["bracket_ev"] = ",".join(str(ev) for ev in steps)
    elif shots == "focus":
        opts["focus_steps"] = min(caps.burst_max, max(2, int(choices.get("focus_steps", DEFAULTS["focus_steps"]))))
    exposure = pick(choices.get("exposure_s", 0), can["exposures"])
    if exposure:
        opts["exposure_ns"] = int(exposure * 1e9)
    if choices.get("timer") in TIMERS and choices.get("timer"):
        opts["timer_s"] = choices["timer"]
    if choices.get("best") and mode not in EXTENSIONS:
        for key, has in (("nr_hq", caps.noise_reduction_hq), ("edge_hq", caps.sharpening_hq),
                         ("hot_pixel_hq", caps.hot_pixel_hq)):
            if has:
                opts[key] = 1
    if caps.distortion and mode not in EXTENSIONS and choices.get("distortion") in ("on", "off"):
        opts["distortion"] = 1 if choices["distortion"] == "on" else 0
    return opts


def default_folder() -> str:
    pictures = QStandardPaths.writableLocation(QStandardPaths.StandardLocation.PicturesLocation) or str(Path.home())
    return str(Path(pictures) / "Telescope")


def exposure_text(seconds: float) -> str:
    return tr("Like the video") if not seconds else tr("{seconds} s", seconds=f"{seconds:g}")


class PhotosPlugin(TelescopePlugin):
    name = "photos"
    title = N_("Photos")
    about = N_("Takes full-quality photos on the phone, RAW and the phone's Night mode included where it has them.")
    category = "camera"
    icon = "photo"
    when_off = N_("Without it there's no Photos card and no Take photo shortcut.")
    optional = True
    panel_region = "right"

    def setup(self, host, bus):
        self._host, self._bus = host, bus
        self._choices = dict(DEFAULTS)
        self._folder = default_folder()
        self._caps: Optional[StillCaps] = None
        self._busy = False
        self._last_saved: list = []
        self._filling = False  # the combos are being refilled: their signals aren't picks
        bus.phone_state_updated.connect(lambda _s: self._refresh())
        bus.stream_stopped.connect(self._refresh)
        bus.device_changed.connect(lambda _id: self._refresh())
        bus.camera_switched.connect(lambda _cam: self._refresh())  # another lens takes other photos
        bus.still_busy_changed.connect(lambda _id, _busy: self._refresh())

    # ── Card ──────────────────────────────────────────────────────────────

    def create_panel(self) -> QWidget:
        card = create_card()
        lay = card_layout(card)
        add_card_header(lay, tr("Photos"), "camera", hue="camera")
        dim_until_paired(card, self._bus, phone_only=True)

        self._take_btn = action_button(tr("Take photo"), "primary")
        self._take_btn.setToolTip(tr("Take a photo on the phone with the settings below"))
        self._take_btn.clicked.connect(self._take)
        self._open_btn = QPushButton(tr("Open folder"))
        self._open_btn.setToolTip(tr("Show the photos taken last"))
        self._open_btn.clicked.connect(self._open_folder)
        top = QHBoxLayout()
        top.setSpacing(8)
        top.addWidget(self._take_btn)
        top.addStretch(1)
        top.addWidget(self._open_btn)
        lay.addLayout(top)
        self._status = ElidingLabel("", mode=Qt.TextElideMode.ElideMiddle)  # a long file name can't widen the card
        self._status.hide()
        lay.addWidget(self._status)
        self._note = wrapped_note("")
        lay.addWidget(self._note)

        self._format_combo = self._combo("format")
        self._format_row = control_row_widget(tr("Format"), self._format_combo)
        lay.addWidget(self._format_row)

        self._quality = NoScrollSlider(Qt.Orientation.Horizontal)
        self._quality.setRange(50, 100)
        self._quality.set_default(DEFAULTS["quality"])
        self._quality_lbl = value_label()
        self._quality.valueChanged.connect(self._on_quality)
        self._quality_row = control_row_widget(tr("Quality"), slider_row(self._quality, self._quality_lbl), stretch=True)
        lay.addWidget(self._quality_row)

        self._mode_combo = self._combo("mode")
        self._mode_combo.setToolTip(tr("The phone maker's own Night, HDR and Bokeh modes pause the video while they shoot"))
        self._mode_row = control_row_widget(tr("Mode"), self._mode_combo)
        lay.addWidget(self._mode_row)

        self._shots_combo = self._combo("shots")
        self._shots_row = control_row_widget(tr("Shots"), self._shots_combo)
        lay.addWidget(self._shots_row)
        self._burst_spin = NoScrollSpinBox()
        self._burst_spin.valueChanged.connect(lambda v: self._pick("burst", v))
        self._burst_row = control_row_widget(tr("How many"), self._burst_spin)
        lay.addWidget(self._burst_row)
        self._bracket_combo = NoScrollComboBox()
        for key, label, _ev in BRACKETS:
            self._bracket_combo.addItem(tr(label), key)
            self._bracket_combo.setItemData(self._bracket_combo.count() - 1, key, SHORTCUT_VALUE_ROLE)
        self._bracket_combo.currentIndexChanged.connect(
            lambda i: self._pick("bracket", self._bracket_combo.itemData(i)))
        self._bracket_row = control_row_widget(tr("Bracket"), self._bracket_combo)
        lay.addWidget(self._bracket_row)
        self._focus_spin = NoScrollSpinBox()
        self._focus_spin.setToolTip(tr("Photos from the nearest focus to the farthest, for focus stacking"))
        self._focus_spin.valueChanged.connect(lambda v: self._pick("focus_steps", v))
        self._focus_row = control_row_widget(tr("Steps"), self._focus_spin)
        lay.addWidget(self._focus_row)

        self._exposure_combo = self._combo("exposure_s")
        self._exposure_combo.setToolTip(tr("A long exposure keeps the video's ISO and the video stops while the shutter is open"))
        self._exposure_row = control_row_widget(tr("Exposure"), self._exposure_combo)
        lay.addWidget(self._exposure_row)

        self._timer_btns = {}
        group = QButtonGroup(card)
        for seconds in TIMERS:
            btn = SegmentButton(tr("Off") if not seconds else tr("{seconds} s", seconds=seconds), value=str(seconds))
            btn.clicked.connect(lambda _c=False, s=seconds: self._pick("timer", s))
            group.addButton(btn)
            self._timer_btns[seconds] = btn
        lay.addLayout(control_row(tr("Timer"), segmented_row(*self._timer_btns.values()), stretch=True))

        self._best_box = Switch(tr("Best quality noise reduction and sharpening"))
        self._best_box.setToolTip(tr("Takes a little longer and only changes the photo, not the video"))
        self._best_box.toggled.connect(lambda on: self._pick("best", on))
        self._best_row = control_row_widget("", self._best_box, stretch=True)
        lay.addWidget(self._best_row)

        self._distortion_combo = NoScrollComboBox()
        for key, label in DISTORTION:
            self._distortion_combo.addItem(tr(label), key)
        self._distortion_combo.currentIndexChanged.connect(
            lambda i: self._pick("distortion", self._distortion_combo.itemData(i)))
        self._distortion_row = control_row_widget(tr("Distortion fix"), self._distortion_combo)
        lay.addWidget(self._distortion_row)

        self._folder_lbl = ElidingLabel("", mode=Qt.TextElideMode.ElideMiddle)  # the folder's name, its path on hover
        self._folder_btn = QPushButton(tr("Change…"))
        self._folder_btn.setToolTip(tr("Where photos are saved"))
        self._folder_btn.clicked.connect(self._choose_folder)
        folder = QHBoxLayout()
        folder.setSpacing(8)
        folder.addWidget(self._folder_lbl, 1)
        folder.addWidget(self._folder_btn)
        lay.addLayout(control_row(tr("Save in"), folder, stretch=True))

        self._show_choices()
        self._refresh()
        return card

    def _combo(self, key: str) -> NoScrollComboBox:
        combo = NoScrollComboBox()
        combo.currentIndexChanged.connect(lambda i: self._pick(key, combo.itemData(i)))
        return combo

    def create_actions(self) -> list:
        g = tr("Photos")
        return [
            button_action("photos.take", tr("Take photo"), g, self._take_btn),
            choice_action("photos.mode", tr("Photo mode"), g, combo=self._mode_combo),
            choice_action("photos.timer", tr("Photo timer"), g, buttons=list(self._timer_btns.values())),
        ]

    # ── Picks (Qt thread) ─────────────────────────────────────────────────

    def _pick(self, key: str, value):
        if self._filling or value is None or self._choices.get(key) == value:
            return
        self._choices[key] = value
        self._show_choices()
        self._host.schedule_save()

    def _on_quality(self, value: int):
        self._quality_lbl.setText(str(value))
        self._pick("quality", value)

    def _fill(self, combo: NoScrollComboBox, items: list, labels: dict, current):
        combo.clear()
        for value in items:
            text = labels[value] if value in labels else value
            combo.addItem(text, value)
            combo.setItemData(combo.count() - 1, str(value), SHORTCUT_VALUE_ROLE)
        combo.setCurrentIndex(max(0, combo.findData(current)))

    def _show_choices(self):
        """Fill and show the controls for what this lens offers, keeping the picks it can't do for another lens."""
        if not hasattr(self, "_format_combo"):
            return
        caps = self._caps or StillCaps()
        can = offered(caps, self._choices)
        c = self._choices
        mode, fmt, shots = pick(c["mode"], can["modes"]), pick(c["format"], can["formats"]), pick(c["shots"], can["shots"])
        self._filling = True
        try:
            self._fill(self._mode_combo, can["modes"], {k: tr(v) for k, v in MODES}, mode)
            self._fill(self._format_combo, can["formats"], {k: tr(v) for k, v in FORMATS}, fmt)
            self._fill(self._shots_combo, can["shots"], {k: tr(v) for k, v in SHOTS}, shots)
            self._fill(self._exposure_combo, can["exposures"], {s: exposure_text(s) for s in can["exposures"]},
                       pick(c["exposure_s"], can["exposures"]))
            self._quality.setValue(int(c["quality"]))
            self._quality_lbl.setText(str(self._quality.value()))
            self._burst_spin.setRange(2, max(2, caps.burst_max))
            self._burst_spin.setValue(int(c["burst"]))
            self._focus_spin.setRange(2, max(2, min(30, caps.burst_max)))
            self._focus_spin.setValue(int(c["focus_steps"]))
            self._bracket_combo.setCurrentIndex(max(0, self._bracket_combo.findData(c["bracket"])))
            self._timer_btns.get(c["timer"], self._timer_btns[0]).setChecked(True)
            self._best_box.setChecked(bool(c["best"]))
            self._distortion_combo.setCurrentIndex(max(0, self._distortion_combo.findData(c["distortion"])))
        finally:
            self._filling = False
        extension = mode in EXTENSIONS
        self._mode_row.setVisible(len(can["modes"]) > 1)
        self._format_row.setVisible(len(can["formats"]) > 1)
        self._quality_row.setVisible(fmt != "raw")
        self._shots_row.setVisible(len(can["shots"]) > 1)
        self._burst_row.setVisible(shots == "burst")
        self._bracket_row.setVisible(shots == "bracket")
        self._focus_row.setVisible(shots == "focus")
        self._exposure_row.setVisible(len(can["exposures"]) > 1)
        self._best_row.setVisible(not extension and (caps.noise_reduction_hq or caps.sharpening_hq or caps.hot_pixel_hq))
        self._distortion_row.setVisible(not extension and caps.distortion)
        self._folder_lbl.setText(Path(self._folder).name or self._folder)
        self._folder_lbl.setToolTip(self._folder)

    def _refresh(self):
        """Bring the card up to date with the shown stream's lens and whether a photo is being taken."""
        caps = self._host.still_caps()
        if caps != self._caps:
            self._caps = caps
            self._show_choices()
        self._busy = self._host.still_busy()
        if not hasattr(self, "_take_btn"):
            return
        self._take_btn.setEnabled(caps is not None and not self._busy)
        if caps is not None:
            self._note.hide()
        else:
            self._note.setText(tr("Start streaming to take photos.") if not self._host.is_streaming() else
                               tr("This phone can't take photos from Telescope yet. Update the app on the phone."))
            self._note.show()
        self._open_btn.setVisible(bool(self._last_saved))

    # ── Taking one ────────────────────────────────────────────────────────

    def _rotation(self) -> int:
        try:
            return _PHONE_ROTATION.get(self._host.view().rotation, 0)
        except Exception:
            return 0

    def _take(self):
        caps = self._host.still_caps()
        if caps is None or self._host.still_busy():
            return
        options = build_options(caps, self._choices, self._rotation())
        if not self._host.take_still(options, self._folder, self._done, self._progress):
            self._show_status(tr("The phone is already taking a photo."), "status_err")
            return
        self._show_status(tr("Taking the photo…"), "status_dim")
        self._refresh()

    def _progress(self, p: StillProgress):
        if p.state == "waiting" and p.countdown_ms > 0:
            text = tr("Taking the photo in {seconds} s", seconds=max(1, math.ceil(p.countdown_ms / 1000)))
        elif p.shots > 1:
            text = tr("Photo {n} of {total}", n=min(p.shots, p.taken + 1), total=p.shots)
        elif p.paused:
            text = tr("Taking the photo, the video is paused…")
        else:
            text = tr("Taking the photo…")
        self._show_status(text, "status_dim")

    def _done(self, result: StillResult):
        self._last_saved = list(result.files)
        if result.ok and result.files:
            name = Path(result.files[0]).name
            more = len(result.files) - 1
            text = (trn("Saved {name} and {n} more", "Saved {name} and {n} more", more, name=name, n=more) if more
                    else tr("Saved {name}", name=name))
            self._show_status(text, "status_ok")
            if QApplication.activeWindow() is None:  # taken with a global shortcut from another app
                self._host.send_notification(tr("Photo saved"), text, urgent=False)
        else:
            self._show_status(result.error or tr("The phone couldn't take the photo."), "status_err")
        self._refresh()

    def _show_status(self, text: str, kind: str):
        if hasattr(self, "_status"):
            self._status.setText(text)
            self._status.setToolTip(text)
            self._status.setVisible(bool(text))
            set_status_kind(self._status, kind)

    def _open_folder(self):
        folder = str(Path(self._last_saved[0]).parent) if self._last_saved else self._folder
        QDesktopServices.openUrl(QUrl.fromLocalFile(folder))

    def _choose_folder(self):
        path = QFileDialog.getExistingDirectory(self._folder_btn.window(), tr("Save photos in"), self._folder)
        if path:
            self._folder = path
            self._show_choices()
            self._host.schedule_save()

    # ── Config ────────────────────────────────────────────────────────────

    def get_config(self) -> dict:
        return dict(self._choices, folder=self._folder)

    def set_config(self, cfg: dict):
        choices = dict(DEFAULTS)
        for key, default in DEFAULTS.items():
            value = cfg.get(key, default)
            if isinstance(default, bool):
                choices[key] = value if isinstance(value, bool) else default
            elif isinstance(default, int):
                choices[key] = value if isinstance(value, (int, float)) and not isinstance(value, bool) else default
            else:
                choices[key] = value if isinstance(value, str) else default
        if choices["timer"] not in TIMERS:
            choices["timer"] = 0
        self._choices = choices
        folder = cfg.get("folder")
        self._folder = folder if isinstance(folder, str) and folder else default_folder()
        self._show_choices()

    def diagnostics(self) -> dict:
        caps = self._caps
        if caps is None:
            return {"Photos": "not available"}
        extras = [f for f in caps.formats if f != "jpeg"] + (["full resolution"] if caps.max_res else []) + list(caps.extensions)
        sizes = [f"{m} {caps.size_for(m)[0]}x{caps.size_for(m)[1]}" for m in offered(caps, {})["modes"] if caps.size_for(m)]
        return {"Photos": ", ".join(["jpeg"] + extras), "Photo sizes": ", ".join(sizes) or "unknown"}
