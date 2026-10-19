import logging
import threading
from typing import Optional

from PyQt6.QtCore import QSize, Qt, QTimer, QUrl, pyqtSignal
from PyQt6.QtGui import QAction, QDesktopServices, QGuiApplication
from PyQt6.QtWidgets import (
    QComboBox,
    QButtonGroup, QDialog, QFrame, QHBoxLayout, QInputDialog, QLabel, QPushButton, QScrollArea, QVBoxLayout,
    QWidget,
)

from telescope import diagnostics, h264_reader, pyav_vaapi, theme, vcam
from telescope.platform import IS_LINUX, adb_available, adb_devices, adb_install, bundled_apk_path
from telescope.platform.linux import (
    CANCELLED, V4L2_OBS_DEV, V4L2_PHONE_DEV, as_text,
    v4l2_devices_ready, v4l2_load, v4l2_module_loaded, v4l2_unload,
    v4l2_persist_disable, v4l2_persist_enable, v4l2_persist_status,
)
from telescope.platform.windows import (
    UC_NAME, download_unitycapture, register_unitycapture, uc_in_app_folder, uc_registered_name, unitycapture_dir,
    unitycapture_downloaded,
)
from telescope.plugin import TelescopePlugin
from telescope.widgets.adb import AdbDownload
from telescope.version import display_version
from telescope.widgets.controls import setting_row
from telescope.widgets.line_icons import line_icon
from telescope.widgets.common import (
    set_button_icon,
    Switch,
    NoScrollComboBox, NoScrollSpinBox, SegmentButton, action_button, add_card_header, button_row, card_layout,
    create_card, dialog_layout, refresh_hues,
    run_off_ui_thread, segmented_row,
    set_status_kind, set_ui_role, ui_px, wrapped_note,
)

# (width, height) tuples for canvas presets; None = auto from first frame
CANVAS_PRESETS: list[tuple[str, tuple[int, int] | None]] = [
    ("Auto (from first frame)",        None),
    ("1080p 16:9 - 1920 x 1080",      (1920, 1080)),
    ("1080p 16:9 Portrait - 1080 x 1920", (1080, 1920)),
    ("720p 16:9 - 1280 x 720",        (1280,  720)),
    ("720p 16:9 Portrait - 720 x 1280",   ( 720, 1280)),
    ("4K 16:9 - 3840 x 2160",         (3840, 2160)),
    ("4K 16:9 Portrait - 2160 x 3840",    (2160, 3840)),
    ("XGA 4:3 - 1024 x 768",          (1024,  768)),
    ("UXGA 4:3 - 1600 x 1200",        (1600, 1200)),
    ("Custom...",                       "custom"),
]

_PRESET_LABELS = [label for label, _ in CANVAS_PRESETS]
_PRESET_VALUES = {label: val for label, val in CANVAS_PRESETS}

logger = logging.getLogger(__name__)


def canvas_dims(cfg: dict) -> tuple[int | None, int | None]:
    """The canvas a Setup config asks for, or (None, None) for auto."""
    val = _PRESET_VALUES.get(cfg.get("canvas_preset"))
    if val is None:
        return None, None
    if val == "custom":
        return _custom_size(cfg)
    return val


_CUSTOM_MAX = (7680, 4320)


def _even(value: int, top: int) -> int:
    """value rounded up to even, within top: on Linux the camera's I420 frames need an even width and height."""
    return min(value + (value & 1), top - (top & 1))


def _custom_size(cfg: dict) -> tuple[int, int]:
    # A hand-edited value outside the dialog's own ranges would stop the dialog opening, the one place to fix it
    w, h = cfg.get("custom_canvas_w"), cfg.get("custom_canvas_h")
    ok = all(isinstance(v, int) and not isinstance(v, bool) for v in (w, h)) and 64 <= w <= 7680 and 64 <= h <= 4320
    return (_even(w, _CUSTOM_MAX[0]), _even(h, _CUSTOM_MAX[1])) if ok else (1920, 1080)


DEFAULT_MAX_ZOOM = 10
MAX_ZOOM_RANGE = (2, 30)
MAX_GAIN_CHOICES = (12, 24, 36, 48)
_HW_NAMES = {"d3d11va": "Direct3D 11", "cuda": "NVIDIA CUDA", "vaapi": "VA-API", "videotoolbox": "VideoToolbox"}
DEFAULT_MAX_GAIN = 12

_SUDO_HINT = "Asks for your password (pkexec or sudo): this changes system settings."


SCREEN_MARGIN = 48  # px the dialog keeps clear of the screen edges
AUTHOR_URL = "https://github.com/LunarKittyy"


class _FitScroll(QScrollArea):
    """Asks for its content's full height, so the dialog opens as tall as the screen allows and scrolls past that."""

    def __init__(self):
        super().__init__()
        self.setWidgetResizable(True)
        self.setHorizontalScrollBarPolicy(Qt.ScrollBarPolicy.ScrollBarAlwaysOff)
        self.setFrameShape(QFrame.Shape.NoFrame)

    def sizeHint(self) -> QSize:
        inner = self.widget().sizeHint() if self.widget() else QSize()
        return QSize(inner.width() + self.verticalScrollBar().sizeHint().width(), inner.height())


