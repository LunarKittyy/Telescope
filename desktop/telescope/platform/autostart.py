"""Open Telescope at sign-in: an XDG autostart entry on Linux, the per-user Run key on Windows.

Everything takes its paths as arguments (defaulting to the real ones) so tests can use a temp dir
and a fake winreg.
"""

import os
import sys
from pathlib import Path
from typing import Optional

from telescope.platform import IS_WINDOWS

MINIMIZED = "--minimized"
_RUN_KEY = r"Software\Microsoft\Windows\CurrentVersion\Run"
_VALUE = "Telescope"


def app_dir() -> Path:
    """The folder with TelescopeDesktop.exe, or the one with main.py and start.sh."""
    if getattr(sys, "frozen", False):
        return Path(sys.executable).resolve().parent
    return Path(__file__).resolve().parent.parent.parent


def launch_command(directory: Optional[Path] = None) -> list:
    """How to start this copy of Telescope, minimized to the tray."""
    directory = directory or app_dir()
    if getattr(sys, "frozen", False):
        return [str(Path(sys.executable).resolve()), MINIMIZED]
    start_sh = directory / "start.sh"
    if not IS_WINDOWS and start_sh.exists():
        return [str(start_sh), MINIMIZED]  # keeps its venv current, like launching it by hand
    python = Path(sys.executable)
    if IS_WINDOWS and (python.parent / "pythonw.exe").exists():
        python = python.parent / "pythonw.exe"  # no console window at sign-in
    return [str(python), str(directory / "main.py"), MINIMIZED]


# ── Linux ─────────────────────────────────────────────────────────────────────

def desktop_file(config_home: Optional[Path] = None) -> Path:
    config_home = config_home or Path(os.environ.get("XDG_CONFIG_HOME") or Path.home() / ".config")
    return config_home / "autostart" / "telescope.desktop"


def _exec_arg(arg: str) -> str:
    """Quote one argument for a desktop entry's Exec key (the spec's rules, not a shell's)."""
    if not any(c in arg for c in ' \t\n"\'\\><~|&;$*?#()`'):
        return arg
    escaped = "".join("\\" + c if c in '"`$\\' else c for c in arg)
    return f'"{escaped}"'


def desktop_entry(command: list) -> str:
    exec_line = " ".join(_exec_arg(a).replace("%", "%%") for a in command)
    return (
        "[Desktop Entry]\n"
        "Type=Application\n"
        "Name=Telescope\n"
        "Comment=Phone camera as a webcam\n"
        f"Exec={exec_line}\n"
        "Terminal=false\n"
        "X-GNOME-Autostart-enabled=true\n"
    )


# ── Both ──────────────────────────────────────────────────────────────────────

def is_enabled(config_home: Optional[Path] = None, winreg=None) -> bool:
    if IS_WINDOWS:
        winreg = winreg or _winreg()
        try:
            with winreg.OpenKey(winreg.HKEY_CURRENT_USER, _RUN_KEY) as key:
                winreg.QueryValueEx(key, _VALUE)
            return True
        except OSError:
            return False
    return desktop_file(config_home).exists()


def enable(command: Optional[list] = None, config_home: Optional[Path] = None, winreg=None) -> tuple:
    command = command or launch_command()
    try:
        if IS_WINDOWS:
            winreg = winreg or _winreg()
            line = " ".join(f'"{a}"' if " " in a else a for a in command)
            with winreg.CreateKey(winreg.HKEY_CURRENT_USER, _RUN_KEY) as key:
                winreg.SetValueEx(key, _VALUE, 0, winreg.REG_SZ, line)
        else:
            path = desktop_file(config_home)
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(desktop_entry(command))
    except OSError as exc:
        return False, f"Couldn't set Telescope to open at sign-in: {exc}"
    return True, ""


def disable(config_home: Optional[Path] = None, winreg=None) -> tuple:
    try:
        if IS_WINDOWS:
            winreg = winreg or _winreg()
            with winreg.OpenKey(winreg.HKEY_CURRENT_USER, _RUN_KEY, 0, winreg.KEY_SET_VALUE) as key:
                winreg.DeleteValue(key, _VALUE)
        else:
            desktop_file(config_home).unlink(missing_ok=True)
    except FileNotFoundError:
        pass
    except OSError as exc:
        return False, f"Couldn't stop Telescope opening at sign-in: {exc}"
    return True, ""


def _winreg():
    import winreg
    return winreg
