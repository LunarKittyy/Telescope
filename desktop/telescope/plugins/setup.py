import threading
from typing import Optional

from PyQt6.QtCore import Qt, pyqtSignal
from PyQt6.QtWidgets import (
    QCheckBox, QDialog, QFrame, QGroupBox, QHBoxLayout, QInputDialog, QLabel,
    QPushButton, QSpinBox, QTextBrowser, QVBoxLayout, QWidget,
)

from telescope.platform import (
    IS_LINUX, IS_WINDOWS, _run, adb_available, adb_devices, adb_exe, bundled_apk_path,
)
from telescope.platform.linux import (
    V4L2_OBS_DEV, V4L2_PHONE_DEV,
    v4l2_devices_ready, v4l2_load, v4l2_module_loaded, v4l2_unload,
    v4l2_persist_disable, v4l2_persist_enable, v4l2_persist_status,
)
from telescope.platform.windows import (
    download_unitycapture, register_unitycapture, uc_is_registered, unitycapture_dir,
)
from telescope.plugin import TelescopePlugin
from telescope.widgets.common import NoScrollComboBox, create_vector_icon

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


_GUIDE_HTML = """
<style>
  body { color: #e8eaed; font-family: sans-serif; font-size: 15px; }
  h2   { color: #ffffff; font-size: 22px; margin-bottom: 6px; font-family: sans-serif; }
  h3   { color: #ffffff; font-size: 17px; margin-top: 18px; margin-bottom: 6px; font-family: sans-serif; }
  p, li { color: #e8eaed; line-height: 1.6; margin-bottom: 6px; }
  b    { color: #ffffff; }
  code { color: #b0bec5; font-size: 13px; }
  a    { color: #6ab0f5; }
  .warn { color: #e8c97a; }
</style>
<h2>Quick Start</h2>

<h3>1. Install the Android app</h3>
<p><b>Easiest:</b> connect your phone via USB with
<a href="https://developer.android.com/studio/debug/dev-options">USB debugging</a> enabled, then click
<b>Setup Drivers &amp; APK &rarr; Install</b>. It runs <code>adb install</code> automatically.</p>
<p><b>Manually:</b> download <code>Telescope.apk</code> from the latest release and run
<code>adb install Telescope.apk</code>, or sideload it from your phone's file manager
with "Install unknown apps" enabled.</p>

<h3>2. Set up the desktop app</h3>
<p>On first launch, click <b>Setup Drivers &amp; APK</b> to load the v4l2loopback kernel module
(Linux) or register the UnityCapture driver (Windows) if not already active. On Linux, the
"Keep this config after reboot" toggle in that dialog can additionally write the same module
config to <code>/etc/modprobe.d/</code> and <code>/etc/modules-load.d/</code> so it survives a
reboot - this is opt-in and fully reversible from the same toggle.</p>
<p>For USB mode you also need <code>adb</code> on your PATH and
<a href="https://developer.android.com/studio/debug/dev-options">USB debugging</a>
enabled on your phone.</p>

<h3>3. Connect your phone</h3>
<p><b>Easiest - QR pairing (Wi-Fi mode):</b></p>
<ol>
  <li>On the desktop app, select <b>Wi-Fi</b> mode and click the QR button next to the device selector.</li>
  <li>A QR code appears. In the Telescope app on your phone, tap the scan button in the top-right corner and scan it.</li>
  <li>The phone is added to your device list automatically. Close the pairing dialog.</li>
</ol>
<p><b>Manually:</b></p>
<ol>
  <li>In the Telescope app on your phone, start streaming and note the Wi-Fi URL shown.</li>
  <li>On the desktop app, click the gear button next to the device selector, then <b>Add</b> a device with that IP.</li>
</ol>
<p><b>Then:</b></p>
<ol>
  <li>Open the Telescope app on your phone, pick a camera and resolution, tap <b>Start Streaming</b>.
    <br>Android will prompt to disable battery optimization - allow it so the service isn't killed in the background.
    <br>Once streaming, the status card shows your Wi-Fi and USB URLs. Tap either one to copy it.</li>
  <li>On the desktop app, select your device and connection mode, then press <b>Start Streaming</b>.</li>
  <li>The camera control panel (lens picker, ISO, shutter, white balance, OIS) will populate within ~2 seconds of connecting.</li>
  <li>In OBS (or any other app), select <b>Phone Camera</b> (Linux) or <b>Unity Video Capture</b> (Windows) as your webcam source.</li>
</ol>

<p class="warn"><b>Note:</b> The MJPEG stream is served unencrypted on port 8080 with no authentication.
Anyone on the same local network can view it. On public or shared networks, enable
<b>Local only</b> in the Android app to bind the server to localhost - the stream will
then only be reachable via USB.</p>
"""


