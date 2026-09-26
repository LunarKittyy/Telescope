"""Shared platform utilities: subprocess runner, adb helpers, path helpers."""

import platform
import shutil
import socket
import subprocess
import sys
import threading
import time
from pathlib import Path
from typing import Optional

IS_LINUX   = platform.system() == "Linux"
IS_WINDOWS = platform.system() == "Windows"

# The packaged Windows app has no console, so Windows gives each console program it runs (adb, powershell)
# a window of its own, flashing up on every status check. This keeps them hidden.
NO_WINDOW = {"creationflags": subprocess.CREATE_NO_WINDOW} if IS_WINDOWS else {}


def _run(cmd, timeout=10):
    if cmd[0] is None:  # adb_exe() found nothing; subprocess would raise TypeError, not FileNotFoundError.
        return -1, "", "adb not found"
    if Path(cmd[0]).stem.lower() == "adb":
        ensure_adb_server()
    try:
        r = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout, **NO_WINDOW)
        return r.returncode, r.stdout, r.stderr
    except FileNotFoundError:         return -1, "", f"Not found: {cmd[0]}"
    except subprocess.TimeoutExpired: return -2, "", "Timed out"


# ── adb's server ──────────────────────────────────────────────────────────────
# adb's first command starts a background server that outlives whoever ran it. On Windows a running adb.exe
# locks its folder, so Telescope couldn't be deleted or fully updated after quitting. So on Windows Telescope
# starts the server itself, as its own child in a job that dies with it (winjob.py). A server another program
# already runs is left alone and simply used.
ADB_PORT = 5037
OWN_ADB_SERVER = IS_WINDOWS
_adb_server: Optional[subprocess.Popen] = None
_adb_server_lock = threading.Lock()


def adb_server_running(port: int = ADB_PORT) -> bool:
    try:
        with socket.create_connection(("127.0.0.1", port), timeout=0.3):
            return True
    except OSError:
        return False


def _tie_to_telescope(proc) -> None:
    from telescope.platform.winjob import kill_with_us
    kill_with_us(proc)


def ensure_adb_server(running=adb_server_running, popen=subprocess.Popen, tie=_tie_to_telescope,
                      sleep=time.sleep) -> None:
    """Windows: start adb's server as Telescope's child unless one is already up."""
    global _adb_server
    if not OWN_ADB_SERVER:
        return
    with _adb_server_lock:
        if _adb_server is not None and _adb_server.poll() is None:
            return
        _adb_server = None
        exe = adb_exe()
        if exe is None or running():
            return
        try:
            # "nodaemon" keeps the server in this process instead of the detached copy adb forks by default.
            proc = popen([exe, "nodaemon", "server"], stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                         stderr=subprocess.DEVNULL, **NO_WINDOW)
        except OSError:
            return
        tie(proc)
        _adb_server = proc
        for _ in range(30):  # it listens within a second or so; the adb command after this would start its own
            if running() or proc.poll() is not None:
                return
            sleep(0.1)


def stop_adb_server() -> None:
    """Stop the server Telescope started (at quit, and before an update replaces platform-tools)."""
    global _adb_server
    with _adb_server_lock:
        proc, _adb_server = _adb_server, None
    if proc is None or proc.poll() is not None:
        return
    proc.terminate()
    try:
        proc.wait(timeout=3)
    except subprocess.TimeoutExpired:
        proc.kill()


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
