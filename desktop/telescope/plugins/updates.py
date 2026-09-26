"""Update check and one-click update for the desktop app (telescope/updates.py does the work).

Checks a few seconds after launch and then once a day, silently: a newer build shows an "Update"
button beside the settings button, nothing more. Updating downloads, verifies, replaces the app's
files and restarts it; it waits while a stream is running. A copy that can't replace itself (a source
checkout, a read-only folder) gets a link to the release page instead.
"""

import logging
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path
from typing import Optional

from PyQt6.QtCore import QObject, QSize, Qt, QTimer, QUrl, pyqtSignal
from PyQt6.QtGui import QAction, QDesktopServices
from PyQt6.QtWidgets import QCheckBox, QDialog, QPushButton, QWidget

from telescope import updates, version
from telescope.platform import stop_adb_server
from telescope.plugin import TelescopePlugin
from telescope.widgets.common import (
    NoScrollComboBox, add_card_header, button_row, card_action, card_layout, control_row,
    create_card, create_vector_icon, dialog_buttons, dialog_header, dialog_layout, set_status_kind,
    set_ui_role, ui_px, wrapped_note,
)

logger = logging.getLogger(__name__)

_FIRST_CHECK_MS = 8_000
_TICK_MS = 60 * 60 * 1000       # how often to see whether a day has passed
_CHECK_INTERVAL_S = 24 * 60 * 60
_CHANNEL_LABELS = (("stable", "Stable"), ("nightly", "Nightly"))
RELEASES_URL = f"https://github.com/{version.REPO}/releases"


class _Signals(QObject):
    checked = pyqtSignal(int, object, str)   # check id, Manifest or None, error text
    progress = pyqtSignal(int, int)          # bytes done, total
    installed = pyqtSignal(object, str)      # InstallResult or None, error text


class UpdatesDialog(QDialog):
    def __init__(self, plugin: "UpdatesPlugin", parent=None):
        super().__init__(parent)
        self._plugin = plugin
        self.setWindowTitle("Updates")
        self.setWindowFlag(Qt.WindowType.WindowContextHelpButtonHint, False)
        self.setMinimumWidth(ui_px(580))
        lay = dialog_layout(self)
        dialog_header(lay, "Updates")

        card = create_card()
        c = card_layout(card)
        self._check_btn = card_action("Check now", "reset")
        self._check_btn.clicked.connect(lambda: plugin.check(manual=True))
        add_card_header(c, "Telescope", "update", action=self._check_btn)
        self._version_lbl = wrapped_note(version.display_version(), "val")
        c.addLayout(control_row("This version", self._version_lbl, stretch=True))

        self._channel = NoScrollComboBox()
        for key, label in _CHANNEL_LABELS:
            self._channel.addItem(label, key)
        self._channel.setToolTip("Nightly is built from every change and gets fixes first, with less testing.")
        self._channel.currentIndexChanged.connect(
            lambda i: plugin.set_channel(self._channel.itemData(i)))
        c.addLayout(control_row("Channel", self._channel, stretch=True))

        self._auto = QCheckBox("On")
        self._auto.toggled.connect(plugin.set_auto_check)
        c.addLayout(control_row("Check daily", self._auto))

        self._status = wrapped_note("")
        c.addLayout(control_row("", self._status, stretch=True))

        self._update_btn = QPushButton("Update and restart")
        set_ui_role(self._update_btn, "primary")
        self._update_btn.clicked.connect(plugin.update_now)
        self._notes_btn = QPushButton("What's new")
        self._notes_btn.clicked.connect(plugin.open_notes)
        self._actions = QWidget()
        self._actions.setLayout(button_row(self._notes_btn, self._update_btn))
        c.addLayout(control_row("", self._actions, stretch=True))
        lay.addWidget(card)

        close_btn = QPushButton("Close")
        close_btn.clicked.connect(self.accept)
        dialog_buttons(lay, close_btn)
        self.refresh()

    def refresh(self):
        p = self._plugin
        self._channel.blockSignals(True)
        self._channel.setCurrentIndex(self._channel.findData(p.channel))
        self._channel.blockSignals(False)
        self._auto.blockSignals(True)
        self._auto.setChecked(p.auto_check)
        self._auto.blockSignals(False)
        text, kind = p.status_text()
        self._status.setText(text)
        set_status_kind(self._status, kind)
        busy = p.busy
        available = p.available is not None
        self._check_btn.setEnabled(not busy)
        self._channel.setEnabled(not busy)
        self._actions.setVisible(available)
        blocker = updates.self_update_blocker() if available else None
        self._update_btn.setText("Open download page" if blocker else "Update and restart")
        self._update_btn.setEnabled(not busy and (blocker is not None or not p.host_streaming()))


