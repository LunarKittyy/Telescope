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
    # __file__ is desktop/telescope/platform/__init__.py → go up 3 levels to desktop/
    return Path(__file__).parent.parent.parent / "platform-tools"


def bundled_apk_path() -> Optional[Path]:
    """Return path to Telescope.apk sitting next to the script/exe, or None."""
    if getattr(sys, "frozen", False):
        base = Path(sys.executable).parent
    else:
        base = Path(__file__).parent.parent.parent
    p = base / "Telescope.apk"
    return p if p.exists() else None


def adb_exe() -> Optional[str]:
    local = platform_tools_dir() / ("adb.exe" if IS_WINDOWS else "adb")
    if local.exists():
        return str(local)
    return shutil.which("adb")


def adb_available() -> bool:
    return adb_exe() is not None


def adb_devices() -> list:
    """Return serials of currently connected & authorized devices/emulators."""
    rc, out, _ = _run([adb_exe(), "devices"])
    if rc != 0:
        return []
    serials = []
    for line in out.splitlines()[1:]:
        parts = line.strip().split("\t")
        if len(parts) == 2 and parts[1] == "device":
            serials.append(parts[0])
    return serials


def _with_serial(cmd, serial):
    return [cmd[0], "-s", serial] + cmd[1:] if serial else cmd


def adb_forward(port, serial=None):
    rc, _, err = _run(_with_serial([adb_exe(), "forward", f"tcp:{port}", f"tcp:{port}"], serial))
    return (True, f"Port {port} forwarded") if rc == 0 else (False, err)


def adb_unforward(port, serial=None):
    _run(_with_serial([adb_exe(), "forward", "--remove", f"tcp:{port}"], serial))


def adb_reverse(port, serial=None):
    """Tunnel connections the phone makes to its own localhost:port back to
    this machine's localhost:port - the mirror of adb_forward, used so a
    USB-only phone (no LAN path, e.g. behind a VPN) can still reach the
    desktop's QR-pairing HTTP server."""
    rc, _, err = _run(_with_serial([adb_exe(), "reverse", f"tcp:{port}", f"tcp:{port}"], serial))
    return (True, f"Port {port} reversed") if rc == 0 else (False, err)


def adb_unreverse(port, serial=None):
    _run(_with_serial([adb_exe(), "reverse", "--remove", f"tcp:{port}"], serial))
