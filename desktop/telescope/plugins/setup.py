import logging
import threading
from dataclasses import replace
from typing import Optional

from PyQt6.QtCore import Qt, QTimer, QUrl, pyqtSignal
from PyQt6.QtGui import QDesktopServices, QGuiApplication
from PyQt6.QtWidgets import (
    QComboBox,
    QButtonGroup, QHBoxLayout, QInputDialog, QLabel, QPushButton, QVBoxLayout,
    QWidget,
)

from telescope import diagnostics, h264_reader, pyav_vaapi, theme, vcam, vision
from telescope.platform import IS_LINUX, adb_available, adb_devices, adb_install, bundled_apk_path
from telescope.platform.linux import (
    CANCELLED, V4L2_OBS_DEV, V4L2_PHONE_DEV, result_parts,
    v4l2_devices_ready, v4l2_load, v4l2_module_loaded, v4l2_unload,
    v4l2_persist_disable, v4l2_persist_enable, v4l2_persist_status,
)
from telescope.platform.windows import (
    UC_NAME, download_unitycapture, register_unitycapture, uc_in_app_folder, uc_registered_name, unitycapture_dir,
    unitycapture_downloaded,
)
from telescope.plugin import SettingsSection, TelescopePlugin
from telescope.widgets.adb import AdbDownload
from telescope.version import display_version
from telescope.widgets.banner import Issue, copy_action
from telescope.widgets.controls import CommandBox, setting_row
from telescope.widgets.line_icons import line_icon
from telescope.widgets.settings_search import keywords
from telescope.widgets.common import (
    set_button_icon,
    Switch,
    NoScrollComboBox, NoScrollSpinBox, SegmentButton, action_button, add_card_header, button_row, card_layout,
    create_card, refresh_hues,
    run_off_ui_thread, segmented_row,
    set_status_kind, set_ui_role, ui_px, wrapped_note,
)
from telescope.i18n import N_, tr

