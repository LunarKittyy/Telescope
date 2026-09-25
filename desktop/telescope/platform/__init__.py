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
    if cmd[0] is None:  # adb_exe() found nothing; subprocess would raise TypeError, not FileNotFoundError.
        return -1, "", "adb not found"
    try:
        r = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
        return r.returncode, r.stdout, r.stderr
    except FileNotFoundError:         return -1, "", f"Not found: {cmd[0]}"
    except subprocess.TimeoutExpired: return -2, "", "Timed out"


def platform_tools_dir() -> Path:
    if getattr(sys, "frozen", False):
        return Path(sys.executable).parent / "platform-tools"
    # __file__: desktop/telescope/platform/__init__.py, go up 3 levels to desktop/.
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


def adb_device_states() -> list:
    """(serial, state) for everything adb sees: "device" (usable), "unauthorized" (debugging prompt not accepted yet), "offline", ..."""
    rc, out, _ = _run([adb_exe(), "devices"])
    if rc != 0:
        return []
    found = []
    for line in out.splitlines()[1:]:
        parts = line.strip().split("\t")
        if len(parts) == 2:
            found.append((parts[0], parts[1]))
    return found


def adb_devices() -> list:
    """Return serials of currently connected & authorized devices/emulators."""
    return [serial for serial, state in adb_device_states() if state == "device"]


def _with_serial(cmd, serial):
    return [cmd[0], "-s", serial] + cmd[1:] if serial else cmd


def adb_forward(port, serial=None):
    rc, _, err = _run(_with_serial([adb_exe(), "forward", f"tcp:{port}", f"tcp:{port}"], serial))
    return (True, f"Port {port} forwarded") if rc == 0 else (False, err)


def adb_forward_auto(remote_port, serial=None):
    """Forward a free local port (adb picks it) to the phone's remote_port; returns the local port or None."""
    rc, out, _ = _run(_with_serial([adb_exe(), "forward", "tcp:0", f"tcp:{remote_port}"], serial))
    if rc != 0:
        return None
    try:
        return int(out.strip().splitlines()[-1])
    except (ValueError, IndexError):
        return None


def adb_unforward(port, serial=None):
    _run(_with_serial([adb_exe(), "forward", "--remove", f"tcp:{port}"], serial))


def adb_reverse(port, serial=None):
    """Tunnel the phone's own localhost:port to this machine's localhost:port - the mirror of adb_forward, so a USB-only phone can still reach the desktop's pairing server."""
    rc, _, err = _run(_with_serial([adb_exe(), "reverse", f"tcp:{port}", f"tcp:{port}"], serial))
    return (True, f"Port {port} reversed") if rc == 0 else (False, err)


def adb_unreverse(port, serial=None):
    _run(_with_serial([adb_exe(), "reverse", "--remove", f"tcp:{port}"], serial))


PAIR_BROADCAST_ACTION = "com.telescope.action.PAIR"


PAIR_BROADCAST_PACKAGE = "com.telescope"


def adb_broadcast_pair(payload_b64: str, serial=None):
    """Broadcasts base64-encoded pairing payload via adb (avoiding shell escaping) and restricts to Telescope package; returns whether adb succeeded, not phone delivery."""
    rc, _, err = _run(_with_serial(
        [adb_exe(), "shell", "am", "broadcast", "-a", PAIR_BROADCAST_ACTION,
         "-p", PAIR_BROADCAST_PACKAGE, "--es", "payload", payload_b64],
        serial,
    ))
    return (True, "Broadcast sent") if rc == 0 else (False, err)


def adb_install(serial: str, apk_path, timeout: int = 120) -> tuple:
    """`adb install -r` onto one phone; (ok, what to tell the user when it failed)."""
    rc, out, err = _run([adb_exe(), "-s", serial, "install", "-r", str(apk_path)], timeout=timeout)
    output = (out + err).strip()
    if rc == 0 and "Success" in output:
        return True, ""
    if "INSTALL_FAILED_UPDATE_INCOMPATIBLE" in output:
        # The installed app was signed with another key (the debug builds from before release signing).
        return False, "The Telescope app on the phone was signed differently. Uninstall it on the phone, then try again."
    if "INSTALL_FAILED_VERSION_DOWNGRADE" in output:
        return False, "The phone already has a newer Telescope app."
    return False, output.splitlines()[-1] if output else "adb install failed"
