"""Set up VA-API: picks a PyAV that has it (the distro's, or one built here) for the next start. See pyav_vaapi."""

import sys
import threading
from pathlib import Path
from typing import Callable, Optional

from PyQt6.QtCore import Qt, QTimer, pyqtSignal
from PyQt6.QtWidgets import (
    QApplication, QDialog, QHBoxLayout, QLabel, QPushButton, QSizePolicy, QVBoxLayout, QWidget,
)

from telescope import pyav_vaapi
from telescope.widgets.common import (
    card_layout, create_card, dialog_header, dialog_layout, set_status_kind, set_ui_role, ui_px,
)
from telescope.widgets.controls import setting_row

_WHY_NOT = {
    "old": "PyAV {v}. It needs PyAV 14 or newer",
    "novaapi": "PyAV {v}, built without VA-API",
    "noopen": "PyAV {v}, but VA-API didn't open with it",
}


def _say(line: QLabel, text: str):
    line.setText(text)
    line.setVisible(bool(text))


class VaapiSetupDialog(QDialog):
    _sig_system = pyqtSignal(object)        # (folder, ok, version, why) or None
    _sig_line = pyqtSignal(str)
    _sig_built = pyqtSignal(bool, str)

    def __init__(self, parent=None, restart: Optional[Callable[[], bool]] = None):
        super().__init__(parent)
        self.setWindowTitle("Set up VA-API")
        self.setMinimumWidth(ui_px(520))
        self._restart = restart
        self._system: Optional[tuple] = None
        self._system_checked = False
        self._building = False
        self._version = pyav_vaapi.standard_version()
        self._sig_system.connect(self._on_system)
        self._sig_line.connect(self._on_line)
        self._sig_built.connect(self._on_built)
        self._build_ui()
        self._refresh()
        threading.Thread(target=self._check_system, daemon=True).start()

    def _build_ui(self):
        lay = dialog_layout(self)
        dialog_header(lay, "Set up VA-API",
                      "AMD and Intel graphics cards decode through VA-API, which the PyAV that comes with Telescope "
                      "leaves out. Telescope can use one that has it instead.")
        card = create_card()
        card_lay = card_layout(card)

        self._system_btn = QPushButton("Use it")
        self._system_btn.clicked.connect(self._use_system)
        strip, self._system_line = setting_row("Your distro's PyAV", "Checking...", self._system_btn)
        card_lay.addWidget(strip)

        self._build_btn = QPushButton("Build")
        self._build_btn.clicked.connect(self._start_build)
        self._command = QLabel("")
        self._command.setObjectName("banner_details")
        self._command.setWordWrap(True)
        self._command.setTextFormat(Qt.TextFormat.PlainText)
        self._command_text = ""
        self._command.setTextInteractionFlags(Qt.TextInteractionFlag.NoTextInteraction)  # selected, its hyphens would paste wrong
        self._copy_btn = QPushButton("Copy")
        self._copy_btn.clicked.connect(lambda: QApplication.clipboard().setText(self._command_text))
        self._recheck_btn = QPushButton("Check again")
        self._recheck_btn.clicked.connect(self._refresh)
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
        strip, self._build_line = setting_row("Build one here", "", self._build_btn, self._tools)
        card_lay.addWidget(strip)

        self._standard_btn = QPushButton("Use the standard one")
        self._standard_btn.clicked.connect(self._use_standard)
        self._using_strip, self._using_line = setting_row("In use", "", self._standard_btn)
        card_lay.addWidget(self._using_strip)
        card.setSizePolicy(QSizePolicy.Policy.Preferred, QSizePolicy.Policy.Maximum)  # spare height goes below, not into the strips
        lay.addWidget(card)
        lay.addStretch(1)

        self._restart_note = QLabel("Restart Telescope to switch.")
        set_status_kind(self._restart_note, "status_dim")
        self._restart_btn = QPushButton("Restart now")
        set_ui_role(self._restart_btn, "primary")
        self._restart_btn.clicked.connect(self._do_restart)
        close_btn = QPushButton("Close")
        close_btn.clicked.connect(self.accept)
        bottom = QHBoxLayout()
        bottom.setSpacing(8)
        bottom.addWidget(self._restart_note)
        bottom.addStretch(1)
        bottom.addWidget(self._restart_btn)
        bottom.addWidget(close_btn)
        lay.addLayout(bottom)

    # ── What there is ─────────────────────────────────────────────────────────

    def _check_system(self):
        package = pyav_vaapi.find_system()
        if package is None:
            self._sig_system.emit(None)
            return
        folder = pyav_vaapi.link_system(package)
        self._sig_system.emit((folder, *pyav_vaapi.probe(folder)))

    def _on_system(self, result):
        self._system, self._system_checked = result, True
        self._refresh()

    def _built_folder(self) -> Path:
        return pyav_vaapi.root() / self._version

    def _refresh(self):
        pick = pyav_vaapi.chosen()
        active = pyav_vaapi.active

        # The distro's
        if self._system is None:
            system_text, system_ok = "None installed", False
            if not self._system_checked:
                system_text = "Checking..."
        else:
            _folder, ok, version, why = self._system
            system_ok = ok
            if ok:
                system_text = f"PyAV {version}, with VA-API"
            elif why in _WHY_NOT:
                system_text = _WHY_NOT[why].format(v=version)
            else:
                system_text = f"Doesn't load in Telescope's Python {sys.version_info.major}.{sys.version_info.minor}"
        _say(self._system_line, system_text)
        self._system_btn.setVisible(system_ok and not (pick and pick["kind"] == pyav_vaapi.SYSTEM))

        # Building one
        built = (self._built_folder() / "av").is_dir()
        missing = [] if built or self._building else pyav_vaapi.missing_tools()
        command = pyav_vaapi.install_command() if missing else ""
        if self._building:
            pass  # the build's own lines show here
        elif built:
            _say(self._build_line, f"PyAV {self._version} is built")
        elif missing:
            needs = " and ".join([", ".join(missing[:-1]), missing[-1]] if len(missing) > 1 else missing)
            _say(self._build_line, f"Needs {needs}." + (" To install them:" if command else " Your distro has them."))
        else:
            _say(self._build_line, f"Builds PyAV {self._version} against your system's FFmpeg, in a minute or two")
        self._command_text = command
        # Shown with non-breaking hyphens so a package name never splits; Copy gives the plain command
        self._command.setText(command.replace("-", "\u2011"))
        self._tools.setVisible(bool(missing))
        self._command.setVisible(bool(command))
        self._copy_btn.setVisible(bool(command))
        self._build_btn.setText("Use it" if built else "Build")
        self._build_btn.setVisible(not missing and not self._building and not (pick and pick["kind"] == pyav_vaapi.BUILD))
        self._build_btn.setEnabled(bool(self._version))

        # What's in use
        if active:
            which = "the distro's" if active["kind"] == pyav_vaapi.SYSTEM else "the built"
            using = f"Telescope is using {which} PyAV {active['version']}"
        elif pyav_vaapi.problem:
            using = f"Not in use: {pyav_vaapi.problem}"
        elif pick:
            using = "From the next start"
        else:
            using = ""
        _say(self._using_line, using)
        self._using_strip.setVisible(bool(using))
        self._standard_btn.setVisible(pick is not None)

        pending = (pick or {}).get("path") != (active or {}).get("path")
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
        if (self._built_folder() / "av").is_dir():
            pyav_vaapi.choose(pyav_vaapi.BUILD, self._built_folder())
            self._refresh()
            return
        self._building = True
        _say(self._build_line, "Starting the build...")
        self._refresh()
        threading.Thread(target=lambda: self._sig_built.emit(*pyav_vaapi.build(self._version, self._sig_line.emit)),
                         daemon=True).start()

    def _on_line(self, line: str):
        if self._building:
            _say(self._build_line, line if len(line) < 90 else line[:87] + "...")

    def _on_built(self, ok: bool, message: str):
        self._building = False
        if ok:
            pyav_vaapi.choose(pyav_vaapi.BUILD, Path(message))
            self._refresh()
        else:
            self._refresh()
            _say(self._build_line, f"The build failed: {message}")
            self._build_btn.setText("Try again")

    def _do_restart(self):
        if self._restart and self._restart():
            self.accept()