# (width, height) tuples for canvas presets; None = auto from first frame. The English label is what's saved.
CANVAS_PRESETS: list[tuple[str, tuple[int, int] | None]] = [
    (N_("Auto (from first frame)"),    None),
    ("1080p 16:9 - 1920 x 1080",      (1920, 1080)),
    (N_("1080p 16:9 Portrait - 1080 x 1920"), (1080, 1920)),
    ("720p 16:9 - 1280 x 720",        (1280,  720)),
    (N_("720p 16:9 Portrait - 720 x 1280"), ( 720, 1280)),
    ("4K 16:9 - 3840 x 2160",         (3840, 2160)),
    (N_("4K 16:9 Portrait - 2160 x 3840"), (2160, 3840)),
    ("XGA 4:3 - 1024 x 768",          (1024,  768)),
    ("UXGA 4:3 - 1600 x 1200",        (1600, 1200)),
    (N_("Custom..."),                   "custom"),
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
_HW_NAMES = {"d3d11va": "Direct3D 11", "cuda": "NVIDIA CUDA", "vaapi": "VA-API"}
DEFAULT_MAX_GAIN = 12

_SUDO_HINT = N_("Asks for your password (pkexec or sudo): this changes system settings.")


AUTHOR_URL = "https://github.com/LunarKittyy"
BUG_REPORT_URL = "https://github.com/LunarKittyy/Telescope/issues/new?template=bug.yml"
HELP_URL = "https://github.com/LunarKittyy/Telescope/blob/master/docs/troubleshooting.md"


class AdvancedSettings(QWidget):
    """Rarely needed controls, as cards for the settings window: the virtual camera and phone app (Virtual camera &
    phone), video, microphone, look (General) and About. First-run setup lives in the onboarding checklist instead.
    The widget itself is never shown; sections() hands the cards out."""

    _sig_v4l_result   = pyqtSignal(bool, str, str)  # ok, message, the command to run by hand ("" if none)
    _sig_v4l_unload   = pyqtSignal(bool, str, str)
    _sig_persist_result = pyqtSignal(bool, str, str)
    _sig_win_checks   = pyqtSignal(str, bool, bool)  # registered camera name ("" if none), adb found, still in the app folder
    _sig_uc_done      = pyqtSignal(bool, str)
    _sig_uc_msg       = pyqtSignal(str)
    _sig_apk_done     = pyqtSignal(bool, str)

    def __init__(self, parent=None, on_apply_canvas=None, report=None, on_max_zoom=None, on_max_gain=None,
                 on_limiter=None, on_remove_extras=None, extras_in_use=None, on_hw_decode=None,
                 on_plugin_hues=None, on_vaapi=None, on_power_saving=None):
        super().__init__(parent)
        self._on_remove_extras = on_remove_extras
        self._extras_in_use = extras_in_use or (lambda: False)
        self._report = report
        self._uc_installing = False
        self._sections: list = []
        self._on_apply_canvas = on_apply_canvas
        self._on_max_zoom = on_max_zoom
        self._on_max_gain = on_max_gain
        self._on_limiter = on_limiter
        self._on_hw_decode = on_hw_decode
        self._on_vaapi = on_vaapi
        self._on_plugin_hues = on_plugin_hues
        self._on_power_saving = on_power_saving
        self._build_ui()
        self._sig_v4l_result.connect(self._on_v4l_result)
        self._sig_v4l_unload.connect(self._on_v4l_unload_result)
        self._sig_persist_result.connect(self._on_persist_result)
        self._sig_win_checks.connect(self._on_win_checks)
        self._sig_uc_done.connect(self._on_uc_done)
        self._sig_uc_msg.connect(lambda msg: self._uc_status_lbl.setText(msg)
                                  if hasattr(self, "_uc_status_lbl") else None)
        self._sig_apk_done.connect(self._on_apk_done)

    def card_visible(self) -> bool:
        """Whether the Video card is on screen, which is where a canvas change reports back."""
        return self._advanced_content.isVisible()

    def sections(self) -> list:
        """The cards as SettingsSections; the owner adds on_show."""
        return list(self._sections)

    def _dialog_parent(self) -> QWidget:
        """The window the cards are in, for what they pop up."""
        return self._vc_card.window()

    def on_show(self):
        """Their tabs came on screen: look at the system again."""
        self._refresh_extras()
        if IS_LINUX:
            self._v4l_check()
            self._refresh_persist_status()
        else:
            threading.Thread(target=self._check_win_setup, daemon=True).start()

    def _place(self, tab: str, card: QWidget, order: int = 0):
        self._sections.append(SettingsSection(tab, card, order=order))

    def _build_ui(self):
        # ── Virtual camera ────────────────────────────────────────────────────
        vc_card = self._vc_card = create_card(self)
        vc_lay = card_layout(vc_card)
        add_card_header(vc_lay, tr("Virtual camera"), "video", hue="stream")
        keywords(vc_card, tr("obs zoom teams discord webcam driver output device loopback", ctx="search"))
        if IS_LINUX:
            chk_btn    = QPushButton(tr("Check"))
            load_btn   = QPushButton(tr("Load"))
            unload_btn = QPushButton(tr("Unload"))
            load_btn.setToolTip(tr(_SUDO_HINT))
            unload_btn.setToolTip(tr(_SUDO_HINT))
            chk_btn.clicked.connect(self._v4l_check)
            load_btn.clicked.connect(self._v4l_load)
            unload_btn.clicked.connect(self._v4l_unload)
            self._v4l_cmd = CommandBox()
            strip, self._v4l_lbl = setting_row(tr("Kernel module"), tr("Checking..."), button_row(chk_btn, load_btn, unload_btn),
                                               self._v4l_cmd)
            self._v4l_lbl.setTextInteractionFlags(Qt.TextInteractionFlag.TextSelectableByMouse)
            set_status_kind(self._v4l_lbl, "status_dim")
            strip.setToolTip(tr("Already says Ready? Leave these alone; they're only for fixing a \"not ready\" status."))
            keywords(strip, tr("v4l2loopback linux virtual camera driver load unload check status not ready /dev/video "
                            "missing fix modprobe", ctx="search"))
            vc_lay.addWidget(strip)

            self._persist_chk = Switch()
            self._persist_chk.setToolTip(
                tr("Also writes it to /etc/modprobe.d/ and /etc/modules-load.d/, so it survives a reboot.") + "\n"
                + tr(_SUDO_HINT)
            )
            self._persist_chk.toggled.connect(self._on_persist_toggled)
            self._persist_cmd = CommandBox()
            strip, self._persist_status_lbl = setting_row(tr("Load when the computer starts"), "", self._persist_chk,
                                                          self._persist_cmd)
            self._persist_status_lbl.setTextInteractionFlags(Qt.TextInteractionFlag.TextSelectableByMouse)
            set_status_kind(self._persist_status_lbl, "status_dim")
            self._persist_row = self._persist_status_lbl
            self._persist_row.setVisible(False)
            keywords(strip, tr("boot startup reboot persist permanent every start modprobe v4l2loopback driver survive "
                            "restart", ctx="search"))
            vc_lay.addWidget(strip)
        else:
            self._uc_btn = action_button(tr("Install driver"))
            self._uc_btn.clicked.connect(self._install_uc)
            strip, self._uc_status_lbl = setting_row(tr("Driver"), tr("Checking..."), self._uc_btn)
            set_status_kind(self._uc_status_lbl, "status_dim")
            keywords(strip, tr("unitycapture unity video capture windows virtual camera install reinstall rename "
                            "register missing not showing obs zoom", ctx="search"))
            vc_lay.addWidget(strip)

            # Not bundled (Google's license doesn't allow passing it on), so it's fetched from Google when asked
            self._adb_btn = action_button(tr("Get adb"), "primary")
            self._adb_from_google = True
            self._adb_btn.setToolTip(tr("Downloads adb from Google, for pairing and installing over USB"))
            self._adb_btn.clicked.connect(self._get_adb)
            # Google blocks the download in some countries, so one got another way can be picked instead
            self._adb_pick_btn = QPushButton(tr("Use my own adb…"))
            self._adb_pick_btn.setToolTip(tr("Pick an adb.exe or platform-tools zip you already have"))
            self._adb_pick_btn.clicked.connect(self._pick_adb)
            self._adb_row = QWidget()
            adb_buttons = QHBoxLayout(self._adb_row)
            adb_buttons.setContentsMargins(0, 0, 0, 0)
            adb_buttons.setSpacing(8)
            adb_buttons.addWidget(self._adb_btn)
            adb_buttons.addWidget(self._adb_pick_btn)
            self._adb_row.setVisible(False)
            strip, self._adb_status_lbl = setting_row("ADB", tr("Checking..."), self._adb_row)
            set_status_kind(self._adb_status_lbl, "status_dim")
            self._adb_status_lbl.setTextFormat(Qt.TextFormat.PlainText)  # errors can hold <urlopen error ...>
            keywords(strip, tr("adb android debug bridge usb pairing download get platform-tools google own cable", ctx="search"))
            vc_lay.addWidget(strip)
            self._adb_download = AdbDownload(self)
            self._adb_download.progress.connect(self._adb_status_lbl.setText)
            self._adb_download.finished.connect(self._on_adb_downloaded)

        # The ones Add camera set up, for streaming several cameras at once
        self._extras_btn = QPushButton(tr("Remove"))
        self._extras_btn.clicked.connect(self._remove_extras)
        self._extras_row, self._extras_lbl = setting_row(
            tr("Extra cameras"), "", self._extras_btn,
            keywords=tr("multiple second cameras add camera several streams remove virtual cameras", ctx="search"))
        self._extras_row.setVisible(False)
        vc_lay.addWidget(self._extras_row)
        self._place("devices", vc_card)

        # ── Video ─────────────────────────────────────────────────────────────
        video_card = create_card(self)
        self._advanced_content = video_card
        video_lay = card_layout(video_card)
        add_card_header(video_lay, tr("Video"), "chip", hue="stream")
        keywords(video_card, tr("picture quality image stream camera output", ctx="search"))
        device = h264_reader.hardware_device()
        self._hw_chk = Switch()
        self._hw_chk.setEnabled(device is not None)
        self._hw_chk.setToolTip(tr("Decodes the Light format on the graphics card from the next stream on."))
        self._hw_chk.toggled.connect(lambda on: self._on_hw_decode(on) if self._on_hw_decode else None)
        # The line under it names the graphics card it would use, so one strip says whether it's on and what with
        # On Linux without a card PyAV can use, VA-API can be set up for AMD and Intel ones (pyav_vaapi)
        self._vaapi_btn = QPushButton(tr("Set up VA-API…"))
        self._vaapi_btn.clicked.connect(lambda: self._on_vaapi() if self._on_vaapi else None)
        self._vaapi_btn.setVisible(pyav_vaapi.supported() and (device is None or pyav_vaapi.chosen() is not None
                                                                or bool(pyav_vaapi.problem)))
        hw_controls = QHBoxLayout()
        hw_controls.setContentsMargins(0, 0, 0, 0)
        hw_controls.setSpacing(10)
        hw_controls.addWidget(self._vaapi_btn)
        hw_controls.addWidget(self._hw_chk)
        strip, self._hw_line = setting_row(
            tr("Hardware acceleration"),
            tr("Found {device}", device=_HW_NAMES.get(device, device)) if device
            else tr("No graphics card Telescope can use"), hw_controls,
            keywords=tr("hw accel gpu graphics card nvidia cuda amd intel vaapi d3d11 decode decoding h264 faster cpu "
                     "processor laptop 4k", ctx="search"))
        strip.setToolTip(self._hw_chk.toolTip())
        video_lay.addWidget(strip)
        self._saving_chk = Switch()
        self._saving_chk.toggled.connect(lambda on: self._on_power_saving(on) if self._on_power_saving else None)
        strip, _ = setting_row(
            tr("Power-saving effects"), tr("Background and Auto framing update less often"), self._saving_chk,
            keywords=tr("battery power energy watts laptop processor cpu usage fan heat slow blur framing mediapipe "
                     "lighter effects quality", ctx="search"))
        strip.setToolTip(tr("Uses less of the processor and battery while Background or Auto framing is on. They "
                            "follow quick moves a little later."))
        video_lay.addWidget(strip)
        self._max_zoom_spin = NoScrollSpinBox()
        self._max_zoom_spin.setObjectName("chip_spin")
        self._max_zoom_spin.setFixedSize(ui_px(80), ui_px(30))
        self._max_zoom_spin.setRange(*MAX_ZOOM_RANGE)
        self._max_zoom_spin.setValue(DEFAULT_MAX_ZOOM)
        self._max_zoom_spin.setSuffix("×")
        self._max_zoom_spin.valueChanged.connect(
            lambda value: self._on_max_zoom(value) if self._on_max_zoom else None)
        strip, _ = setting_row(tr("Max zoom"), "", self._max_zoom_spin,
                               keywords=tr("zoom limit magnify telephoto digital crop 10x 30x slider range", ctx="search"))
        strip.setToolTip(tr("How far the Zoom slider goes. Past what the phone can zoom itself, the rest is cropped "
                            "on this computer."))
        video_lay.addWidget(strip)

        self._canvas_combo = NoScrollComboBox()
        for label in _PRESET_LABELS:
            self._canvas_combo.addItem(tr(label), label)
        self._canvas_combo.setSizeAdjustPolicy(QComboBox.SizeAdjustPolicy.AdjustToMinimumContentsLengthWithIcon)
        self._canvas_combo.setMinimumContentsLength(18)  # the row fits the dialog; the list opens as wide as it needs
        self._canvas_combo.currentIndexChanged.connect(
            lambda _index: self._on_preset_changed(self._canvas_combo.currentData()))

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
            canvas_note = tr("Applying stops the stream, unloads v4l2loopback and loads it again. "
                             "Close OBS and anything else using the virtual camera first.")
            apply_label, apply_tooltip = tr("Apply and reload"), tr(_SUDO_HINT)
        else:
            canvas_note = tr("Applying restarts the stream at the new size. If OBS loses the source, remove it and "
                             "add it again.")
            apply_label, apply_tooltip = tr("Apply"), None
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
        self._canvas_cmd = CommandBox()
        under.addWidget(self._canvas_cmd)
        strip, _ = setting_row(tr("Canvas size"), "", self._canvas_combo, under,
                               keywords=tr("resolution output size 1080p 720p 4k portrait landscape "
                                        "aspect ratio custom width height pixels apply reload", ctx="search"))
        video_lay.addWidget(strip)
        self._place("video", video_card)

        # ── Microphone ────────────────────────────────────────────────────────
        mic_card = create_card(self)
        mic_lay = card_layout(mic_card)
        add_card_header(mic_lay, tr("Microphone"), "mic", hue="mic")
        keywords(mic_card, tr("audio sound voice input volume", ctx="search"))
        self._max_gain_btns = {db: SegmentButton(f"+{db} dB") for db in MAX_GAIN_CHOICES}
        self._max_gain_grp = QButtonGroup(mic_card)
        self._max_gain_grp.setExclusive(True)
        for db, btn in self._max_gain_btns.items():
            self._max_gain_grp.addButton(btn, db)
        self._max_gain_btns[DEFAULT_MAX_GAIN].setChecked(True)
        self._max_gain_grp.idClicked.connect(
            lambda db: self._on_max_gain(db) if self._on_max_gain else None)
        strip, _ = setting_row(tr("Max gain"), "", None, segmented_row(*self._max_gain_btns.values()),
                               keywords=tr("microphone volume loudness boost amplify quiet decibel db slider range", ctx="search"))
        strip.setToolTip(tr("How far the Gain slider goes. Less range gives finer steps; more helps a quiet phone."))
        mic_lay.addWidget(strip)
        self._limiter_chk = Switch()
        self._limiter_chk.setChecked(True)
        self._limiter_chk.toggled.connect(lambda on: self._on_limiter(on) if self._on_limiter else None)
        strip, _ = setting_row(tr("Limiter"), tr("Keeps loud sounds from clipping"), self._limiter_chk,
                               keywords=tr("microphone audio compressor distortion clipping loud peaks sound", ctx="search"))
        mic_lay.addWidget(strip)
        self._place("audio", mic_card)

        # ── Phone app ─────────────────────────────────────────────────────────
        apk_card = create_card(self)
        apk_lay = card_layout(apk_card)
        add_card_header(apk_lay, tr("Phone app"), "phone", hue="phone")
        keywords(apk_card, tr("android install apk usb", ctx="search"))
        _apk = bundled_apk_path()
        self._apk_btn = action_button(tr("Install over USB") if _apk else tr("Choose APK..."))
        set_button_icon(self._apk_btn, line_icon("download", theme.TEXT_DIM))
        self._apk_btn.setToolTip(tr("Installs the Telescope.apk that came with this app, or one you pick, on a "
                                    "phone plugged in over USB."))
        self._apk_btn.clicked.connect(self._install_apk)
        strip, self._apk_status_lbl = setting_row(
            "Telescope.apk", tr("Comes with this app") if _apk else tr("None came with this app"), self._apk_btn,
            keywords=tr("phone app install usb android reinstall newer version sideload cable", ctx="search"))
        set_status_kind(self._apk_status_lbl, "status_dim")
        apk_lay.addWidget(strip)
        self._place("devices", apk_card, order=10)

        # ── Look ────────────────────────────────────────────────────────────
        look_card = create_card(self)
        look_lay = card_layout(look_card)
        add_card_header(look_lay, tr("Look"), "sliders")
        keywords(look_card, tr("appearance theme colors colours interface", ctx="search"))
        self._hues_own, self._hues_purple = SegmentButton(tr("Colourful")), SegmentButton(tr("Monochrome"))
        self._hues_grp = QButtonGroup(look_card)
        self._hues_grp.setExclusive(True)
        self._hues_grp.addButton(self._hues_own)
        self._hues_grp.addButton(self._hues_purple)
        self._hues_own.setChecked(True)
        self._hues_own.setToolTip(tr("Every card in the main window gets its own colour."))
        self._hues_grp.buttonClicked.connect(
            lambda btn: self._on_plugin_hues(btn is self._hues_own) if self._on_plugin_hues else None)
        strip, _ = setting_row(tr("Card colours"), "", None, segmented_row(self._hues_own, self._hues_purple),
                               keywords=tr("color colour theme appearance monochrome colourful purple hue accent look", ctx="search"))
        look_lay.addWidget(strip)
        self._place("general", look_card, order=10)

        about_card = create_card(self)
        about_lay = card_layout(about_card)
        add_card_header(about_lay, tr("About"), "help")
        keywords(about_card, tr("info version help support credits", ctx="search"))
        # Translators: keep the <a href="{url}"> link and the name LunarKittyy as they are
        by_lbl = QLabel(tr('Made by <a href="{url}">LunarKittyy</a>', url=AUTHOR_URL))
        by_lbl.setTextFormat(Qt.TextFormat.RichText)
        by_lbl.setTextInteractionFlags(Qt.TextInteractionFlag.LinksAccessibleByMouse)  # a credit, not a Tab stop
        by_lbl.setFocusPolicy(Qt.FocusPolicy.NoFocus)
        by_lbl.setOpenExternalLinks(True)
        strip, _ = setting_row("Telescope", tr("Version {version}", version=display_version()), by_lbl,
                               keywords=tr("about author credits made by version number build", ctx="search"))
        about_lay.addWidget(strip)
        self._diag_btn = QPushButton(tr("Copy diagnostics"))
        set_button_icon(self._diag_btn, line_icon("copy", theme.TEXT_DIM))
        self._diag_btn.setToolTip(tr("Copies info for a bug report, leaving out pairing tokens, addresses and file paths."))
        self._diag_btn.clicked.connect(self._copy_diagnostics)
        self._diag_btn.setVisible(self._report is not None)
        self._log_btn = QPushButton(tr("Open log"))
        self._log_btn.setToolTip(tr("The full log, in the temp folder (the system clears it)."))
        self._log_btn.clicked.connect(self._open_log)
        self._log_btn.setVisible(self._report is not None and diagnostics.events.path is not None)
        self._bug_btn = QPushButton(tr("Report a bug"))
        self._bug_btn.setToolTip(tr("Copies the diagnostics and opens the bug form on GitHub."))
        self._bug_btn.clicked.connect(self._report_bug)
        self._support_row, _ = setting_row(tr("Bug reports"), tr("Paste the diagnostics into the report."),
                                           self._support_buttons())
        keywords(self._support_row, tr("bug report diagnostics logs debug support problem issue help copy log file", ctx="search"))
        self._support_row.setVisible(self._report is not None)
        about_lay.addWidget(self._support_row)
        help_btn = QPushButton(tr("Troubleshooting"))
        help_btn.clicked.connect(lambda: QDesktopServices.openUrl(QUrl(HELP_URL)))
        help_row, _ = setting_row(tr("Need help?"), tr("Fixes for common problems, and where to ask if yours isn't there."),
                                  help_btn)
        keywords(help_row, tr("help support troubleshooting question faq problem not working stuck ask", ctx="search"))
        about_lay.addWidget(help_row)
        self._place("about", about_card, order=10)

    def _copy_diagnostics(self):
        QGuiApplication.clipboard().setText(self._report())
        self._diag_btn.setText(tr("Copied"))
        # A timer owned by the button dies with it, unlike a bare singleShot lambda, which would touch a deleted button
        reset = QTimer(self._diag_btn)
        reset.setSingleShot(True)
        reset.timeout.connect(lambda: (self._diag_btn.setText(tr("Copy diagnostics")), reset.deleteLater()))
        reset.start(2000)

    def _support_buttons(self) -> QHBoxLayout:
        # Natural widths: three buttons sharing the row equally squeeze the text next to them onto extra lines
        lay = QHBoxLayout()
        lay.setContentsMargins(0, 0, 0, 0)
        lay.setSpacing(8)
        for btn in (self._log_btn, self._diag_btn, self._bug_btn):
            lay.addWidget(btn)
        return lay

    def _report_bug(self):
        self._copy_diagnostics()
        QDesktopServices.openUrl(QUrl(BUG_REPORT_URL))

    def _open_log(self):
        if diagnostics.events.path is not None:
            QDesktopServices.openUrl(QUrl.fromLocalFile(str(diagnostics.events.path)))

    def _on_preset_changed(self, label: str):
        self._custom_widget.setVisible(label == "Custom...")

    def _apply_canvas(self):
        w, h = self._get_selected_dims()
        self._canvas_apply_btn.setEnabled(False)
        set_status_kind(self._canvas_status_lbl, "status_dim")
        self._canvas_status_lbl.setText(tr("Reloading loopback...") if IS_LINUX else tr("Restarting stream..."))
        self._canvas_status_row.setVisible(True)
        if self._on_apply_canvas:
            self._on_apply_canvas(w, h)

    def set_canvas_apply_result(self, ok: bool, msg: str, command: str = ""):
        """Called from SetupPlugin once the reload completes; command is one to run by hand, shown under it."""
        self._canvas_apply_btn.setEnabled(True)
        self._canvas_cmd.set_command(command)
        if ok:
            set_status_kind(self._canvas_status_lbl, "status_ok")
            self._canvas_status_lbl.setText(
                tr("Done - loopback reloaded.") if IS_LINUX else tr("Done - canvas updated."))
        else:
            set_status_kind(self._canvas_status_lbl, "status_err")
            if "in use" in msg.lower():
                self._canvas_status_lbl.setText(
                    tr("Failed: module is still in use. Close OBS and any other app "
                       "using the virtual camera, then try again.")
                )
            else:
                self._canvas_status_lbl.setText(tr("Failed: {message}", message=msg))
        self._canvas_status_row.setVisible(True)

    def _get_selected_dims(self) -> tuple[int | None, int | None]:
        label = self._canvas_combo.currentData()
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
        idx = self._canvas_combo.findData(label)
        if idx >= 0:
            self._canvas_combo.setCurrentIndex(idx)
        self._custom_w.setValue(custom_w)
        self._custom_h.setValue(custom_h)

    def get_canvas_preset_label(self) -> str:
        return self._canvas_combo.currentData()

    def set_max_gain(self, value: int):
        self._max_gain_btns[value].setChecked(True)

    def set_hw_decode(self, on: bool):
        self._hw_chk.blockSignals(True)  # syncing the stored value isn't a change
        self._hw_chk.setChecked(on)
        self._hw_chk.blockSignals(False)

    def set_power_saving(self, on: bool):
        self._saving_chk.blockSignals(True)  # syncing the stored value isn't a change
        self._saving_chk.setChecked(on)
        self._saving_chk.blockSignals(False)

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
            self._v4l_lbl.setText(tr("Ready: {phone} + {obs}", phone=V4L2_PHONE_DEV, obs=V4L2_OBS_DEV))
        elif v4l2_module_loaded():
            set_status_kind(self._v4l_lbl, "status_warn")
            self._v4l_lbl.setText(tr("Module loaded but {device} not found - another config active", device=V4L2_PHONE_DEV))
        else:
            set_status_kind(self._v4l_lbl, "status_err")
            self._v4l_lbl.setText(tr("Not loaded - click Load"))

    def _v4l_load(self):
        set_status_kind(self._v4l_lbl, "status_dim")
        self._v4l_lbl.setText(tr("Loading..."))
        threading.Thread(target=lambda: self._sig_v4l_result.emit(*result_parts(v4l2_load())), daemon=True).start()

    def _v4l_unload(self):
        set_status_kind(self._v4l_lbl, "status_dim")
        self._v4l_lbl.setText(tr("Unloading..."))
        def unload():
            with vcam.device_released():
                result = v4l2_unload()
            self._sig_v4l_unload.emit(*result_parts(result))
        threading.Thread(target=unload, daemon=True).start()

    def _on_v4l_result(self, ok: bool, msg: str, command: str = ""):
        self._v4l_lbl.setText(tr("Loaded - {message}", message=msg) if ok else tr("Failed - {message}", message=msg))
        set_status_kind(self._v4l_lbl, "status_ok" if ok else "status_err")
        self._v4l_cmd.set_command(command)

    def _on_v4l_unload_result(self, ok: bool, msg: str, command: str = ""):
        self._v4l_lbl.setText(tr("Unloaded - {message}", message=msg) if ok else tr("Failed - {message}", message=msg))
        set_status_kind(self._v4l_lbl, "status_ok" if ok else "status_err")
        self._v4l_cmd.set_command(command)

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
        self._persist_status_lbl.setText(tr("Working..."))
        self._persist_row.setVisible(True)
        action = v4l2_persist_enable if checked else v4l2_persist_disable
        threading.Thread(target=lambda: self._sig_persist_result.emit(*result_parts(action())), daemon=True).start()

    def _on_persist_result(self, ok: bool, msg: str, command: str = ""):
        self._persist_chk.setEnabled(True)
        self._persist_cmd.set_command(command)
        if not ok:
            # Revert the checkbox without re-triggering the write/remove action.
            self._persist_chk.blockSignals(True)
            self._persist_chk.setChecked(not self._persist_chk.isChecked())
            self._persist_chk.blockSignals(False)
        set_status_kind(self._persist_status_lbl, "status_ok" if ok else "status_err")
        self._persist_status_lbl.setText(msg)
        self._persist_row.setVisible(True)

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
            tr("Stop the streams going to them first") if busy else
            tr("Back to just the one camera; Add camera sets them up again.") + " "
            + (tr("Asks for your password.") if IS_LINUX else tr("Windows asks for permission.")))

    def _remove_extras(self):
        self._extras_btn.setEnabled(False)
        self._extras_lbl.setText(tr("Removing…"))
        self._on_remove_extras(self._on_extras_removed)

    def _on_extras_removed(self, ok: bool, msg: str):
        if ok:
            self._refresh_extras(tr("Removed"), "status_ok")
        elif msg == CANCELLED:
            self._refresh_extras()
        else:
            self._refresh_extras(tr("Couldn't remove them: {message}", message=msg), "status_err")

    # ── Windows ───────────────────────────────────────────────────────────────

    def _check_win_setup(self):
        self._sig_win_checks.emit(uc_registered_name() or "", adb_available(), uc_in_app_folder())

    def _on_win_checks(self, uc_name: str, adb_ok: bool, in_app_folder: bool = False):
        self._uc_btn.setEnabled(not self._uc_installing)
        if uc_name and in_app_folder:
            # Registered by an older Telescope from the app's own folder, which any program running as this user can write to.
            set_status_kind(self._uc_status_lbl, "status_warn")
            self._uc_status_lbl.setText(tr("Reinstall for a security fix"))
            self._uc_btn.setText(tr("Reinstall"))
            self._uc_btn.setToolTip(tr("Moves the driver into Program Files (Windows asks for admin access once)."))
            set_ui_role(self._uc_btn, "primary")
        elif uc_name == UC_NAME:
            set_status_kind(self._uc_status_lbl, "status_ok")
            self._uc_status_lbl.setText(tr("Ready"))
            self._uc_btn.setText(tr("Reinstall"))
            self._uc_btn.setToolTip("")
            set_ui_role(self._uc_btn, "")
        elif uc_name:
            # Registered by an older Telescope under UnityCapture's own name. Works, just harder to find.
            set_status_kind(self._uc_status_lbl, "status_warn")
            self._uc_status_lbl.setText(tr("Apps list it as “{name}”", name=uc_name))
            self._uc_btn.setText(tr("Rename"))
            self._uc_btn.setToolTip(tr("Register it again as “{name}”", name=UC_NAME))
            set_ui_role(self._uc_btn, "primary")
        else:
            set_status_kind(self._uc_status_lbl, "status_err")
            dlls = unitycapture_downloaded(unitycapture_dir())
            self._uc_status_lbl.setText(
                tr("Not installed") if dlls else tr("Not installed (Install downloads the driver first)"))
            self._uc_btn.setText(tr("Install driver"))
            self._uc_btn.setToolTip("")
            set_ui_role(self._uc_btn, "primary")
        if self._adb_download.running:
            return
        self._adb_row.setVisible(not adb_ok)
        if adb_ok:
            set_status_kind(self._adb_status_lbl, "status_ok")
            self._adb_status_lbl.setText(tr("Ready"))
        else:
            set_status_kind(self._adb_status_lbl, "status_err")
            self._adb_status_lbl.setText(tr("Not found. Pairing and installing over USB need it; Wi-Fi doesn't."))

    def _adb_starting(self):
        self._adb_btn.setEnabled(False)
        self._adb_pick_btn.setEnabled(False)
        set_status_kind(self._adb_status_lbl, "status_dim")

    def _get_adb(self):
        self._adb_from_google = True
        self._adb_download.ask_and_start(self._dialog_parent(), self._adb_starting)

    def _pick_adb(self):
        self._adb_from_google = False
        self._adb_download.pick_and_start(self._dialog_parent(), self._adb_starting)

    def _on_adb_downloaded(self, ok: bool, detail: str):
        self._adb_btn.setEnabled(True)
        self._adb_pick_btn.setEnabled(True)
        if ok:
            self._adb_row.setVisible(False)
            set_status_kind(self._adb_status_lbl, "status_ok")
            if self._adb_from_google:
                text = (tr("Ready (adb {version}, from Google)", version=detail) if detail
                        else tr("Ready (adb, from Google)"))
            else:
                text = tr("Ready (adb {version})", version=detail) if detail else tr("Ready (adb)")
            self._adb_status_lbl.setText(text)
        else:
            self._adb_btn.setText(tr("Try again"))
            set_status_kind(self._adb_status_lbl, "status_err")
            self._adb_status_lbl.setText(detail)

    def _install_uc(self):
        self._uc_installing = True
        self._uc_btn.setEnabled(False)
        set_status_kind(self._uc_status_lbl, "status_dim")

        def worker():
            try:
                if not unitycapture_downloaded(unitycapture_dir()):
                    self._sig_uc_msg.emit(tr("Downloading driver files..."))
                    ok, msg = download_unitycapture()
                    if not ok:
                        self._sig_uc_done.emit(False, msg)
                        return
                self._sig_uc_msg.emit(tr("Registering (admin access required)..."))
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
            self._uc_status_lbl.setText(tr("Ready"))
            self._uc_btn.setText(tr("Reinstall"))
            self._uc_btn.setToolTip("")
        else:
            set_status_kind(self._uc_status_lbl, "status_err")
            self._uc_status_lbl.setText(tr("Failed: {message}", message=msg))
            self._uc_btn.setText(tr("Retry"))

    # ── APK ───────────────────────────────────────────────────────────────────

    def _install_apk(self):
        if not adb_available():
            set_status_kind(self._apk_status_lbl, "status_err")
            self._apk_status_lbl.setText(tr("Installing over USB needs adb. Click Get adb under Virtual camera above.")
                                         if not IS_LINUX else
                                         tr("Installing over USB needs adb. Install it from your distro's packages."))
            return

        apk = bundled_apk_path()
        if apk is None:
            from PyQt6.QtWidgets import QFileDialog
            chosen, _ = QFileDialog.getOpenFileName(self._dialog_parent(), tr("Select APK"), "",
                                                    tr("Android Package (*.apk)"))
            if not chosen:
                return
            path = chosen
        else:
            path = str(apk)

        serials = run_off_ui_thread(adb_devices)
        if not serials:
            set_status_kind(self._apk_status_lbl, "status_err")
            self._apk_status_lbl.setText(tr("No phone found over USB. Plug it in and allow USB debugging when the phone "
                                            "asks."))
            return
        serial = serials[0]
        if len(serials) > 1:
            serial, ok = QInputDialog.getItem(
                self._dialog_parent(), tr("Select device"),
                tr("More than one phone is plugged in. Which one should get the app?"),
                serials, 0, False,
            )
            if not ok:
                return

        self._apk_btn.setEnabled(False)
        set_status_kind(self._apk_status_lbl, "status_dim")
        self._apk_status_lbl.setText(tr("Installing..."))

        def worker():
            ok, detail = adb_install(serial, path)
            self._sig_apk_done.emit(ok, tr("Installed") if ok else detail)

        threading.Thread(target=worker, daemon=True).start()

    def _on_apk_done(self, ok: bool, msg: str):
        self._apk_btn.setEnabled(True)
        set_status_kind(self._apk_status_lbl, "status_ok" if ok else "status_err")
        self._apk_status_lbl.setText(msg)