class AdvancedDialog(QDialog):
    """Rarely needed controls. First-run setup lives in the onboarding checklist instead."""

    _sig_v4l_result   = pyqtSignal(bool, str)
    _sig_v4l_unload   = pyqtSignal(bool, str)
    _sig_persist_result = pyqtSignal(bool, str)
    _sig_win_checks   = pyqtSignal(str, bool, bool)  # registered camera name ("" if none), adb found, still in the app folder
    _sig_uc_done      = pyqtSignal(bool, str)
    _sig_uc_msg       = pyqtSignal(str)
    _sig_apk_done     = pyqtSignal(bool, str)

    def __init__(self, parent=None, on_apply_canvas=None, report=None, on_max_zoom=None, on_max_gain=None,
                 on_limiter=None, on_remove_extras=None, extras_in_use=None, on_hw_decode=None,
                 on_plugin_hues=None, on_vaapi=None):
        super().__init__(parent)
        self._on_remove_extras = on_remove_extras
        self._extras_in_use = extras_in_use or (lambda: False)
        self._report = report
        self._uc_installing = False
        self.setWindowTitle("Advanced")
        self.setMinimumWidth(ui_px(560))
        self.setWindowFlag(Qt.WindowType.WindowContextHelpButtonHint, False)
        self._on_apply_canvas = on_apply_canvas
        self._on_max_zoom = on_max_zoom
        self._on_max_gain = on_max_gain
        self._on_limiter = on_limiter
        self._on_hw_decode = on_hw_decode
        self._on_vaapi = on_vaapi
        self._on_plugin_hues = on_plugin_hues
        self._build_ui()
        self._sig_v4l_result.connect(self._on_v4l_result)
        self._sig_v4l_unload.connect(self._on_v4l_unload_result)
        self._sig_persist_result.connect(self._on_persist_result)
        self._sig_win_checks.connect(self._on_win_checks)
        self._sig_uc_done.connect(self._on_uc_done)
        self._sig_uc_msg.connect(lambda msg: self._uc_status_lbl.setText(msg)
                                  if hasattr(self, "_uc_status_lbl") else None)
        self._sig_apk_done.connect(self._on_apk_done)

    def _fit_height(self):
        # Not adjustSize(): Qt caps that at two thirds of the screen
        screen = self.screen() or QGuiApplication.primaryScreen()
        if screen is None:
            self.adjustSize()
            return
        cap = screen.availableGeometry().height() - SCREEN_MARGIN
        self.setMaximumHeight(cap)
        self.resize(max(self.width(), self.sizeHint().width()), min(self.sizeHint().height(), cap))

    def showEvent(self, event):
        self._refresh_extras()
        self._fit_height()
        super().showEvent(event)
        if IS_LINUX:
            self._v4l_check()
            self._refresh_persist_status()
        else:
            threading.Thread(target=self._check_win_setup, daemon=True).start()

    def _build_ui(self):
        # The bottom row is a bar across the whole dialog, so the dialog's own layout has no margins
        outer = QVBoxLayout(self)
        outer.setContentsMargins(0, 0, 0, 0)
        outer.setSpacing(0)
        page = QWidget()
        outer.addWidget(page, 1)
        lay = dialog_layout(page)
        lay.setContentsMargins(20, 18, 20, 14)
        title_row = QHBoxLayout()
        title = QLabel("Advanced")
        title.setObjectName("dialog_title")
        title_row.addWidget(title)
        title_row.addStretch(1)
        version = QLabel(f"Telescope {display_version()}")
        version.setObjectName("version_note")
        title_row.addWidget(version)
        lay.addLayout(title_row)
        # The cards scroll; the title and the bottom row stay put
        scroll = _FitScroll()
        content = QWidget()
        body = QVBoxLayout(content)
        body.setContentsMargins(0, 0, 6, 0)  # room for the scrollbar beside the cards
        body.setSpacing(lay.spacing())
        scroll.setWidget(content)
        lay.addWidget(scroll, 1)

        # ── Virtual camera ────────────────────────────────────────────────────
        vc_card = create_card()
        vc_lay = card_layout(vc_card)
        add_card_header(vc_lay, "Virtual camera", "video", hue="stream")
        if IS_LINUX:
            chk_btn    = QPushButton("Check")
            load_btn   = QPushButton("Load")
            unload_btn = QPushButton("Unload")
            load_btn.setToolTip(_SUDO_HINT)
            unload_btn.setToolTip(_SUDO_HINT)
            chk_btn.clicked.connect(self._v4l_check)
            load_btn.clicked.connect(self._v4l_load)
            unload_btn.clicked.connect(self._v4l_unload)
            strip, self._v4l_lbl = setting_row("Kernel module", "Checking...", button_row(chk_btn, load_btn, unload_btn))
            self._v4l_lbl.setTextInteractionFlags(Qt.TextInteractionFlag.TextSelectableByMouse)
            set_status_kind(self._v4l_lbl, "status_dim")
            strip.setToolTip("Already says Ready? Leave these alone; they're only for fixing a \"not ready\" status.")
            vc_lay.addWidget(strip)

            self._persist_chk = Switch()
            self._persist_chk.setToolTip(
                "Also writes it to /etc/modprobe.d/ and /etc/modules-load.d/, so it survives a reboot.\n" + _SUDO_HINT
            )
            self._persist_chk.toggled.connect(self._on_persist_toggled)
            strip, self._persist_status_lbl = setting_row("Load when the computer starts", "", self._persist_chk)
            self._persist_status_lbl.setTextInteractionFlags(Qt.TextInteractionFlag.TextSelectableByMouse)
            set_status_kind(self._persist_status_lbl, "status_dim")
            self._persist_row = self._persist_status_lbl
            self._persist_row.setVisible(False)
            vc_lay.addWidget(strip)
        else:
            self._uc_btn = action_button("Install driver")
            self._uc_btn.clicked.connect(self._install_uc)
            strip, self._uc_status_lbl = setting_row("Driver", "Checking...", self._uc_btn)
            set_status_kind(self._uc_status_lbl, "status_dim")
            vc_lay.addWidget(strip)

            # Not bundled (Google's license doesn't allow passing it on), so it's fetched from Google when asked
            self._adb_btn = action_button("Get adb", "primary")
            self._adb_from_google = True
            self._adb_btn.setToolTip("Downloads adb from Google, for pairing and installing over USB")
            self._adb_btn.clicked.connect(self._get_adb)
            # Google blocks the download in some countries, so one got another way can be picked instead
            self._adb_pick_btn = QPushButton("Use my own adb…")
            self._adb_pick_btn.setToolTip("Pick an adb.exe or platform-tools zip you already have")
            self._adb_pick_btn.clicked.connect(self._pick_adb)
            self._adb_row = QWidget()
            adb_buttons = QHBoxLayout(self._adb_row)
            adb_buttons.setContentsMargins(0, 0, 0, 0)
            adb_buttons.setSpacing(8)
            adb_buttons.addWidget(self._adb_btn)
            adb_buttons.addWidget(self._adb_pick_btn)
            self._adb_row.setVisible(False)
            strip, self._adb_status_lbl = setting_row("ADB", "Checking...", self._adb_row)
            set_status_kind(self._adb_status_lbl, "status_dim")
            self._adb_status_lbl.setTextFormat(Qt.TextFormat.PlainText)  # errors can hold <urlopen error ...>
            vc_lay.addWidget(strip)
            self._adb_download = AdbDownload(self)
            self._adb_download.progress.connect(self._adb_status_lbl.setText)
            self._adb_download.finished.connect(self._on_adb_downloaded)

        # The ones Add camera set up, for streaming several cameras at once
        self._extras_btn = QPushButton("Remove")
        self._extras_btn.clicked.connect(self._remove_extras)
        self._extras_row, self._extras_lbl = setting_row("Extra cameras", "", self._extras_btn)
        self._extras_row.setVisible(False)
        vc_lay.addWidget(self._extras_row)
        body.addWidget(vc_card)

        # ── Video ─────────────────────────────────────────────────────────────
        video_card = create_card()
        self._advanced_content = video_card
        video_lay = card_layout(video_card)
        add_card_header(video_lay, "Video", "chip", hue="stream")
        device = h264_reader.hardware_device()
        self._hw_chk = Switch()
        self._hw_chk.setEnabled(device is not None)
        self._hw_chk.setToolTip("Decodes the Light format on the graphics card from the next stream on.")
        self._hw_chk.toggled.connect(lambda on: self._on_hw_decode(on) if self._on_hw_decode else None)
        # The line under it names the graphics card it would use, so one strip says whether it's on and what with
        # On Linux without a card PyAV can use, VA-API can be set up for AMD and Intel ones (pyav_vaapi)
        self._vaapi_btn = QPushButton("Set up VA-API…")
        self._vaapi_btn.clicked.connect(lambda: self._on_vaapi() if self._on_vaapi else None)
        self._vaapi_btn.setVisible(pyav_vaapi.supported() and (device is None or pyav_vaapi.chosen() is not None
                                                                or bool(pyav_vaapi.problem)))
        hw_controls = QHBoxLayout()
        hw_controls.setContentsMargins(0, 0, 0, 0)
        hw_controls.setSpacing(10)
        hw_controls.addWidget(self._vaapi_btn)
        hw_controls.addWidget(self._hw_chk)
        strip, self._hw_line = setting_row(
            "Hardware acceleration",
            f"Found {_HW_NAMES.get(device, device)}" if device else "No graphics card Telescope can use", hw_controls)
        strip.setToolTip(self._hw_chk.toolTip())
        video_lay.addWidget(strip)
        self._max_zoom_spin = NoScrollSpinBox()
        self._max_zoom_spin.setObjectName("chip_spin")
        self._max_zoom_spin.setFixedSize(ui_px(80), ui_px(30))
        self._max_zoom_spin.setRange(*MAX_ZOOM_RANGE)
        self._max_zoom_spin.setValue(DEFAULT_MAX_ZOOM)
        self._max_zoom_spin.setSuffix("×")
        self._max_zoom_spin.valueChanged.connect(
            lambda value: self._on_max_zoom(value) if self._on_max_zoom else None)
        strip, _ = setting_row("Max zoom", "", self._max_zoom_spin)
        strip.setToolTip("How far the Zoom slider goes. Past what the phone can zoom itself, the rest is cropped on "
                         "this computer.")
        video_lay.addWidget(strip)

        self._canvas_combo = NoScrollComboBox()
        self._canvas_combo.addItems(_PRESET_LABELS)
        self._canvas_combo.setSizeAdjustPolicy(QComboBox.SizeAdjustPolicy.AdjustToMinimumContentsLengthWithIcon)
        self._canvas_combo.setMinimumContentsLength(18)  # the row fits the dialog; the list opens as wide as it needs
        self._canvas_combo.currentTextChanged.connect(self._on_preset_changed)

        # Custom W x H spinboxes (hidden unless "Custom..." selected)
        self._custom_w = NoScrollSpinBox()
        self._custom_w.setRange(64, _CUSTOM_MAX[0])
        self._custom_w.setSingleStep(2)  # odd sizes break the colours on Linux
        self._custom_w.setValue(1920)
        self._custom_w.setSuffix(" px")
        self._custom_h = NoScrollSpinBox()
        self._custom_h.setRange(64, _CUSTOM_MAX[1])
        self._custom_h.setSingleStep(2)
        self._custom_h.setValue(1080)
        self._custom_h.setSuffix(" px")
        self._custom_widget = QWidget()
        size_lay = QHBoxLayout(self._custom_widget)
        size_lay.setContentsMargins(0, 0, 0, 0)
        size_lay.setSpacing(8)
        size_lay.addWidget(self._custom_w, 1)
        times = QLabel("×")
        times.setObjectName("dim")
        size_lay.addWidget(times)
        size_lay.addWidget(self._custom_h, 1)
        if IS_LINUX:
            canvas_note = ("Applying stops the stream, unloads v4l2loopback and loads it again. "
                           "Close OBS and anything else using the virtual camera first.")
            apply_label, apply_tooltip = "Apply and reload", _SUDO_HINT
        else:
            canvas_note = "Applying restarts the stream at the new size. If OBS loses the source, remove it and add it again."
            apply_label, apply_tooltip = "Apply", None
        self._canvas_apply_btn = action_button(apply_label, tooltip="\n".join(t for t in (canvas_note, apply_tooltip) if t))
        self._canvas_apply_btn.clicked.connect(self._apply_canvas)
        self._custom_widget.setVisible(False)
        self._canvas_status_lbl = wrapped_note("")
        set_status_kind(self._canvas_status_lbl, "status_dim")
        self._canvas_status_row = self._canvas_status_lbl
        self._canvas_status_row.setVisible(False)
        under = QVBoxLayout()
        under.setContentsMargins(0, 0, 0, 0)
        under.setSpacing(6)
        under.addWidget(self._custom_widget)
        apply_row = QHBoxLayout()
        apply_row.setContentsMargins(0, 0, 0, 0)
        apply_row.setSpacing(8)
        apply_row.addWidget(self._canvas_status_lbl, 1)
        apply_row.addWidget(self._canvas_apply_btn, 0, Qt.AlignmentFlag.AlignRight | Qt.AlignmentFlag.AlignTop)
        under.addLayout(apply_row)
        strip, _ = setting_row("Canvas size", "", self._canvas_combo, under)
        video_lay.addWidget(strip)
        body.addWidget(video_card)

        # ── Microphone ────────────────────────────────────────────────────────
        mic_card = create_card()
        mic_lay = card_layout(mic_card)
        add_card_header(mic_lay, "Microphone", "mic", hue="mic")
        self._max_gain_btns = {db: SegmentButton(f"+{db} dB") for db in MAX_GAIN_CHOICES}
        self._max_gain_grp = QButtonGroup(mic_card)
        self._max_gain_grp.setExclusive(True)
        for db, btn in self._max_gain_btns.items():
            self._max_gain_grp.addButton(btn, db)
        self._max_gain_btns[DEFAULT_MAX_GAIN].setChecked(True)
        self._max_gain_grp.idClicked.connect(
            lambda db: self._on_max_gain(db) if self._on_max_gain else None)
        strip, _ = setting_row("Max gain", "", None, segmented_row(*self._max_gain_btns.values()))
        strip.setToolTip("How far the Gain slider goes. Less range gives finer steps; more helps a quiet phone.")
        mic_lay.addWidget(strip)
        self._limiter_chk = Switch()
        self._limiter_chk.setChecked(True)
        self._limiter_chk.toggled.connect(lambda on: self._on_limiter(on) if self._on_limiter else None)
        strip, _ = setting_row("Limiter", "Keeps loud sounds from clipping", self._limiter_chk)
        mic_lay.addWidget(strip)
        body.addWidget(mic_card)

        # ── Phone app ─────────────────────────────────────────────────────────
        apk_card = create_card()
        apk_lay = card_layout(apk_card)
        add_card_header(apk_lay, "Phone app", "phone", hue="phone")
        _apk = bundled_apk_path()
        self._apk_btn = action_button("Install over USB" if _apk else "Choose APK...")
        set_button_icon(self._apk_btn, line_icon("download", theme.TEXT_DIM))
        self._apk_btn.setToolTip("Installs the Telescope.apk that came with this app, or one you pick, on a phone "
                                 "plugged in over USB.")
        self._apk_btn.clicked.connect(self._install_apk)
        strip, self._apk_status_lbl = setting_row(
            "Telescope.apk", "Comes with this app" if _apk else "None came with this app", self._apk_btn)
        set_status_kind(self._apk_status_lbl, "status_dim")
        apk_lay.addWidget(strip)
        body.addWidget(apk_card)

        # ── Look ────────────────────────────────────────────────────────────
        look_card = create_card()
        look_lay = card_layout(look_card)
        add_card_header(look_lay, "Look", "sliders")
        self._hues_own, self._hues_purple = SegmentButton("Each its own"), SegmentButton("All purple")
        self._hues_grp = QButtonGroup(look_card)
        self._hues_grp.setExclusive(True)
        self._hues_grp.addButton(self._hues_own)
        self._hues_grp.addButton(self._hues_purple)
        self._hues_own.setChecked(True)
        self._hues_own.setToolTip("Every card in the main window gets its own colour.")
        self._hues_grp.buttonClicked.connect(
            lambda btn: self._on_plugin_hues(btn is self._hues_own) if self._on_plugin_hues else None)
        strip, _ = setting_row("Card colours", "", None, segmented_row(self._hues_own, self._hues_purple))
        look_lay.addWidget(strip)
        body.addWidget(look_card)
        body.addStretch(1)

        by_lbl = QLabel(f'by <a href="{AUTHOR_URL}">LunarKittyy</a>')
        by_lbl.setObjectName("version_note")
        by_lbl.setTextFormat(Qt.TextFormat.RichText)
        by_lbl.setTextInteractionFlags(Qt.TextInteractionFlag.TextBrowserInteraction)
        by_lbl.setOpenExternalLinks(True)
        self._diag_btn = QPushButton("Copy diagnostics")
        set_button_icon(self._diag_btn, line_icon("copy", theme.TEXT_DIM))
        self._diag_btn.setToolTip("Copies info for a bug report, leaving out pairing tokens, addresses and file paths.")
        self._diag_btn.clicked.connect(self._copy_diagnostics)
        self._diag_btn.setVisible(self._report is not None)
        self._log_btn = QPushButton("Open log")
        self._log_btn.setToolTip("The full log, in the temp folder (the system clears it).")
        self._log_btn.clicked.connect(self._open_log)
        self._log_btn.setVisible(self._report is not None and diagnostics.events.path is not None)
        close_btn = QPushButton("Close")
        set_ui_role(close_btn, "primary")
        close_btn.clicked.connect(self.accept)
        bar = QFrame()
        bar.setObjectName("dialog_footer")
        bottom = QHBoxLayout(bar)
        bottom.setContentsMargins(20, 10, 20, 10)
        bottom.setSpacing(8)
        bottom.addWidget(by_lbl)
        bottom.addStretch(1)
        bottom.addWidget(self._log_btn)
        bottom.addWidget(self._diag_btn)
        bottom.addWidget(close_btn)
        outer.addWidget(bar)

    def _copy_diagnostics(self):
        QGuiApplication.clipboard().setText(self._report())
        self._diag_btn.setText("Copied")
        # A timer owned by the button dies with it, unlike a bare singleShot lambda, which would touch a deleted button
        reset = QTimer(self._diag_btn)
        reset.setSingleShot(True)
        reset.timeout.connect(lambda: (self._diag_btn.setText("Copy diagnostics"), reset.deleteLater()))
        reset.start(2000)

    def _open_log(self):
        if diagnostics.events.path is not None:
            QDesktopServices.openUrl(QUrl.fromLocalFile(str(diagnostics.events.path)))

    def _on_preset_changed(self, label: str):
        self._custom_widget.setVisible(label == "Custom...")

    def _apply_canvas(self):
        w, h = self._get_selected_dims()
        self._canvas_apply_btn.setEnabled(False)
        set_status_kind(self._canvas_status_lbl, "status_dim")
        self._canvas_status_lbl.setText("Reloading loopback..." if IS_LINUX else "Restarting stream...")
        self._canvas_status_row.setVisible(True)
        if self._on_apply_canvas:
            self._on_apply_canvas(w, h)

    def set_canvas_apply_result(self, ok: bool, msg: str):
        """Called from SetupPlugin once the reload completes."""
        self._canvas_apply_btn.setEnabled(True)
        if ok:
            set_status_kind(self._canvas_status_lbl, "status_ok")
            self._canvas_status_lbl.setText(
                "Done - loopback reloaded." if IS_LINUX else "Done - canvas updated.")
        else:
            set_status_kind(self._canvas_status_lbl, "status_err")
            if "in use" in msg.lower():
                self._canvas_status_lbl.setText(
                    "Failed: module is still in use. Close OBS and any other app "
                    "using the virtual camera, then try again."
                )
            else:
                self._canvas_status_lbl.setText(f"Failed: {msg}")
        self._canvas_status_row.setVisible(True)

    def _get_selected_dims(self) -> tuple[int | None, int | None]:
        label = self._canvas_combo.currentText()
        val = _PRESET_VALUES.get(label)
        if val is None:
            return None, None
        if val == "custom":
            # A typed odd size goes up to the next even one, and the boxes show what's used
            self._custom_w.setValue(_even(self._custom_w.value(), _CUSTOM_MAX[0]))
            self._custom_h.setValue(_even(self._custom_h.value(), _CUSTOM_MAX[1]))
            return self._custom_w.value(), self._custom_h.value()
        return val  # (w, h)

    # ── called by SetupPlugin to sync combo to stored config ─────────────────

    def set_canvas_preset(self, label: str, custom_w: int = 1920, custom_h: int = 1080):
        idx = self._canvas_combo.findText(label)
        if idx >= 0:
            self._canvas_combo.setCurrentIndex(idx)
        self._custom_w.setValue(custom_w)
        self._custom_h.setValue(custom_h)

    def get_canvas_preset_label(self) -> str:
        return self._canvas_combo.currentText()

    def set_max_gain(self, value: int):
        self._max_gain_btns[value].setChecked(True)

    def set_hw_decode(self, on: bool):
        self._hw_chk.blockSignals(True)  # syncing the stored value isn't a change
        self._hw_chk.setChecked(on)
        self._hw_chk.blockSignals(False)

    def set_plugin_hues(self, on: bool):
        (self._hues_own if on else self._hues_purple).setChecked(True)

    def set_limiter(self, on: bool):
        self._limiter_chk.blockSignals(True)  # syncing the stored value isn't a change
        self._limiter_chk.setChecked(on)
        self._limiter_chk.blockSignals(False)

    def set_max_zoom(self, value: int):
        self._max_zoom_spin.blockSignals(True)  # syncing the stored value isn't a change
        self._max_zoom_spin.setValue(value)
        self._max_zoom_spin.blockSignals(False)

    # ── v4l2 ─────────────────────────────────────────────────────────────────

    def _v4l_check(self):
        if v4l2_devices_ready():
            set_status_kind(self._v4l_lbl, "status_ok")
            self._v4l_lbl.setText(f"Ready: {V4L2_PHONE_DEV} + {V4L2_OBS_DEV}")
        elif v4l2_module_loaded():
            set_status_kind(self._v4l_lbl, "status_warn")
            self._v4l_lbl.setText(f"Module loaded but {V4L2_PHONE_DEV} not found - another config active")
        else:
            set_status_kind(self._v4l_lbl, "status_err")
            self._v4l_lbl.setText("Not loaded - click Load")

    def _v4l_load(self):
        set_status_kind(self._v4l_lbl, "status_dim")
        self._v4l_lbl.setText("Loading...")
        threading.Thread(target=lambda: self._sig_v4l_result.emit(*as_text(v4l2_load())), daemon=True).start()

    def _v4l_unload(self):
        set_status_kind(self._v4l_lbl, "status_dim")
        self._v4l_lbl.setText("Unloading...")
        def unload():
            with vcam.device_released():
                result = v4l2_unload()
            self._sig_v4l_unload.emit(*as_text(result))
        threading.Thread(target=unload, daemon=True).start()

    def _on_v4l_result(self, ok: bool, msg: str):
        self._v4l_lbl.setText(("Loaded - " if ok else "Failed - ") + msg)
        set_status_kind(self._v4l_lbl, "status_ok" if ok else "status_err")

    def _on_v4l_unload_result(self, ok: bool, msg: str):
        self._v4l_lbl.setText(("Unloaded - " if ok else "Failed - ") + msg)
        set_status_kind(self._v4l_lbl, "status_ok" if ok else "status_err")

    # ── v4l2 persistence ─────────────────────────────────────────────────────

    def _refresh_persist_status(self):
        status = v4l2_persist_status()
        persisted = status["modprobe_conf"] or status["modules_load_conf"]
        self._persist_chk.blockSignals(True)
        self._persist_chk.setChecked(persisted)
        self._persist_chk.blockSignals(False)

    def _on_persist_toggled(self, checked: bool):
        self._persist_chk.setEnabled(False)
        set_status_kind(self._persist_status_lbl, "status_dim")
        self._persist_status_lbl.setText("Working...")
        self._persist_row.setVisible(True)
        self._fit_height()
        action = v4l2_persist_enable if checked else v4l2_persist_disable
        threading.Thread(target=lambda: self._sig_persist_result.emit(*as_text(action())), daemon=True).start()

    def _on_persist_result(self, ok: bool, msg: str):
        self._persist_chk.setEnabled(True)
        if not ok:
            # Revert the checkbox without re-triggering the write/remove action.
            self._persist_chk.blockSignals(True)
            self._persist_chk.setChecked(not self._persist_chk.isChecked())
            self._persist_chk.blockSignals(False)
        set_status_kind(self._persist_status_lbl, "status_ok" if ok else "status_err")
        self._persist_status_lbl.setText(msg)
        self._persist_row.setVisible(True)
        self._fit_height()

    # ── Extra cameras ─────────────────────────────────────────────────────────

    def _refresh_extras(self, text: Optional[str] = None, kind: str = "status_dim"):
        extras = [n for n in range(1, vcam.MAX_SLOTS) if vcam.slot_ready(n)]
        busy = self._extras_in_use()
        if text is None:
            text = ", ".join(vcam.slot_label(n) for n in extras)
        set_status_kind(self._extras_lbl, kind)
        self._extras_lbl.setText(text)
        self._extras_row.setVisible(bool(extras) or kind != "status_dim")
        self._extras_btn.setVisible(bool(extras))
        self._extras_btn.setEnabled(bool(extras) and not busy and self._on_remove_extras is not None)
        self._extras_btn.setToolTip(
            "Stop the streams going to them first" if busy else
            "Back to just the one camera; Add camera sets them up again. "
            + ("Asks for your password." if IS_LINUX else "Windows asks for permission."))

    def _remove_extras(self):
        self._extras_btn.setEnabled(False)
        self._extras_lbl.setText("Removing…")
        self._on_remove_extras(self._on_extras_removed)

    def _on_extras_removed(self, ok: bool, msg: str):
        if ok:
            self._refresh_extras("Removed", "status_ok")
        elif msg == CANCELLED:
            self._refresh_extras()
        else:
            self._refresh_extras(f"Couldn't remove them: {msg}", "status_err")

    # ── Windows ───────────────────────────────────────────────────────────────

    def _check_win_setup(self):
        self._sig_win_checks.emit(uc_registered_name() or "", adb_available(), uc_in_app_folder())

    def _on_win_checks(self, uc_name: str, adb_ok: bool, in_app_folder: bool = False):
        self._uc_btn.setEnabled(not self._uc_installing)
        if uc_name and in_app_folder:
            # Registered by an older Telescope from the app's own folder, which any program running as this user can write to.
            set_status_kind(self._uc_status_lbl, "status_warn")
            self._uc_status_lbl.setText("Reinstall for a security fix")
            self._uc_btn.setText("Reinstall")
            self._uc_btn.setToolTip("Moves the driver into Program Files (Windows asks for admin access once).")
            set_ui_role(self._uc_btn, "primary")
        elif uc_name == UC_NAME:
            set_status_kind(self._uc_status_lbl, "status_ok")
            self._uc_status_lbl.setText("Ready")
            self._uc_btn.setText("Reinstall")
            self._uc_btn.setToolTip("")
            set_ui_role(self._uc_btn, "")
        elif uc_name:
            # Registered by an older Telescope under UnityCapture's own name. Works, just harder to find.
            set_status_kind(self._uc_status_lbl, "status_warn")
            self._uc_status_lbl.setText(f"Apps list it as \u201c{uc_name}\u201d")
            self._uc_btn.setText("Rename")
            self._uc_btn.setToolTip(f"Register it again as \u201c{UC_NAME}\u201d")
            set_ui_role(self._uc_btn, "primary")
        else:
            set_status_kind(self._uc_status_lbl, "status_err")
            dlls = unitycapture_downloaded(unitycapture_dir())
            self._uc_status_lbl.setText(
                "Not installed" if dlls else "Not installed (Install downloads the driver first)")
            self._uc_btn.setText("Install driver")
            self._uc_btn.setToolTip("")
            set_ui_role(self._uc_btn, "primary")
        if self._adb_download.running:
            return
        self._adb_row.setVisible(not adb_ok)
        if adb_ok:
            set_status_kind(self._adb_status_lbl, "status_ok")
            self._adb_status_lbl.setText("Ready")
        else:
            set_status_kind(self._adb_status_lbl, "status_err")
            self._adb_status_lbl.setText("Not found. Pairing and installing over USB need it; Wi-Fi doesn't.")

    def _adb_starting(self):
        self._adb_btn.setEnabled(False)
        self._adb_pick_btn.setEnabled(False)
        set_status_kind(self._adb_status_lbl, "status_dim")

    def _get_adb(self):
        self._adb_from_google = True
        self._adb_download.ask_and_start(self, self._adb_starting)

    def _pick_adb(self):
        self._adb_from_google = False
        self._adb_download.pick_and_start(self, self._adb_starting)

    def _on_adb_downloaded(self, ok: bool, detail: str):
        self._adb_btn.setEnabled(True)
        self._adb_pick_btn.setEnabled(True)
        if ok:
            self._adb_row.setVisible(False)
            set_status_kind(self._adb_status_lbl, "status_ok")
            where = ", from Google" if self._adb_from_google else ""
            self._adb_status_lbl.setText(f"Ready (adb {detail}{where})" if detail else f"Ready (adb{where})")
        else:
            self._adb_btn.setText("Try again")
            set_status_kind(self._adb_status_lbl, "status_err")
            self._adb_status_lbl.setText(detail)

    def _install_uc(self):
        self._uc_installing = True
        self._uc_btn.setEnabled(False)
        set_status_kind(self._uc_status_lbl, "status_dim")

        def worker():
            try:
                if not unitycapture_downloaded(unitycapture_dir()):
                    self._sig_uc_msg.emit("Downloading driver files...")
                    ok, msg = download_unitycapture()
                    if not ok:
                        self._sig_uc_done.emit(False, msg)
                        return
                self._sig_uc_msg.emit("Registering (admin access required)...")
                ok, msg = register_unitycapture()
            except Exception as exc:  # always report back, or the button stays off until a restart
                logger.exception("Driver install failed")
                ok, msg = False, str(exc)
            self._sig_uc_done.emit(ok, msg)

        threading.Thread(target=worker, daemon=True).start()

    def _on_uc_done(self, ok: bool, msg: str):
        self._uc_installing = False
        self._uc_btn.setEnabled(True)
        if ok:
            set_status_kind(self._uc_status_lbl, "status_ok")
            self._uc_status_lbl.setText("Ready")
            self._uc_btn.setText("Reinstall")
            self._uc_btn.setToolTip("")
        else:
            set_status_kind(self._uc_status_lbl, "status_err")
            self._uc_status_lbl.setText(f"Failed: {msg}")
            self._uc_btn.setText("Retry")

    # ── APK ───────────────────────────────────────────────────────────────────

    def _install_apk(self):
        if not adb_available():
            set_status_kind(self._apk_status_lbl, "status_err")
            self._apk_status_lbl.setText("Installing over USB needs adb. Click Get adb under Virtual camera above."
                                         if not IS_LINUX else
                                         "Installing over USB needs adb. Install it from your distro's packages.")
            return

        apk = bundled_apk_path()
        if apk is None:
            from PyQt6.QtWidgets import QFileDialog
            chosen, _ = QFileDialog.getOpenFileName(self, "Select APK", "", "Android Package (*.apk)")
            if not chosen:
                return
            path = chosen
        else:
            path = str(apk)

        serials = run_off_ui_thread(adb_devices)
        if not serials:
            set_status_kind(self._apk_status_lbl, "status_err")
            self._apk_status_lbl.setText("No phone found over USB. Plug it in and allow USB debugging when the phone asks.")
            return
        serial = serials[0]
        if len(serials) > 1:
            serial, ok = QInputDialog.getItem(
                self, "Select device",
                "More than one phone is plugged in. Which one should get the app?",
                serials, 0, False,
            )
            if not ok:
                return

        self._apk_btn.setEnabled(False)
        set_status_kind(self._apk_status_lbl, "status_dim")
        self._apk_status_lbl.setText("Installing...")

        def worker():
            ok, detail = adb_install(serial, path)
            self._sig_apk_done.emit(ok, "Installed" if ok else detail)

        threading.Thread(target=worker, daemon=True).start()

    def _on_apk_done(self, ok: bool, msg: str):
        self._apk_btn.setEnabled(True)
        set_status_kind(self._apk_status_lbl, "status_ok" if ok else "status_err")
        self._apk_status_lbl.setText(msg)


