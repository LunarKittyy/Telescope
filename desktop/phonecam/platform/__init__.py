"""Shared platform utilities: subprocess runner, adb helpers, path helpers."""

import platform
import shutil
import subprocess
import sys
from pathlib import Path
from typing import Optional

IS_LINUX   = platform.system() == "Linux"
IS_WINDOWS = platform.system() == "Windows"


def _run(cmd, timeout=10):
    try:
        r = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
        return r.returncode, r.stdout, r.stderr
    except FileNotFoundError:         return -1, "", f"Not found: {cmd[0]}"
    except subprocess.TimeoutExpired: return -2, "", "Timed out"


def platform_tools_dir() -> Path:
    if getattr(sys, "frozen", False):
        return Path(sys.executable).parent / "platform-tools"
    # __file__ is desktop/phonecam/platform/__init__.py → go up 3 levels to desktop/
    return Path(__file__).parent.parent.parent / "platform-tools"


def bundled_apk_path() -> Optional[Path]:
    """Return path to PhoneCam.apk sitting next to the script/exe, or None."""
    if getattr(sys, "frozen", False):
        base = Path(sys.executable).parent
    else:
        base = Path(__file__).parent.parent.parent
    p = base / "PhoneCam.apk"
    return p if p.exists() else None


def adb_exe() -> Optional[str]:
    local = platform_tools_dir() / ("adb.exe" if IS_WINDOWS else "adb")
    if local.exists():
        return str(local)
    return shutil.which("adb")


def adb_available() -> bool:
    return adb_exe() is not None


def adb_forward(port):
    rc, _, err = _run([adb_exe(), "forward", f"tcp:{port}", f"tcp:{port}"])
    return (True, f"Port {port} forwarded") if rc == 0 else (False, err)


def adb_unforward(port):
    _run([adb_exe(), "forward", "--remove", f"tcp:{port}"])
