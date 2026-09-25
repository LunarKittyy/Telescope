"""First-run checklist on the video stage: virtual camera, phone app, add a phone, start streaming.

It shows until the first stream has delivered frames, then gets out of the way for good.
The other plugins are reached only through the EventBus: phones_changed tells it when a phone is
paired, add_phone_requested opens pairing, and setup_needed lets the video stage make room.
"""

import threading
from typing import Optional

from PyQt6.QtCore import QObject, Qt, pyqtSignal
from PyQt6.QtWidgets import QGridLayout, QLabel, QVBoxLayout, QWidget

from telescope.platform import IS_LINUX, _run, adb_available, adb_devices, adb_exe, bundled_apk_path
from telescope.platform.linux import v4l2_module_installed
from telescope.platform.windows import (
    download_unitycapture, register_unitycapture, uc_is_registered, unitycapture_dir,
)
from telescope.plugin import TelescopePlugin
from telescope.widgets.common import (
    WrapLabel, action_button, add_card_header, card_layout, create_card, set_status_kind,
    set_ui_role,
)
from telescope.widgets.qr import QRCodeWidget

APK_URL = "https://github.com/LunarKittyy/Telescope/releases/download/nightly/Telescope.apk"

_LINUX_INSTALL_HINT = (
    "Install the v4l2loopback package. Fedora and Nobara: "
    "v4l2loopback (from RPM Fusion). Debian, Ubuntu and Arch: v4l2loopback-dkms."
)


class _Signals(QObject):
    vcam = pyqtSignal(bool, str)        # ready, detail
    apk = pyqtSignal(bool, str)         # installed, detail


class _Step:
    """One checklist row: badge, title + text (+ extra), and an action button on the right."""

    def __init__(self, grid: QGridLayout, row: int, number: int, title: str):
        self.number = number
        self.badge = QLabel(str(number))
        self.badge.setObjectName("step_badge")
        self.title = QLabel(title)
        self.title.setObjectName("step_title")
        self.text = WrapLabel("")
        set_status_kind(self.text, "status_dim")
        self.body = QVBoxLayout()
        self.body.setContentsMargins(0, 3, 0, 0)
        self.body.setSpacing(4)
        self.body.addWidget(self.title)
        self.body.addWidget(self.text)
        self.button = action_button("")
        self.button.setVisible(False)
        grid.addWidget(self.badge, row, 0, Qt.AlignmentFlag.AlignTop)
        grid.addLayout(self.body, row, 1)
        grid.addWidget(self.button, row, 2, Qt.AlignmentFlag.AlignTop)

    def set(self, done: bool, text: str, kind: str = "status_dim"):
        self.badge.setObjectName("step_badge_done" if done else "step_badge")
        self.badge.setText("✓" if done else str(self.number))
        self.badge.style().unpolish(self.badge)
        self.badge.style().polish(self.badge)
        self.text.setText(text)
        set_status_kind(self.text, "status_ok" if done and kind == "status_dim" else kind)

    def action(self, label: Optional[str], primary: bool = False):
        self.button.setVisible(bool(label))
        if label:
            self.button.setText(label)
            set_ui_role(self.button, "primary" if primary else "")