class SetupPlugin(TelescopePlugin):
    name = "setup"
    title = "Advanced settings"
    about = "The Advanced dialog with the virtual camera, its size, extra cameras and other settings you rarely change."
    required = True

    def setup(self, host, bus):
        self._host = host
        self._bus = bus
        self._dlg: Optional[AdvancedDialog] = None
        self._max_zoom = DEFAULT_MAX_ZOOM
        self._max_gain = DEFAULT_MAX_GAIN
        self._limiter = True
        self._hw_decode = False
        self._plugin_hues = True
        self._canvas_preset = "Auto (from first frame)"
        self._custom_w = 1920
        self._custom_h = 1080

    def get_canvas_dims(self) -> tuple[int | None, int | None]:
        """Return (canvas_w, canvas_h) for StreamWorker, or (None, None) for auto."""
        return canvas_dims(self.get_config())

    def create_panel(self) -> Optional[QWidget]:
        """No panel; setup is dialog-only (see create_menu_actions)."""
        return None

    def create_menu_actions(self) -> list:
        action = QAction("Advanced…", None)
        action.triggered.connect(self._open)
        return [action]

    def _open(self):
        if self._dlg is None:
            self._dlg = AdvancedDialog(self._host, on_apply_canvas=self._on_apply_canvas,
                                       report=self._host.diagnostics_report,
                                       on_max_zoom=self._on_max_zoom, on_max_gain=self._on_max_gain,
                                       on_limiter=self._on_limiter, on_plugin_hues=self._on_plugin_hues,
                                       on_remove_extras=lambda done: self._host.remove_extra_cameras(done),
                                       extras_in_use=lambda: self._host.stream_count() > 1,
                                       on_hw_decode=self._on_hw_decode, on_vaapi=self._open_vaapi)
        self._dlg.set_canvas_preset(self._canvas_preset, self._custom_w, self._custom_h)
        self._dlg.set_max_zoom(self._max_zoom)
        self._dlg.set_max_gain(self._max_gain)
        self._dlg.set_limiter(self._limiter)
        self._dlg.set_hw_decode(self._hw_decode)
        self._dlg.set_plugin_hues(self._plugin_hues)
        self._dlg.show()
        self._dlg.raise_()
        self._dlg.activateWindow()

    def _on_apply_canvas(self, w: int | None, h: int | None):
        # In effect for the restart, but kept only once the host accepts it
        before = (self._canvas_preset, self._custom_w, self._custom_h)
        if self._dlg:
            self._canvas_preset = self._dlg.get_canvas_preset_label()
            if self._canvas_preset == "Custom...":
                self._custom_w = w
                self._custom_h = h

        def on_done(ok: bool, msg: str):
            if not ok:
                self._canvas_preset, self._custom_w, self._custom_h = before
            self._host.schedule_save()
            if self._dlg:
                self._dlg.set_canvas_apply_result(ok, msg)
        self._host.restart_vcam_canvas(w, h, on_done=on_done)

    def _on_max_zoom(self, value: int):
        self._max_zoom = value
        self._bus.max_zoom_changed.emit(value)
        self._host.schedule_save()

    def _on_max_gain(self, value: int):
        self._max_gain = value
        self._bus.max_gain_changed.emit(value)
        self._host.schedule_save()

    def _open_vaapi(self):
        from telescope.widgets.vaapi_setup import VaapiSetupDialog
        VaapiSetupDialog(self._dlg, self._host.restart_app).exec()

    def _on_hw_decode(self, on: bool):
        self._hw_decode = on
        h264_reader.set_hardware(on)
        self._host.schedule_save()

    def _on_plugin_hues(self, on: bool):
        self._plugin_hues = on
        theme.set_plugin_hues(on)
        refresh_hues()
        self._host.schedule_save()

    def _on_limiter(self, on: bool):
        self._limiter = on
        self._bus.limiter_changed.emit(on)
        self._host.schedule_save()

    def diagnostics(self) -> dict:
        return {**self._decoding(), **self._pyav()}

    def _pyav(self) -> dict:
        if pyav_vaapi.active:
            which = "distro's" if pyav_vaapi.active["kind"] == pyav_vaapi.SYSTEM else "built here"
            return {"PyAV": f"{pyav_vaapi.active['version']} ({which}, for VA-API)"}
        if pyav_vaapi.problem:
            return {"PyAV": f"standard (the VA-API one isn't used: {pyav_vaapi.problem})"}
        return {}

    def _decoding(self) -> dict:
        active = h264_reader.decoding_backends()
        chosen = "graphics card" if self._hw_decode else "processor"
        if not active:
            return {"Decoding": f"not streaming ({chosen} selected)"}
        text = ", ".join("processor" if b == "processor" else f"graphics card ({b})" for b in active)
        if self._hw_decode != any(b != "processor" for b in active):
            text += (" (graphics card failed; processor until restart)" if self._hw_decode and h264_reader.hardware_failed()
                     else f" ({chosen} selected for the next connection)")
        return {"Decoding": text}

    def get_config(self) -> dict:
        return {
            "canvas_preset":   self._canvas_preset,
            "custom_canvas_w": self._custom_w,
            "custom_canvas_h": self._custom_h,
            "max_zoom":        self._max_zoom,
            "max_gain":        self._max_gain,
            "limiter":         self._limiter,
            "hw_decode":       self._hw_decode,
            "plugin_hues":     self._plugin_hues,
        }

    def set_config(self, cfg: dict):
        self._canvas_preset = cfg.get("canvas_preset", "Auto (from first frame)")
        self._custom_w, self._custom_h = _custom_size(cfg)
        max_zoom = cfg.get("max_zoom", DEFAULT_MAX_ZOOM)
        valid = isinstance(max_zoom, int) and not isinstance(max_zoom, bool) and \
            MAX_ZOOM_RANGE[0] <= max_zoom <= MAX_ZOOM_RANGE[1]
        self._max_zoom = max_zoom if valid else DEFAULT_MAX_ZOOM
        self._bus.max_zoom_changed.emit(self._max_zoom)
        max_gain = cfg.get("max_gain", DEFAULT_MAX_GAIN)
        self._max_gain = max_gain if max_gain in MAX_GAIN_CHOICES and not isinstance(max_gain, bool) else DEFAULT_MAX_GAIN
        self._bus.max_gain_changed.emit(self._max_gain)
        self._limiter = cfg.get("limiter", True) is not False  # only an explicit off turns it off
        self._bus.limiter_changed.emit(self._limiter)
        self._hw_decode = cfg.get("hw_decode") is True  # off unless turned on
        h264_reader.set_hardware(self._hw_decode)
        self._plugin_hues = cfg.get("plugin_hues", True) is not False
        theme.set_plugin_hues(self._plugin_hues)
        refresh_hues()