class _GuideDialog(QDialog):
    def __init__(self, parent=None):
        super().__init__(parent)
        self.setWindowTitle("Quick Start Guide")
        self.setMinimumSize(680, 680)
        self.setWindowFlag(Qt.WindowType.WindowContextHelpButtonHint, False)
        lay = QVBoxLayout(self)
        browser = QTextBrowser()
        browser.setStyleSheet("QTextBrowser { background-color: #1e2126; color: #e8eaed; border: none; font-size: 15px; }")
        from PyQt6.QtGui import QFont
        f = QFont("sans-serif", 12)
        browser.setFont(f)
        browser.setHtml(_GUIDE_HTML)
        browser.setOpenExternalLinks(True)
        browser.setReadOnly(True)
        lay.addWidget(browser)
        close_row = QHBoxLayout()
        close_row.addStretch()
        close_btn = QPushButton("Close")
        close_btn.clicked.connect(self.accept)
        close_row.addWidget(close_btn)
        lay.addLayout(close_row)


class SetupDialog(QDialog):
    _sig_v4l_result   = pyqtSignal(bool, str)
    _sig_v4l_unload   = pyqtSignal(bool, str)
    _sig_persist_result = pyqtSignal(bool, str)
    _sig_win_checks   = pyqtSignal(bool, bool)
    _sig_uc_done      = pyqtSignal(bool, str)
    _sig_uc_msg       = pyqtSignal(str)
    _sig_apk_done     = pyqtSignal(bool, str)

    def __init__(self, parent=None, on_apply_canvas=None):
        super().__init__(parent)
        self.setWindowTitle("System Setup")
        self.setMinimumWidth(500)
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
        lay = QVBoxLayout(self)
        lay.setSpacing(12)

        if IS_LINUX:
            vc_gb = QGroupBox("Virtual Camera (v4l2loopback)")
            vc_lay = QVBoxLayout(vc_gb)
            self._v4l_lbl = QLabel("Checking...")
            self._v4l_lbl.setObjectName("status_dim")
            self._v4l_lbl.setWordWrap(True)
            self._v4l_lbl.setToolTip(
                f"Virtual camera mapping:\n  Phone Feed: {V4L2_PHONE_DEV}\n  OBS Loopback: {V4L2_OBS_DEV}"
            )
            vc_lay.addWidget(self._v4l_lbl)
            btn_row = QHBoxLayout()
            _base = "padding: 4px 10px; border-radius: 4px;"
            chk_btn   = QPushButton("Check Status")
            chk_btn.setStyleSheet(_base)
            load_btn  = QPushButton("Load Module")
            unload_btn = QPushButton("Unload Module")
            load_btn.setStyleSheet(f"QPushButton {{ background-color: #3a6b4f; {_base} }} QPushButton:hover {{ background-color: #4a8b65; }}")
            unload_btn.setStyleSheet(f"QPushButton {{ background-color: #6b3a3a; {_base} }} QPushButton:hover {{ background-color: #8b4a4a; }}")
            load_btn.setToolTip(_SUDO_HINT)
            unload_btn.setToolTip(_SUDO_HINT)
            chk_btn.clicked.connect(self._v4l_check)
            load_btn.clicked.connect(self._v4l_load)
            unload_btn.clicked.connect(self._v4l_unload)
            btn_row.addWidget(chk_btn)
            btn_row.addWidget(load_btn)
            btn_row.addWidget(unload_btn)
            btn_row.addStretch()
            vc_lay.addLayout(btn_row)

            persist_row = QHBoxLayout()
            self._persist_chk = QCheckBox("Keep this config after reboot")
            self._persist_chk.setToolTip(
                "Writes the same module config to /etc/modprobe.d/ and "
                "/etc/modules-load.d/ so it survives a reboot.\n\n" + _SUDO_HINT
            )
            self._persist_chk.toggled.connect(self._on_persist_toggled)
            persist_row.addWidget(self._persist_chk)
            persist_row.addStretch()
            vc_lay.addLayout(persist_row)

            self._persist_status_lbl = QLabel("")
            self._persist_status_lbl.setObjectName("status_dim")
            self._persist_status_lbl.setWordWrap(True)
            self._persist_status_lbl.setTextInteractionFlags(
                Qt.TextInteractionFlag.TextSelectableByMouse
            )
            self._persist_status_lbl.setVisible(False)
            vc_lay.addWidget(self._persist_status_lbl)

            lay.addWidget(vc_gb)
        else:
            vc_gb = QGroupBox("Virtual Camera (UnityCapture)")
            vc_lay = QHBoxLayout(vc_gb)
            self._uc_status_lbl = QLabel("Checking...")
            self._uc_status_lbl.setObjectName("status_dim")
            self._uc_btn = QPushButton("Install Driver")
            self._uc_btn.setFixedWidth(180)
            self._uc_btn.setStyleSheet("QPushButton { background-color: #3a6b4f; padding: 4px 10px; border-radius: 4px; } QPushButton:hover { background-color: #4a8b65; }")
            self._uc_btn.clicked.connect(self._install_uc)
            vc_lay.addWidget(self._uc_status_lbl, 1)
            vc_lay.addWidget(self._uc_btn)
            lay.addWidget(vc_gb)

            adb_gb = QGroupBox("ADB (USB Mode)")
            adb_lay = QHBoxLayout(adb_gb)
            self._adb_status_lbl = QLabel("Checking...")
            self._adb_status_lbl.setObjectName("status_dim")
            adb_lay.addWidget(self._adb_status_lbl)
            lay.addWidget(adb_gb)

        apk_gb = QGroupBox("Install Phone App (via USB)")
        apk_lay = QHBoxLayout(apk_gb)
        _apk = bundled_apk_path()
        self._apk_status_lbl = QLabel("Telescope.apk found" if _apk else "No APK found next to app")
        self._apk_status_lbl.setObjectName("status_ok" if _apk else "status_dim")
        self._apk_status_lbl.setWordWrap(True)
        self._apk_btn = QPushButton("Install" if _apk else "Choose APK...")
        self._apk_btn.setFixedWidth(150)
        self._apk_btn.clicked.connect(self._install_apk)
        apk_lay.addWidget(self._apk_status_lbl, 1)
        apk_lay.addWidget(self._apk_btn)
        lay.addWidget(apk_gb)

        # ── Advanced ──────────────────────────────────────────────────────────
        adv_gb = QGroupBox("Advanced")
        adv_lay = QVBoxLayout(adv_gb)
        adv_lay.setSpacing(8)

        canvas_row = QHBoxLayout()
        canvas_lbl = QLabel("Virtual Camera Canvas")
        canvas_lbl.setObjectName("dim")
        canvas_row.addWidget(canvas_lbl)
        self._canvas_combo = NoScrollComboBox()
        self._canvas_combo.addItems(_PRESET_LABELS)
        self._canvas_combo.setMinimumWidth(280)
        self._canvas_combo.currentTextChanged.connect(self._on_preset_changed)
        canvas_row.addWidget(self._canvas_combo)
        canvas_row.addStretch()
        adv_lay.addLayout(canvas_row)

        # Custom W x H spinboxes (hidden unless "Custom..." selected)
        self._custom_widget = QWidget()
        custom_lay = QHBoxLayout(self._custom_widget)
        custom_lay.setContentsMargins(0, 0, 0, 0)
        custom_lay.setSpacing(6)
        custom_lay.addWidget(QLabel("Width"))
        self._custom_w = QSpinBox()
        self._custom_w.setRange(64, 7680)
        self._custom_w.setValue(1920)
        self._custom_w.setSuffix(" px")
        self._custom_w.setFixedWidth(100)
        custom_lay.addWidget(self._custom_w)
        custom_lay.addWidget(QLabel("Height"))
        self._custom_h = QSpinBox()
        self._custom_h.setRange(64, 4320)
        self._custom_h.setValue(1080)
        self._custom_h.setSuffix(" px")
        self._custom_h.setFixedWidth(100)
        custom_lay.addWidget(self._custom_h)
        custom_lay.addStretch()
        self._custom_widget.setVisible(False)
        adv_lay.addWidget(self._custom_widget)

        if IS_LINUX:
            warn_lbl = QLabel(
                "Applying a new canvas will stop the stream, unload v4l2loopback, "
                "and reload it. Close OBS and any other app using the virtual camera first."
            )
            warn_lbl.setObjectName("status_warn")
            warn_lbl.setWordWrap(True)
            adv_lay.addWidget(warn_lbl)
            apply_label = "Apply && Restart Loopback"
            apply_tooltip = _SUDO_HINT
        else:
            note_lbl = QLabel(
                "Applying will stop and restart the stream with the new canvas size. "
                "If OBS loses the source, remove and re-add it after applying."
            )
            note_lbl.setObjectName("status_dim")
            note_lbl.setWordWrap(True)
            adv_lay.addWidget(note_lbl)
            apply_label = "Apply Canvas"
            apply_tooltip = None

        apply_row = QHBoxLayout()
        self._canvas_apply_btn = QPushButton(apply_label)
        if apply_tooltip:
            self._canvas_apply_btn.setToolTip(apply_tooltip)
        self._canvas_apply_btn.clicked.connect(self._apply_canvas)
        apply_row.addWidget(self._canvas_apply_btn)
        apply_row.addStretch()
        adv_lay.addLayout(apply_row)

        self._canvas_status_lbl = QLabel("")
        self._canvas_status_lbl.setObjectName("status_dim")
        self._canvas_status_lbl.setWordWrap(True)
        self._canvas_status_lbl.setVisible(False)
        adv_lay.addWidget(self._canvas_status_lbl)

        lay.addWidget(adv_gb)

        close_row = QHBoxLayout()
        close_row.addStretch()
        close_btn = QPushButton("Close")
        close_btn.clicked.connect(self.accept)
        close_row.addWidget(close_btn)
        lay.addLayout(close_row)

    def _on_preset_changed(self, label: str):
        self._custom_widget.setVisible(label == "Custom...")

    def _apply_canvas(self):
        w, h = self._get_selected_dims()
        self._canvas_apply_btn.setEnabled(False)
        self._canvas_status_lbl.setObjectName("status_dim")
        self._canvas_status_lbl.setText("Reloading loopback...")
        self._canvas_status_lbl.setStyleSheet("")
        self._canvas_status_lbl.setVisible(True)
        if self._on_apply_canvas:
            self._on_apply_canvas(w, h)

    def set_canvas_apply_result(self, ok: bool, msg: str):
        """Called from SetupPlugin once the reload completes."""
        self._canvas_apply_btn.setEnabled(True)
        if ok:
            self._canvas_status_lbl.setObjectName("status_ok")
            self._canvas_status_lbl.setText("Done - loopback reloaded successfully.")
        else:
            self._canvas_status_lbl.setObjectName("status_err")
            if "in use" in msg.lower():
                self._canvas_status_lbl.setText(
                    "Failed: module is still in use. Close OBS and any other app "
                    "using the virtual camera, then try again."
                )
            else:
                self._canvas_status_lbl.setText(f"Failed: {msg}")
        self._canvas_status_lbl.setStyleSheet("")
        self._canvas_status_lbl.setVisible(True)

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
            self._v4l_lbl.setObjectName("status_ok")
            self._v4l_lbl.setText(f"Ready: {V4L2_PHONE_DEV} + {V4L2_OBS_DEV}")
        elif v4l2_module_loaded():
            self._v4l_lbl.setObjectName("status_warn")
            self._v4l_lbl.setText(f"Module loaded but {V4L2_PHONE_DEV} not found - another config active")
        else:
            self._v4l_lbl.setObjectName("status_err")
            self._v4l_lbl.setText("Not loaded - click Load Module")
        self._v4l_lbl.setStyleSheet("")

    def _v4l_load(self):
        self._v4l_lbl.setObjectName("status_dim")
        self._v4l_lbl.setText("Loading...")
        self._v4l_lbl.setStyleSheet("")
        threading.Thread(target=lambda: self._sig_v4l_result.emit(*v4l2_load()), daemon=True).start()

    def _v4l_unload(self):
        self._v4l_lbl.setObjectName("status_dim")
        self._v4l_lbl.setText("Unloading...")
        self._v4l_lbl.setStyleSheet("")
        threading.Thread(target=lambda: self._sig_v4l_unload.emit(*v4l2_unload()), daemon=True).start()

    def _on_v4l_result(self, ok: bool, msg: str):
        self._v4l_lbl.setText(("Loaded - " if ok else "Failed - ") + msg)
        self._v4l_lbl.setObjectName("status_ok" if ok else "status_err")
        self._v4l_lbl.setStyleSheet("")

    def _on_v4l_unload_result(self, ok: bool, msg: str):
        self._v4l_lbl.setText(("Unloaded - " if ok else "Failed - ") + msg)
        self._v4l_lbl.setObjectName("status_ok" if ok else "status_err")
        self._v4l_lbl.setStyleSheet("")

    # ── v4l2 persistence ─────────────────────────────────────────────────────

    def _refresh_persist_status(self):
        status = v4l2_persist_status()
        persisted = status["modprobe_conf"] or status["modules_load_conf"]
        self._persist_chk.blockSignals(True)
        self._persist_chk.setChecked(persisted)
        self._persist_chk.blockSignals(False)

    def _on_persist_toggled(self, checked: bool):
        self._persist_chk.setEnabled(False)
        self._persist_status_lbl.setObjectName("status_dim")
        self._persist_status_lbl.setText("Working...")
        self._persist_status_lbl.setStyleSheet("padding-bottom: 4px;")
        self._persist_status_lbl.setVisible(True)
        self.adjustSize()
        action = v4l2_persist_enable if checked else v4l2_persist_disable
        threading.Thread(target=lambda: self._sig_persist_result.emit(*action()), daemon=True).start()

    def _on_persist_result(self, ok: bool, msg: str):
        self._persist_chk.setEnabled(True)
        if not ok:
            # Revert the checkbox without re-triggering the write/remove action.
            self._persist_chk.blockSignals(True)
            self._persist_chk.setChecked(not self._persist_chk.isChecked())
            self._persist_chk.blockSignals(False)
        self._persist_status_lbl.setObjectName("status_ok" if ok else "status_err")
        self._persist_status_lbl.setText(msg)
        self._persist_status_lbl.setStyleSheet("padding-bottom: 4px;")
        self._persist_status_lbl.setVisible(True)
        self.adjustSize()

    # ── Windows ───────────────────────────────────────────────────────────────

    def _check_win_setup(self):
        self._sig_win_checks.emit(uc_is_registered(), adb_available())

    def _on_win_checks(self, uc_ok: bool, adb_ok: bool):
        if uc_ok:
            self._uc_status_lbl.setObjectName("status_ok")
            self._uc_status_lbl.setText("Ready")
            self._uc_btn.setText("Reinstall")
        else:
            self._uc_status_lbl.setObjectName("status_err")
            self._uc_status_lbl.setText("Not installed")
            dlls = (unitycapture_dir() / "UnityCaptureFilter64.dll").exists()
            self._uc_btn.setText("Install" if dlls else "Download and Install")
        self._uc_status_lbl.setStyleSheet("")
        if adb_ok:
            self._adb_status_lbl.setObjectName("status_ok")
            self._adb_status_lbl.setText("Ready")
        else:
            self._adb_status_lbl.setObjectName("status_err")
            self._adb_status_lbl.setText("Not found - USB mode unavailable")
        self._adb_status_lbl.setStyleSheet("")

    def _install_uc(self):
        self._uc_btn.setEnabled(False)
        self._uc_status_lbl.setObjectName("status_dim")
        self._uc_status_lbl.setStyleSheet("")

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
            self._uc_status_lbl.setObjectName("status_ok")
            self._uc_status_lbl.setText("Ready")
            self._uc_btn.setText("Reinstall")
        else:
            self._uc_status_lbl.setObjectName("status_err")
            self._uc_status_lbl.setText(f"Failed: {msg}")
            self._uc_btn.setText("Retry")
        self._uc_status_lbl.setStyleSheet("")

    # ── APK ───────────────────────────────────────────────────────────────────

    def _install_apk(self):
        if not adb_available():
            self._apk_status_lbl.setObjectName("status_err")
            self._apk_status_lbl.setText("adb not found - install Android platform-tools first")
            self._apk_status_lbl.setStyleSheet("")
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

        serials = adb_devices()
        if not serials:
            self._apk_status_lbl.setObjectName("status_err")
            self._apk_status_lbl.setText("No authorized ADB device found")
            self._apk_status_lbl.setStyleSheet("")
            return
        serial = serials[0]
        if len(serials) > 1:
            serial, ok = QInputDialog.getItem(
                self, "Select device",
                "Multiple ADB devices/emulators are connected.\nChoose which one to install to:",
                serials, 0, False,
            )
            if not ok:
                return

        self._apk_btn.setEnabled(False)
        self._apk_status_lbl.setObjectName("status_dim")
        self._apk_status_lbl.setText("Installing...")
        self._apk_status_lbl.setStyleSheet("")

        def worker():
            rc, out, err = _run([adb_exe(), "-s", serial, "install", "-r", path], timeout=60)
            output = (out + err).strip()
            if rc == 0 and "Success" in output:
                self._sig_apk_done.emit(True, "Installed successfully")
            else:
                detail = output.splitlines()[-1] if output else "unknown error"
                self._sig_apk_done.emit(False, detail)

        threading.Thread(target=worker, daemon=True).start()

    def _on_apk_done(self, ok: bool, msg: str):
        self._apk_btn.setEnabled(True)
        self._apk_status_lbl.setObjectName("status_ok" if ok else "status_err")
        self._apk_status_lbl.setText(msg)
        self._apk_status_lbl.setStyleSheet("")


