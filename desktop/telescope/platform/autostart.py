"""Open Telescope at sign-in: an XDG autostart entry on Linux, the per-user Run key on Windows. Also the Linux
app-menu entry, which points at wherever this copy lives.

Everything takes its paths as arguments (defaulting to the real ones) so tests can use a temp dir
and a fake winreg.
"""

import logging
import os
import sys
from pathlib import Path
from typing import Callable, Optional

from telescope import version
from telescope.platform import IS_WINDOWS

logger = logging.getLogger(__name__)

APP_ID    = "telescope"  # the menu entry's file name, its icon's name, and the window's app id
MINIMIZED = "--minimized"
_RUN_KEY = r"Software\Microsoft\Windows\CurrentVersion\Run"
_VALUE = "Telescope"


def app_dir() -> Path:
    """The folder with TelescopeDesktop.exe, or the one with main.py and start.sh."""
    if getattr(sys, "frozen", False):
        return Path(sys.executable).resolve().parent
    return Path(__file__).resolve().parent.parent.parent


def launch_command(directory: Optional[Path] = None, minimized: bool = True) -> list:
    """How to start this copy of Telescope, minimized to the tray unless minimized is False."""
    directory = directory or app_dir()
    extra = [MINIMIZED] if minimized else []
    if getattr(sys, "frozen", False):
        return [str(Path(sys.executable).resolve())] + extra
    start_sh = directory / "start.sh"
    if not IS_WINDOWS and start_sh.exists():
        return [str(start_sh)] + extra  # keeps its venv current, like launching it by hand
    python = Path(sys.executable)
    if IS_WINDOWS and (python.parent / "pythonw.exe").exists():
        python = python.parent / "pythonw.exe"  # no console window at sign-in
    return [str(python), str(directory / "main.py")] + extra


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


def desktop_entry(command: list, menu: bool = False) -> str:
    exec_line = " ".join(_exec_arg(a).replace("%", "%%") for a in command)
    kind = ("Categories=AudioVideo;Video;\nKeywords=webcam;camera;phone;android;\n" if menu
            else "X-GNOME-Autostart-enabled=true\n")
    return (
        "[Desktop Entry]\n"
        "Type=Application\n"
        "Name=Telescope\n"
        "GenericName=Phone webcam\n"
        "Comment=Phone camera as a webcam\n"
        f"Exec={exec_line}\n"
        f"Icon={APP_ID}\n"
        f"StartupWMClass={APP_ID}\n"
        "Terminal=false\n"
        + kind
    )


def _data_home(data_home: Optional[Path]) -> Path:
    return data_home or Path(os.environ.get("XDG_DATA_HOME") or Path.home() / ".local" / "share")


def menu_file(data_home: Optional[Path] = None) -> Path:
    return _data_home(data_home) / "applications" / f"{APP_ID}.desktop"


def icon_file(data_home: Optional[Path] = None) -> Path:
    return _data_home(data_home) / "icons" / "hicolor" / "256x256" / "apps" / f"{APP_ID}.png"


def is_dev_checkout() -> bool:
    return version.CHANNEL == "dev"


def update_menu_entry(save_icon: Callable[[Path], bool], command: Optional[list] = None,
                      data_home: Optional[Path] = None) -> bool:
    """Linux: point the app menu's Telescope entry at this copy, writing it only when it changed (the folder
    moved, or it's the first run). save_icon(path) writes the icon when it's missing. Returns whether it wrote."""
    text = desktop_entry(command or launch_command(minimized=False), menu=True)
    path = menu_file(data_home)
    icon = icon_file(data_home)
    try:
        if not icon.exists():
            icon.parent.mkdir(parents=True, exist_ok=True)
            save_icon(icon)
        if path.exists() and path.read_text() == text:
            return False
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)
    except OSError as exc:
        logger.info("Couldn't write the app menu entry: %s", exc)
        return False
    return True


def _has_entry(app_id: str, data_home: Optional[Path]) -> bool:
    dirs = [_data_home(data_home)] + [Path(d) for d in (os.environ.get("XDG_DATA_DIRS")
                                                        or "/usr/local/share:/usr/share").split(":") if d]
    return any((d / "applications" / f"{app_id}.desktop").exists() for d in dirs)


def ensure_portal_entry(app_id: str, command: Optional[list] = None, data_home: Optional[Path] = None) -> bool:
    """Linux: make sure there's a menu entry named app_id, which the desktop portal needs before it lets Telescope
    register under that id (for global shortcuts). A copy that writes no menu entry of its own (a dev profile, a
    git checkout) gets a hidden one. Returns whether one is there now."""
    if _has_entry(app_id, data_home):
        return True
    text = desktop_entry(command or launch_command(minimized=False), menu=True) + "NoDisplay=true\n"
    path = _data_home(data_home) / "applications" / f"{app_id}.desktop"
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)
    except OSError as exc:
        logger.info("Couldn't write a menu entry for the desktop portal: %s", exc)
        return False
    return True


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
            line = _run_line(command)
            with winreg.CreateKey(winreg.HKEY_CURRENT_USER, _RUN_KEY) as key:
                winreg.SetValueEx(key, _VALUE, 0, winreg.REG_SZ, line)
        else:
            path = desktop_file(config_home)
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(desktop_entry(command))
    except OSError as exc:
        return False, f"Couldn't set Telescope to open at sign-in: {exc}"
    return True, ""


def _run_line(command: list) -> str:
    return " ".join(f'"{a}"' if " " in a else a for a in command)


def refresh(command: Optional[list] = None, config_home: Optional[Path] = None, winreg=None) -> bool:
    """If opening at sign-in is on but points at another copy (the folder moved), point it at this one."""
    command = command or launch_command()
    try:
        if IS_WINDOWS:
            winreg = winreg or _winreg()
            with winreg.OpenKey(winreg.HKEY_CURRENT_USER, _RUN_KEY) as key:
                current = winreg.QueryValueEx(key, _VALUE)[0]
            if current == _run_line(command):
                return False
        else:
            path = desktop_file(config_home)
            if not path.exists() or path.read_text() == desktop_entry(command):
                return False
    except OSError:
        return False  # off, or unreadable: leave it
    return enable(command, config_home, winreg)[0]


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