class SetupPlugin(TelescopePlugin):
    name = "setup"
    title = N_("Advanced settings")
    about = N_("The virtual camera, its size, extra cameras, video and microphone limits, and the Updates & About tab.")
    required = True

    def setup(self, host, bus):
        self._host = host
        self._bus = bus
        self._panel: Optional[AdvancedSettings] = None
        self._vaapi_dlg = None  # closing setup leaves a build running; reopening follows the same job
        self._max_zoom = DEFAULT_MAX_ZOOM
        self._max_gain = DEFAULT_MAX_GAIN
        self._limiter = True
        self._hw_decode = False
        self._power_saving = False
        self._plugin_hues = True
        self._canvas_preset = "Auto (from first frame)"
        self._custom_w = 1920
        self._custom_h = 1080

    def get_canvas_dims(self) -> tuple[int | None, int | None]:
        """Return (canvas_w, canvas_h) for StreamWorker, or (None, None) for auto."""
        return canvas_dims(self.get_config())

    def create_panel(self) -> Optional[QWidget]:
        """No panel; its settings are in the settings window (see create_settings_sections)."""
        return None

    def create_settings_sections(self) -> list:
        self._panel = AdvancedSettings(
            self._host, on_apply_canvas=self._on_apply_canvas,
            report=self._host.diagnostics_report,
            on_max_zoom=self._on_max_zoom, on_max_gain=self._on_max_gain,
            on_limiter=self._on_limiter, on_plugin_hues=self._on_plugin_hues,
            on_remove_extras=lambda done: self._host.remove_extra_cameras(done),
            extras_in_use=lambda: self._host.stream_count() > 1,
            on_hw_decode=self._on_hw_decode, on_vaapi=self._open_vaapi, on_power_saving=self._on_power_saving)
        self._sync_panel()
        return [replace(section, on_show=self._on_show) for section in self._panel.sections()]

    def _sync_panel(self):
        """The controls show what's saved, which changes while Settings is closed (a preset, a config reload)."""
        panel = self._panel
        panel.set_canvas_preset(self._canvas_preset, self._custom_w, self._custom_h)
        panel.set_max_zoom(self._max_zoom)
        panel.set_max_gain(self._max_gain)
        panel.set_limiter(self._limiter)
        panel.set_hw_decode(self._hw_decode)
        panel.set_power_saving(self._power_saving)
        panel.set_plugin_hues(self._plugin_hues)

    def _on_show(self):
        self._sync_panel()
        self._panel.on_show()

    def _on_apply_canvas(self, w: int | None, h: int | None):
        # In effect for the restart, but kept only once the host accepts it
        before = (self._canvas_preset, self._custom_w, self._custom_h)
        if self._panel:
            self._canvas_preset = self._panel.get_canvas_preset_label()
            if self._canvas_preset == "Custom...":
                self._custom_w = w
                self._custom_h = h

        def on_done(ok: bool, msg: str, command: str = ""):
            if not ok:
                self._canvas_preset, self._custom_w, self._custom_h = before
            self._host.schedule_save()
            if self._panel:
                self._panel.set_canvas_apply_result(ok, msg, command)
            if command and not (self._panel and self._panel.card_visible()):  # closed meanwhile: the main window says it
                self._host.show_issue("vcam", Issue(tr("Resize the virtual camera"), msg, [copy_action(command)],
                                                    kind="warn", details=command))
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
        if self._vaapi_dlg is None:
            self._vaapi_dlg = VaapiSetupDialog(self._panel._dialog_parent() if self._panel else None, self._host.restart_app)
        else:
            self._vaapi_dlg.check_again()
        self._vaapi_dlg.exec()

    def _on_hw_decode(self, on: bool):
        self._hw_decode = on
        h264_reader.set_hardware(on)
        self._host.schedule_save()

    def _on_power_saving(self, on: bool):
        self._power_saving = on
        vision.set_power_saving(on)
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
            "power_saving":    self._power_saving,
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
        self._power_saving = cfg.get("power_saving") is True  # off unless turned on
        vision.set_power_saving(self._power_saving)
        self._plugin_hues = cfg.get("plugin_hues", True) is not False
        theme.set_plugin_hues(self._plugin_hues)
        refresh_hues()
