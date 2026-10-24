"""Set up VA-API: picks a PyAV that has it (the distro's, or one built here) for the next start. See pyav_vaapi."""

import logging
import sys
import threading
from pathlib import Path
from typing import Callable, Optional

from PyQt6.QtCore import Qt, QTimer, pyqtSignal
from PyQt6.QtWidgets import (
    QApplication, QHBoxLayout, QLabel, QPushButton, QSizePolicy, QVBoxLayout, QWidget,
)

from telescope import pyav_vaapi
from telescope.widgets.common import (
    PlacedDialog, card_layout, create_card, dialog_header, dialog_layout, set_status_kind, set_ui_role, ui_px,
)
from telescope.widgets.controls import setting_row
from telescope.i18n import N_, and_list, tr

_WHY_NOT = {
    "old": N_("PyAV {v}. It needs PyAV 14 or newer"),
    "novaapi": N_("PyAV {v}, built without VA-API"),
    "noopen": N_("PyAV {v}, but VA-API didn't open with it"),
}


def _say(line: QLabel, text: str):
    line.setText(text)
    line.setVisible(bool(text))


class VaapiSetupDialog(PlacedDialog):
    _sig_system = pyqtSignal(object)        # (folder, ok, version, why) or None
    _sig_existing = pyqtSignal(bool)
    _sig_line = pyqtSignal(str)
    _sig_built = pyqtSignal(bool, str)

    def __init__(self, parent=None, restart: Optional[Callable[[], bool]] = None):
        super().__init__(parent)
        self.setWindowTitle(tr("Set up VA-API"))
        self.setMinimumWidth(ui_px(520))
        self._restart = restart
        self._system: Optional[tuple] = None
        self._system_checked = False
        self._checks = 0  # background checks still running
        self._building = False
        self._built_ok: Optional[bool] = None
        self._rebuilt_active = False
        self._version = pyav_vaapi.standard_version()
        self._sig_system.connect(self._on_system)
        self._sig_existing.connect(self._on_existing)
        self._sig_line.connect(self._on_line)
        self._sig_built.connect(self._on_built)
        self._build_ui()
        self.check_again()

    def _build_ui(self):
        lay = dialog_layout(self)
        dialog_header(lay, tr("Set up VA-API"),
                      tr("AMD and Intel graphics cards decode through VA-API, which the PyAV that comes with Telescope "
                         "leaves out. Telescope can use one that has it instead."))
        card = create_card()
        card_lay = card_layout(card)

        self._system_btn = QPushButton(tr("Use it"))
        self._system_btn.clicked.connect(self._use_system)
        strip, self._system_line = setting_row(tr("Your distro's PyAV"), tr("Checking..."), self._system_btn)
        card_lay.addWidget(strip)

        self._build_btn = QPushButton(tr("Build"))
        self._build_btn.clicked.connect(self._start_build)
        self._command = QLabel("")
        self._command.setObjectName("banner_details")
        self._command.setWordWrap(True)
        self._command.setTextFormat(Qt.TextFormat.PlainText)
        self._command_text = ""
        self._command.setTextInteractionFlags(Qt.TextInteractionFlag.NoTextInteraction)  # selected, its hyphens would paste wrong
        self._copy_btn = QPushButton(tr("Copy"))
        self._copy_btn.clicked.connect(lambda: QApplication.clipboard().setText(self._command_text))
        self._recheck_btn = QPushButton(tr("Check again"))
        self._recheck_btn.clicked.connect(self.check_again)
        # The command gets the strip's whole width, its buttons under it
        self._tools = QWidget()
        tools_lay = QVBoxLayout(self._tools)
        tools_lay.setContentsMargins(0, 0, 0, 0)
        tools_lay.setSpacing(8)
        tools_lay.addWidget(self._command)
        buttons = QHBoxLayout()
        buttons.setSpacing(8)
        buttons.addStretch(1)
        buttons.addWidget(self._copy_btn)
        buttons.addWidget(self._recheck_btn)
        tools_lay.addLayout(buttons)
        strip, self._build_line = setting_row(tr("Build one here"), "", self._build_btn, self._tools)
        card_lay.addWidget(strip)

        self._standard_btn = QPushButton(tr("Use the standard one"))
        self._standard_btn.clicked.connect(self._use_standard)
        self._using_strip, self._using_line = setting_row(tr("In use"), "", self._standard_btn)
        card_lay.addWidget(self._using_strip)
        card.setSizePolicy(QSizePolicy.Policy.Preferred, QSizePolicy.Policy.Maximum)  # spare height goes below, not into the strips
        lay.addWidget(card)
        lay.addStretch(1)

        self._restart_note = QLabel(tr("Restart Telescope to switch."))
        set_status_kind(self._restart_note, "status_dim")
        self._restart_btn = QPushButton(tr("Restart now"))
        set_ui_role(self._restart_btn, "primary")
        self._restart_btn.clicked.connect(self._do_restart)
        close_btn = QPushButton(tr("Close"))
        close_btn.clicked.connect(self.accept)
        bottom = QHBoxLayout()
        bottom.setSpacing(8)
        bottom.addWidget(self._restart_note)
        bottom.addStretch(1)
        bottom.addWidget(self._restart_btn)
        bottom.addWidget(close_btn)
        lay.addLayout(bottom)

    # ── What there is ─────────────────────────────────────────────────────────

    def check_again(self):
        """Packages may have changed while setup was closed. An existing build job keeps running instead."""
        if self._building or self._checks:
            return
        self._checks = 2
        self._built_ok = None
        self._system = None
        self._system_checked = False
        self._refresh()
        threading.Thread(target=self._check_system, daemon=True).start()
        threading.Thread(target=self._check_existing, daemon=True).start()

    def _check_system(self):
        result = None
        try:
            package = pyav_vaapi.find_system()
            if package is not None:
                folder = pyav_vaapi.link_system(package)
                result = (folder, *pyav_vaapi.probe(folder))
        except Exception:
            logging.exception("Checking the distro's PyAV failed")
        self._sig_system.emit(result)

    def _check_existing(self):
        ok = False
        try:
            folder = self._existing_folder()
            ok = folder is not None and pyav_vaapi.probe(folder)[0]
        except Exception:
            logging.exception("Checking the existing VA-API build failed")
        self._sig_existing.emit(ok)

    def _on_system(self, result):
        self._system, self._system_checked = result, True
        self._checks -= 1
        self._refresh()

    def _on_existing(self, ok: bool):
        self._built_ok = ok
        self._checks -= 1
        self._refresh()

    def _built_folder(self) -> Path:
        return pyav_vaapi.root() / self._version

    def _existing_folder(self) -> Optional[Path]:
        """The build the next start uses: a rebuild waiting for it, else the one there."""
        for folder in (pyav_vaapi.staged(self._built_folder()), self._built_folder()):
            if (folder / "av").is_dir():
                return folder
        return None

    def _refresh(self):
        pick = pyav_vaapi.chosen()
        active = pyav_vaapi.active

        # The distro's
        if self._system is None:
            system_text, system_ok = tr("None installed"), False
            if not self._system_checked:
                system_text = tr("Checking...")
        else:
            _folder, ok, version, why = self._system
            system_ok = ok
            if ok:
                system_text = tr("PyAV {version}, with VA-API", version=version)
            elif why in _WHY_NOT:
                system_text = tr(_WHY_NOT[why], v=version)
            else:
                system_text = tr("Doesn't load in Telescope's Python {version}",
                                 version=f"{sys.version_info.major}.{sys.version_info.minor}")
        _say(self._system_line, system_text)
        self._system_btn.setVisible(system_ok and not (pick and pick["kind"] == pyav_vaapi.SYSTEM))

        # Building one
        exists = self._existing_folder() is not None
        built = exists and self._built_ok is True
        checking = exists and self._built_ok is None
        supported = pyav_vaapi.build_version_supported(self._version)
        missing = [] if built or checking or self._building or not supported else pyav_vaapi.missing_tools()
        command = pyav_vaapi.install_command() if missing else ""
        if self._building:
            pass  # the build's own lines show here
        elif not supported:
            _say(self._build_line, pyav_vaapi.upgrade_needed())
        elif checking:
            _say(self._build_line, tr("Checking the existing build..."))
        elif built:
            _say(self._build_line, tr("PyAV {version} is built", version=self._version))
        elif missing:
            needs = and_list(missing)
            _say(self._build_line, tr("Needs {needs}. To install them:", needs=needs) if command
                 else tr("Needs {needs}. Your distro has them.", needs=needs))
        else:
            _say(self._build_line, tr("The existing build no longer works. Rebuild it against your system's FFmpeg.")
                 if exists else tr("Builds PyAV {version} against your system's FFmpeg, in a minute or two",
                                   version=self._version))
        self._command_text = command
        # Shown with non-breaking hyphens so a package name never splits; Copy gives the plain command
        self._command.setText(command.replace("-", "\u2011"))
        self._tools.setVisible(bool(missing))
        self._command.setVisible(bool(command))
        self._copy_btn.setVisible(bool(command))
        self._build_btn.setText(tr("Use it") if built else tr("Rebuild") if exists else tr("Build"))
        self._build_btn.setVisible(supported and not missing and not checking and not self._building
                                  and not (built and pick and pick["kind"] == pyav_vaapi.BUILD))

        # What's in use
        if active:
            using = (tr("Telescope is using the distro's PyAV {version}", version=active["version"])
                     if active["kind"] == pyav_vaapi.SYSTEM
                     else tr("Telescope is using the built PyAV {version}", version=active["version"]))
        elif pyav_vaapi.problem:
            using = tr("Not in use: {problem}", problem=pyav_vaapi.problem)
        elif pick:
            using = tr("From the next start")
        else:
            using = ""
        _say(self._using_line, using)
        self._using_strip.setVisible(bool(using))
        self._standard_btn.setVisible(pick is not None)

        pending = self._rebuilt_active or (pick or {}).get("path") != (active or {}).get("path")
        self._restart_note.setVisible(pending)
        self._restart_btn.setVisible(pending and self._restart is not None)
        QTimer.singleShot(0, self._fit)

    def _fit(self):
        """As tall as what's showing: Qt's own size doesn't count wrapped lines at the dialog's real width."""
        lay = self.layout()
        if lay is not None and self.isVisible():
            self.resize(self.width(), max(self.minimumHeight(), lay.totalHeightForWidth(self.width())))

    # ── Choosing ──────────────────────────────────────────────────────────────

    def _use_system(self):
        pyav_vaapi.choose(pyav_vaapi.SYSTEM, self._system[0])
        self._refresh()

    def _use_standard(self):
        pyav_vaapi.forget()
        self._refresh()

    def _start_build(self):
        if self._building or not pyav_vaapi.build_version_supported(self._version):
            return
        if self._built_ok and self._existing_folder() is not None:
            pyav_vaapi.choose(pyav_vaapi.BUILD, self._built_folder())
            self._refresh()
            return
        self._building = True
        _say(self._build_line, tr("Starting the build..."))
        self._refresh()
        threading.Thread(target=self._build, daemon=True).start()

    def _build(self):
        try:
            result = pyav_vaapi.build(self._version, self._sig_line.emit)
        except Exception as exc:
            logging.exception("VA-API build failed")
            result = False, str(exc)
        self._sig_built.emit(*result)

    def _on_line(self, line: str):
        if self._building:
            _say(self._build_line, line if len(line) < 90 else line[:87] + "...")

    def _on_built(self, ok: bool, message: str):
        self._building = False
        if ok:
            self._built_ok = True
            self._rebuilt_active |= (pyav_vaapi.active or {}).get("path") == message
            pyav_vaapi.choose(pyav_vaapi.BUILD, Path(message))
            self._refresh()
        else:
            self._refresh()
            _say(self._build_line, tr("The build failed: {message}", message=message))
            self._build_btn.setText(tr("Try again"))

    def _do_restart(self):
        if self._restart and self._restart():
            self.accept()