class OnboardingPlugin(TelescopePlugin):
    name = "onboarding"
    panel_region = "center"

    def setup(self, host, bus):
        self._host = host
        self._bus = bus
        self._phones = 0
        self._vcam_ready = False
        self._vcam_checked = False
        self._streaming = False
        self._busy = False
        self._streamed = False  # a stream has delivered frames at least once; persisted
        self._signals = _Signals()
        self._signals.vcam.connect(self._on_vcam)
        self._signals.apk.connect(self._on_apk)
        bus.phones_changed.connect(self._on_phones)
        bus.stream_started.connect(lambda _url: self._set_streaming(True))
        bus.stream_stopped.connect(lambda: self._set_streaming(False))
        bus.stream_connected.connect(self._on_stream_connected)

    # ── UI ────────────────────────────────────────────────────────────────

    def create_panel(self) -> QWidget:
        self._card = create_card()
        lay = card_layout(self._card)
        add_card_header(lay, "Get set up", "check")

        grid = QGridLayout()
        grid.setContentsMargins(0, 4, 0, 0)
        grid.setHorizontalSpacing(14)
        grid.setVerticalSpacing(22)
        grid.setColumnStretch(1, 1)
        self._vcam = _Step(grid, 0, 1, "Virtual camera")
        self._vcam.button.clicked.connect(self._vcam_action)
        self._app = _Step(grid, 1, 2, "Phone app")
        self._app.button.clicked.connect(self._install_apk)
        self._qr_row = QWidget()
        qr_lay = QVBoxLayout(self._qr_row)
        qr_lay.setContentsMargins(0, 4, 0, 0)
        qr_lay.addWidget(QRCodeWidget(APK_URL, module_px=3), 0, Qt.AlignmentFlag.AlignLeft)
        self._app.body.addWidget(self._qr_row)
        self._pair = _Step(grid, 2, 3, "Add your phone")
        self._pair.button.clicked.connect(self._bus.add_phone_requested.emit)
        self._start = _Step(grid, 3, 4, "Start streaming")
        lay.addLayout(grid)
        lay.addStretch(1)

        self._render()
        self.check_virtual_camera()
        return self._card

    def _render(self):
        vcam = self._vcam
        if not self._vcam_checked:
            vcam.set(False, "Checking…")
            vcam.action(None)
        elif self._vcam_ready:
            vcam.set(True, "Installed. Starting a stream may ask for your password to switch it on."
                     if IS_LINUX else "Installed.")
            vcam.action(None)
        elif IS_LINUX:
            vcam.set(False, _LINUX_INSTALL_HINT, "status_warn")
            vcam.action("Check again")
        else:
            vcam.set(False, "Other apps see Telescope as a webcam through this driver. "
                            "Installing it needs admin access.")
            vcam.action("Install driver", primary=True)

        paired = self._phones > 0
        if paired:
            self._app.set(True, "Installed.")
            self._app.action(None)
        elif not self._busy:
            self._app.set(False, "Scan this with the phone's camera to download Telescope.apk, "
                                 "then open it to install.")
            usb = bundled_apk_path() is not None and adb_available()
            self._app.action("Install over USB" if usb else None)
        self._qr_row.setVisible(not paired)

        if paired:
            self._pair.set(True, "Paired.")
            self._pair.action(None)
        else:
            self._pair.set(False, "Open Telescope on the phone first.")
            self._pair.action("Add phone", primary=True)

        if self._streamed:
            self._start.set(True, "Done.")
        else:
            # Points at the real button rather than duplicating it, so people learn where it lives.
            self._start.set(False, "Click Start Streaming in the top right corner of this window.")
        self._update_visibility()

    def _update_visibility(self):
        needed = not self._streaming and not (self._phones > 0 and self._vcam_ready and self._streamed)
        if self._card.isHidden() != (not needed):
            self._card.setVisible(needed)
        self._bus.setup_needed.emit(needed)

    # ── Events ────────────────────────────────────────────────────────────

    def _on_phones(self, count: int):
        self._phones = count
        self._render()

    def _on_stream_connected(self):
        if not self._streamed:
            self._streamed = True
            self._host.schedule_save()
            self._render()

    def _set_streaming(self, streaming: bool):
        self._streaming = streaming
        self._update_visibility()

    # ── Virtual camera ────────────────────────────────────────────────────

    def check_virtual_camera(self):
        signals = self._signals

        def work():
            ready = v4l2_module_installed() if IS_LINUX else uc_is_registered()
            signals.vcam.emit(ready, "")
        threading.Thread(target=work, daemon=True).start()

    def _vcam_action(self):
        if IS_LINUX:
            self._vcam_checked = False
            self._render()
            self.check_virtual_camera()
            return
        self._vcam.button.setEnabled(False)
        self._vcam.set(False, "Installing… Windows asks for admin access.")
        signals = self._signals

        def work():
            ok, msg = True, ""
            if not (unitycapture_dir() / "UnityCaptureFilter64.dll").exists():
                ok, msg = download_unitycapture()
            if ok:
                ok, msg = register_unitycapture()
            signals.vcam.emit(ok and uc_is_registered(), "" if ok else msg)
        threading.Thread(target=work, daemon=True).start()

    def _on_vcam(self, ready: bool, detail: str):
        self._vcam_checked = True
        self._vcam_ready = ready
        self._vcam.button.setEnabled(True)
        self._render()
        if detail and not ready:
            self._vcam.set(False, f"Didn't install: {detail}", "status_err")
            self._vcam.action("Try again", primary=True)

    # ── Phone app over USB ────────────────────────────────────────────────

    def _install_apk(self):
        apk = bundled_apk_path()
        if apk is None:
            return
        self._busy = True
        self._app.button.setEnabled(False)
        self._app.set(False, "Installing over USB…")
        signals = self._signals

        def work():
            serials = adb_devices()
            if not serials:
                signals.apk.emit(False, "No phone found over USB. Plug it in and allow USB debugging "
                                        "when the phone asks.")
                return
            rc, out, err = _run([adb_exe(), "-s", serials[0], "install", "-r", str(apk)], timeout=120)
            output = (out + err).strip()
            if rc == 0 and "Success" in output:
                signals.apk.emit(True, "")
            else:
                signals.apk.emit(False, output.splitlines()[-1] if output else "adb install failed")
        threading.Thread(target=work, daemon=True).start()

    def _on_apk(self, ok: bool, detail: str):
        self._busy = False
        self._app.button.setEnabled(True)
        self._render()
        if ok:
            self._app.set(False, "Installed. Open Telescope on the phone for the next step.", "status_ok")
        else:
            self._app.set(False, detail, "status_err")

    def get_config(self) -> dict:
        return {"streamed": self._streamed}

    def set_config(self, cfg: dict):
        self._streamed = cfg.get("streamed") is True
        self._render()