class UpdatesPlugin(TelescopePlugin):
    name = "updates"
    header_side = "right"

    def setup(self, host, bus):
        self._host = host
        self._bus = bus
        self.channel = updates.default_channel()
        self.auto_check = True
        self._last_check = 0.0
        self.available: Optional[updates.Manifest] = None
        self.latest: Optional[updates.Manifest] = None  # the last manifest seen, newer or not
        self._checked = False
        self._error = ""
        self._check_id = 0
        self._checking = False
        self._manual = False
        self._phase = ""          # "", "downloading", "installing"
        self._progress = (0, 0)
        self._cancel = threading.Event()
        self._dlg: Optional[UpdatesDialog] = None
        self._signals = _Signals()
        self._signals.checked.connect(self._on_checked)
        self._signals.progress.connect(self._on_progress)
        self._signals.installed.connect(self._on_installed)
        bus.update_requested.connect(self.open_dialog)
        bus.stream_started.connect(lambda _url: self._refresh())
        bus.stream_stopped.connect(self._refresh)

        self._timer = QTimer()
        self._timer.timeout.connect(self._maybe_auto_check)
        self._timer.start(_TICK_MS)
        QTimer.singleShot(_FIRST_CHECK_MS, self._maybe_auto_check)

    # ── UI ────────────────────────────────────────────────────────────────

    def create_header_widget(self) -> QWidget:
        # Green, not the accent blue: Start Streaming stays the one primary button in the header.
        self._header_btn = QPushButton("Update")
        self._header_btn.setFixedHeight(36)
        self._header_btn.setIcon(create_vector_icon("update", "#ffffff"))
        self._header_btn.setIconSize(QSize(16, 16))
        set_ui_role(self._header_btn, "success")
        self._header_btn.clicked.connect(self.open_dialog)
        self._header_btn.setVisible(False)
        return self._header_btn

    def create_menu_actions(self) -> list:
        action = QAction("Updates…", None)
        action.triggered.connect(self.open_dialog)
        return [action]

    def open_dialog(self):
        if self._dlg is None:
            self._dlg = UpdatesDialog(self, self._host)
        self._dlg.refresh()
        self._dlg.show()
        self._dlg.raise_()
        self._dlg.activateWindow()
        if not self._checked and not self._checking:
            self.check(manual=True)

    def _refresh(self):
        if hasattr(self, "_header_btn"):
            self._header_btn.setVisible(self.available is not None)
            if self.available is not None:
                self._header_btn.setToolTip(f"Telescope {self.available.display_version} is available")
        if self._dlg is not None:
            self._dlg.refresh()

    @property
    def busy(self) -> bool:
        return self._checking or bool(self._phase)

    def host_streaming(self) -> bool:
        return self._host.is_streaming()

    def status_text(self) -> tuple:
        if self._phase == "downloading":
            done, total = self._progress
            pct = f" {done * 100 // total}%" if total else ""
            return f"Downloading{pct}…", "status_dim"
        if self._phase == "installing":
            return "Installing…", "status_dim"
        if self._checking:
            return "Checking…", "status_dim"
        if self._error:
            return self._error, "status_err"
        if self.available is not None:
            blocker = updates.self_update_blocker()
            text = f"Telescope {self.available.display_version} is available."
            if blocker:
                return f"{text} {blocker}", "status_warn"
            if self.host_streaming():
                return f"{text} Stop streaming to update.", "status_warn"
            return text, "status_ok"
        if not self._checked:
            return "", "status_dim"
        if version.CHANNEL == "dev":
            latest = f" The latest {self.channel} build is {self.latest.display_version}." if self.latest else ""
            return f"This is a source checkout, so it doesn't update itself.{latest}", "status_dim"
        if self.latest is None:
            return f"There's no {self.channel} release yet.", "status_dim"
        return "You have the latest version.", "status_dim"

    # ── Settings ──────────────────────────────────────────────────────────

    def set_channel(self, channel: str):
        if channel not in updates.CHANNELS or channel == self.channel:
            return
        self.channel = channel
        self.available = None
        self.latest = None
        self._host.schedule_save()
        self.check(manual=True)

    def set_auto_check(self, on: bool):
        self.auto_check = bool(on)
        self._host.schedule_save()

    # ── Checking ──────────────────────────────────────────────────────────

    def _maybe_auto_check(self):
        """Once each launch, then daily while Telescope stays open."""
        if not self.auto_check or version.CHANNEL == "dev" or self.busy:
            return
        if not self._checked or time.time() - self._last_check >= _CHECK_INTERVAL_S:
            self.check(manual=False)

    def check(self, manual: bool = False):
        if self.busy:
            return
        self._check_id += 1
        self._checking = True
        self._manual = manual
        self._error = ""
        self._refresh()
        self._spawn_check(self._check_id, self.channel)

    def _spawn_check(self, check_id: int, channel: str):
        signals = self._signals

        def work():
            try:
                manifest, error = updates.fetch_manifest(channel), ""
            except updates.UpdateError as exc:
                manifest, error = None, str(exc)
            except Exception:
                logger.exception("Update check failed")
                manifest, error = None, "Couldn't check for updates."
            try:
                signals.checked.emit(check_id, manifest, error)
            except RuntimeError:
                pass
        threading.Thread(target=work, daemon=True).start()

    def _on_checked(self, check_id: int, manifest, error: str):
        if check_id != self._check_id:
            return
        self._checking = False
        if error:
            # A background check fails quietly (offline is normal); only a check you asked for says so.
            self._error = error if self._manual else ""
        else:
            self._checked = True
            self._last_check = time.time()
            self._host.schedule_save()
            self.latest = manifest
            newer = updates.is_newer(manifest) and manifest is not None and updates.platform_asset(manifest)
            self.available = manifest if newer else None
        self._refresh()

    # ── Updating ──────────────────────────────────────────────────────────

    def open_notes(self):
        target = self.available or self.latest
        QDesktopServices.openUrl(QUrl(target.notes if target and target.notes else RELEASES_URL))

    def update_now(self):
        manifest = self.available
        if manifest is None or self.busy:
            return
        if updates.self_update_blocker():
            QDesktopServices.openUrl(QUrl(manifest.notes or RELEASES_URL))
            return
        if self._host.is_streaming():
            self._refresh()
            return
        asset = updates.platform_asset(manifest)
        self._phase = "downloading"
        self._progress = (0, asset.size)
        self._error = ""
        self._cancel.clear()
        self._refresh()
        self._spawn_install(asset)

    def _spawn_install(self, asset):
        signals, cancel = self._signals, self._cancel

        def work():
            try:
                archive = updates.download(
                    asset, Path(tempfile.gettempdir()) / "telescope-update",
                    progress=lambda d, t: signals.progress.emit(d, t), cancelled=cancel.is_set)
                signals.progress.emit(-1, -1)  # downloaded: now installing
                stop_adb_server()  # a running adb.exe would keep platform-tools on its old version
                result, error = updates.install(archive), ""
                archive.unlink(missing_ok=True)
            except updates.UpdateError as exc:
                result, error = None, str(exc)
            except Exception:
                logger.exception("Update failed")
                result, error = None, "The update failed."
            try:
                signals.installed.emit(result, error)
            except RuntimeError:
                pass
        threading.Thread(target=work, daemon=True).start()

    def _on_progress(self, done: int, total: int):
        if done < 0:
            self._phase = "installing"
        else:
            self._progress = (done, total)
        self._refresh()

    def _on_installed(self, result, error: str):
        if result is None:
            self._phase = ""
            self._error = error
            self._refresh()
            return
        if result.skipped:
            logger.warning("Files in use kept their old version: %s", ", ".join(result.skipped))
        self._relaunch(result.relaunch)
        self._host.quit_app()

    @staticmethod
    def _relaunch(argv: list):
        kwargs = {"close_fds": True}
        if sys.platform == "win32":
            kwargs["creationflags"] = subprocess.DETACHED_PROCESS | subprocess.CREATE_NEW_PROCESS_GROUP
        else:
            kwargs["start_new_session"] = True
        subprocess.Popen(argv, **kwargs)

    def shutdown(self):
        self._cancel.set()
        self._timer.stop()

    # ── Config ────────────────────────────────────────────────────────────

    def get_config(self) -> dict:
        return {"channel": self.channel, "auto_check": self.auto_check, "last_check": self._last_check}

    def set_config(self, cfg: dict):
        channel = cfg.get("channel")
        self.channel = channel if channel in updates.CHANNELS else updates.default_channel()
        self.auto_check = cfg.get("auto_check") is not False
        last = cfg.get("last_check")
        self._last_check = float(last) if isinstance(last, (int, float)) and not isinstance(last, bool) else 0.0
        self._refresh()