class SetupPlugin(TelescopePlugin):
    name = "setup"

    def setup(self, host, bus):
        self._host = host
        self._dlg: Optional[SetupDialog] = None
        self._guide_dlg: Optional[_GuideDialog] = None
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

    def create_panel(self) -> QWidget:
        card = QFrame()
        card.setFrameShape(QFrame.Shape.StyledPanel)
        card.setObjectName("card")
        lay = QVBoxLayout(card)
        lay.setContentsMargins(14, 14, 14, 14)
        lay.setSpacing(10)

        hdr = QHBoxLayout()
        hdr.setContentsMargins(0, 0, 0, 4)
        hdr.setSpacing(8)
        icon_lbl = QLabel()
        icon_lbl.setPixmap(create_vector_icon("gear", "#518cc6").pixmap(18, 18))
        icon_lbl.setFixedSize(18, 18)
        hdr.addWidget(icon_lbl)
        title_lbl = QLabel("System Setup")
        title_lbl.setObjectName("card_title")
        hdr.addWidget(title_lbl)
        hdr.addStretch()
        lay.addLayout(hdr)

        btn_row = QHBoxLayout()
        btn_row.setContentsMargins(0, 0, 0, 0)
        btn_row.setSpacing(8)
        open_btn = QPushButton("Setup Drivers && APK")
        open_btn.clicked.connect(self._open)
        btn_row.addWidget(open_btn)
        guide_btn = QPushButton("Guide")
        guide_btn.setFixedWidth(70)
        guide_btn.clicked.connect(self._open_guide)
        btn_row.addWidget(guide_btn)
        btn_row.addStretch()
        lay.addLayout(btn_row)

        return card

    def _open_guide(self):
        if self._guide_dlg is None or not self._guide_dlg.isVisible():
            self._guide_dlg = _GuideDialog(self._host)
            self._guide_dlg.setWindowModality(Qt.WindowModality.NonModal)
        self._guide_dlg.show()
        self._guide_dlg.raise_()
        self._guide_dlg.activateWindow()

    def _open(self):
        if self._dlg is None:
            self._dlg = SetupDialog(self._host, on_apply_canvas=self._on_apply_canvas)
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
