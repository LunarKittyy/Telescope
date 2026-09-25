import threading
from typing import Optional

from PyQt6.QtCore import Qt, pyqtSignal
from PyQt6.QtGui import QAction
from PyQt6.QtWidgets import (
    QCheckBox, QDialog, QHBoxLayout, QInputDialog, QLabel, QPushButton, QWidget,
)

from telescope.platform import IS_LINUX, adb_available, adb_devices, adb_install, bundled_apk_path
from telescope.platform.linux import (
    V4L2_OBS_DEV, V4L2_PHONE_DEV, as_text,
    v4l2_devices_ready, v4l2_load, v4l2_module_loaded, v4l2_unload,
    v4l2_persist_disable, v4l2_persist_enable, v4l2_persist_status,
)
from telescope.platform.windows import (
    UC_NAME, download_unitycapture, register_unitycapture, uc_registered_name, unitycapture_dir,
)
from telescope.plugin import TelescopePlugin
from telescope.version import display_version
from telescope.widgets.common import (
    NoScrollComboBox, NoScrollSpinBox, action_button, add_card_header, button_row, card_layout,
    control_row, create_card, dialog_buttons, dialog_header, dialog_layout, run_off_ui_thread,
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

_SUDO_HINT = "This will prompt for your password (via pkexec/sudo) to make a system-level change."


class AdvancedDialog(QDialog):
    """Rarely needed controls. First-run setup lives in the onboarding checklist instead."""

    _sig_v4l_result   = pyqtSignal(bool, str)
    _sig_v4l_unload   = pyqtSignal(bool, str)
    _sig_persist_result = pyqtSignal(bool, str)
    _sig_win_checks   = pyqtSignal(str, bool)  # registered camera name ("" if none), adb found
    _sig_uc_done      = pyqtSignal(bool, str)
    _sig_uc_msg       = pyqtSignal(str)
    _sig_apk_done     = pyqtSignal(bool, str)

    def __init__(self, parent=None, on_apply_canvas=None):
        super().__init__(parent)
        self.setWindowTitle("Advanced")
        self.setMinimumWidth(ui_px(560))
        self.setWindowFlag(Qt.WindowType.WindowContextHelpButtonHint, False)
        self._on_apply_canvas = on_apply_canvas
        self._build_ui()
        self._sig_v4l_result.connect(self._on_v4l_result)
        self._sig_v4l_unload.connect(self._on_v4l_unload_result)
        self._sig_persist_result.connect(self._on_persist_result)
        self._sig_win_checks.connect(self._on_win_checks)
        self._sig_uc_done.connect(self._on_uc_done)
        self._sig_uc_msg.connect(lambda msg: self._uc_status_lbl.setText(msg)
                                  if hasattr(self, "_uc_status_lbl") else None)
        self._sig_apk_done.connect(self._on_apk_done)

    def showEvent(self, event):
        super().showEvent(event)
        if IS_LINUX:
            self._v4l_check()
            self._refresh_persist_status()
        else:
            threading.Thread(target=self._check_win_setup, daemon=True).start()

    def _build_ui(self):
        lay = dialog_layout(self)
        dialog_header(lay, "Advanced", "Virtual camera module, output canvas, and installing the phone app over USB.")

        # ── Virtual camera ────────────────────────────────────────────────────
        vc_card = create_card()
        vc_lay = card_layout(vc_card)
        add_card_header(vc_lay, "Virtual camera", "stream")
        if IS_LINUX:
            self._v4l_lbl = QLabel("Checking...")
            self._v4l_lbl.setWordWrap(True)
            self._v4l_lbl.setTextInteractionFlags(Qt.TextInteractionFlag.TextSelectableByMouse)
            set_status_kind(self._v4l_lbl, "status_dim")
            self._v4l_lbl.setWordWrap(True)
            self._v4l_lbl.setToolTip(
                f"Virtual camera mapping:\n  Phone Feed: {V4L2_PHONE_DEV}\n  OBS Loopback: {V4L2_OBS_DEV}"
            )
            vc_lay.addLayout(control_row("Status", self._v4l_lbl, stretch=True))
            chk_btn    = QPushButton("Check")
            load_btn   = QPushButton("Load")
            unload_btn = QPushButton("Unload")
            load_btn.setToolTip(_SUDO_HINT)
            unload_btn.setToolTip(_SUDO_HINT)
            chk_btn.clicked.connect(self._v4l_check)
            load_btn.clicked.connect(self._v4l_load)
            unload_btn.clicked.connect(self._v4l_unload)
            vc_lay.addLayout(control_row("Module", button_row(chk_btn, load_btn, unload_btn), stretch=True))
            vc_lay.addLayout(control_row("", wrapped_note(
                "Already says Ready? Leave these alone; they're only for fixing a \"not ready\" status."),
                stretch=True))

            self._persist_chk = QCheckBox("On")
            self._persist_chk.setToolTip(
                "Writes the same module config to /etc/modprobe.d/ and "
                "/etc/modules-load.d/ so it survives a reboot.\n\n" + _SUDO_HINT
            )
            self._persist_chk.toggled.connect(self._on_persist_toggled)
            vc_lay.addLayout(control_row("Load at boot", self._persist_chk))

            self._persist_status_lbl = wrapped_note("")
            self._persist_status_lbl.setTextInteractionFlags(Qt.TextInteractionFlag.TextSelectableByMouse)
            set_status_kind(self._persist_status_lbl, "status_dim")
            self._persist_status_lbl.setTextInteractionFlags(
                Qt.TextInteractionFlag.TextSelectableByMouse
            )
            self._persist_row = QWidget()
            self._persist_row.setObjectName("form_row")
            self._persist_row.setLayout(control_row("", self._persist_status_lbl, stretch=True))
            self._persist_row.setVisible(False)
            vc_lay.addWidget(self._persist_row)
        else:
            self._uc_status_lbl = QLabel("Checking...")
            set_status_kind(self._uc_status_lbl, "status_dim")
            vc_lay.addLayout(control_row("Driver", self._uc_status_lbl, stretch=True))
            self._uc_btn = action_button("Install driver")
            self._uc_btn.clicked.connect(self._install_uc)
            vc_lay.addLayout(control_row("", self._uc_btn))

            self._adb_status_lbl = QLabel("Checking...")
            set_status_kind(self._adb_status_lbl, "status_dim")
            vc_lay.addLayout(control_row("ADB", self._adb_status_lbl, stretch=True))
        lay.addWidget(vc_card)

        # ── Phone app ─────────────────────────────────────────────────────────
        apk_card = create_card()
        apk_lay = card_layout(apk_card)
        add_card_header(apk_lay, "Phone app", "devices")
        _apk = bundled_apk_path()
        self._apk_status_lbl = QLabel("Telescope.apk found" if _apk else "No Telescope.apk came with this app")
        set_status_kind(self._apk_status_lbl, "status_ok" if _apk else "status_dim")
        self._apk_status_lbl.setWordWrap(True)
        apk_lay.addLayout(control_row("APK", self._apk_status_lbl, stretch=True))
        self._apk_btn = action_button("Install APK" if _apk else "Choose APK...", "primary")
        self._apk_btn.clicked.connect(self._install_apk)
        apk_lay.addLayout(control_row("", self._apk_btn))
        apk_lay.addLayout(control_row("", wrapped_note(
            "Installs the Telescope.apk that came with this app, or one you pick, on a phone plugged "
            "in over USB."), stretch=True))
        lay.addWidget(apk_card)

        # ── Canvas ──────────────────────────────────────────────────────────
        adv_card = create_card()
        self._advanced_content = adv_card
        adv_lay = card_layout(adv_card)
        add_card_header(adv_lay, "Virtual camera canvas", "expand")

        self._canvas_combo = NoScrollComboBox()
        self._canvas_combo.addItems(_PRESET_LABELS)
        self._canvas_combo.currentTextChanged.connect(self._on_preset_changed)
        adv_lay.addLayout(control_row("Canvas", self._canvas_combo, stretch=True))

        # Custom W x H spinboxes (hidden unless "Custom..." selected)
        self._custom_w = NoScrollSpinBox()
        self._custom_w.setRange(64, 7680)
        self._custom_w.setValue(1920)
        self._custom_w.setSuffix(" px")
        self._custom_h = NoScrollSpinBox()
        self._custom_h.setRange(64, 4320)
        self._custom_h.setValue(1080)
        self._custom_h.setSuffix(" px")
        size_lay = QHBoxLayout()
        size_lay.setContentsMargins(0, 0, 0, 0)
        size_lay.setSpacing(8)
        size_lay.addWidget(self._custom_w, 1)
        times = QLabel("×")
        times.setObjectName("dim")
        size_lay.addWidget(times)
        size_lay.addWidget(self._custom_h, 1)
        self._custom_widget = QWidget()
        self._custom_widget.setObjectName("form_row")
        self._custom_widget.setLayout(control_row("Size", size_lay, stretch=True))
        self._custom_widget.setVisible(False)
        adv_lay.addWidget(self._custom_widget)

        if IS_LINUX:
            adv_lay.addLayout(control_row("", wrapped_note(
                "Applying stops the stream, unloads v4l2loopback and loads it again. "
                "Close OBS and anything else using the virtual camera first.", "status_warn"),
                stretch=True))
            apply_label = "Apply and reload"
            apply_tooltip = _SUDO_HINT
        else:
            adv_lay.addLayout(control_row("", wrapped_note(
                "Applying restarts the stream at the new size. "
                "If OBS loses the source, remove it and add it again."), stretch=True))
            apply_label = "Apply"
            apply_tooltip = None

        self._canvas_apply_btn = action_button(apply_label, tooltip=apply_tooltip or "")
        self._canvas_apply_btn.clicked.connect(self._apply_canvas)
        adv_lay.addLayout(control_row("", self._canvas_apply_btn))

        self._canvas_status_lbl = wrapped_note("")
        set_status_kind(self._canvas_status_lbl, "status_dim")
        self._canvas_status_row = QWidget()
        self._canvas_status_row.setObjectName("form_row")
        self._canvas_status_row.setLayout(control_row("", self._canvas_status_lbl, stretch=True))
        self._canvas_status_row.setVisible(False)
        adv_lay.addWidget(self._canvas_status_row)

        lay.addWidget(adv_card)
        lay.addStretch(1)

        version_lbl = QLabel(f"Telescope {display_version()}")
        version_lbl.setObjectName("dim")
        version_lbl.setTextInteractionFlags(Qt.TextInteractionFlag.TextSelectableByMouse)
        lay.addWidget(version_lbl)

        close_btn = QPushButton("Close")
        close_btn.clicked.connect(self.accept)
        dialog_buttons(lay, close_btn)

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
        threading.Thread(target=lambda: self._sig_v4l_unload.emit(*as_text(v4l2_unload())), daemon=True).start()

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
        self.adjustSize()
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
        self.adjustSize()

    # ── Windows ───────────────────────────────────────────────────────────────

    def _check_win_setup(self):
        self._sig_win_checks.emit(uc_registered_name() or "", adb_available())

    def _on_win_checks(self, uc_name: str, adb_ok: bool):
        if uc_name == UC_NAME:
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
            dlls = (unitycapture_dir() / "UnityCaptureFilter64.dll").exists()
            self._uc_status_lbl.setText(
                "Not installed" if dlls else "Not installed (Install downloads the driver first)")
            self._uc_btn.setText("Install driver")
            self._uc_btn.setToolTip("")
            set_ui_role(self._uc_btn, "primary")
        if adb_ok:
            set_status_kind(self._adb_status_lbl, "status_ok")
            self._adb_status_lbl.setText("Ready")
        else:
            set_status_kind(self._adb_status_lbl, "status_err")
            self._adb_status_lbl.setText("Not found. Pairing and installing over USB won't work.")

    def _install_uc(self):
        self._uc_btn.setEnabled(False)
        set_status_kind(self._uc_status_lbl, "status_dim")

        def worker():
            if not (unitycapture_dir() / "UnityCaptureFilter64.dll").exists():
                self._sig_uc_msg.emit("Downloading driver files...")
                ok, msg = download_unitycapture()
                if not ok:
                    self._sig_uc_done.emit(False, msg)
                    return
            self._sig_uc_msg.emit("Registering (admin access required)...")
            ok, msg = register_unitycapture()
            self._sig_uc_done.emit(ok, msg)

        threading.Thread(target=worker, daemon=True).start()

    def _on_uc_done(self, ok: bool, msg: str):
        self._uc_btn.setEnabled(True)
        if ok:
            set_status_kind(self._uc_status_lbl, "status_ok")
            self._uc_status_lbl.setText("Ready")
            self._uc_btn.setText("Reinstall")
        else:
            set_status_kind(self._uc_status_lbl, "status_err")
            self._uc_status_lbl.setText(f"Failed: {msg}")
            self._uc_btn.setText("Retry")

    # ── APK ───────────────────────────────────────────────────────────────────

    def _install_apk(self):
        if not adb_available():
            set_status_kind(self._apk_status_lbl, "status_err")
            self._apk_status_lbl.setText("adb not found - install Android platform-tools first")
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

    def setup(self, host, bus):
        self._host = host
        self._dlg: Optional[AdvancedDialog] = None
        self._canvas_preset = "Auto (from first frame)"
        self._custom_w = 1920
        self._custom_h = 1080

    def get_canvas_dims(self) -> tuple[int | None, int | None]:
        """Return (canvas_w, canvas_h) for StreamWorker, or (None, None) for auto."""
        val = _PRESET_VALUES.get(self._canvas_preset)
        if val is None:
            return None, None
        if val == "custom":
            return self._custom_w, self._custom_h
        return val

    def create_panel(self) -> Optional[QWidget]:
        """No panel; setup is dialog-only (see create_menu_actions)."""
        return None

    def create_menu_actions(self) -> list:
        action = QAction("Advanced…", None)
        action.triggered.connect(self._open)
        return [action]

    def _open(self):
        if self._dlg is None:
            self._dlg = AdvancedDialog(self._host, on_apply_canvas=self._on_apply_canvas)
        self._dlg.set_canvas_preset(self._canvas_preset, self._custom_w, self._custom_h)
        self._dlg.show()
        self._dlg.raise_()
        self._dlg.activateWindow()

    def _on_apply_canvas(self, w: int | None, h: int | None):
        if self._dlg:
            self._canvas_preset = self._dlg.get_canvas_preset_label()
            if self._canvas_preset == "Custom...":
                self._custom_w = w
                self._custom_h = h
        self._host.schedule_save()

        def on_done(ok: bool, msg: str):
            if self._dlg:
                self._dlg.set_canvas_apply_result(ok, msg)
        self._host.restart_vcam_canvas(w, h, on_done=on_done)

    def get_config(self) -> dict:
        return {
            "canvas_preset":   self._canvas_preset,
            "custom_canvas_w": self._custom_w,
            "custom_canvas_h": self._custom_h,
        }

    def set_config(self, cfg: dict):
        self._canvas_preset = cfg.get("canvas_preset", "Auto (from first frame)")
        self._custom_w = cfg.get("custom_canvas_w", 1920)
        self._custom_h = cfg.get("custom_canvas_h", 1080)
