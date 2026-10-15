"""One Telescope per user: the lock later launches find, raising the running window, and `telescope --action`."""

import hashlib
import hmac
import json
import logging
import os
import socket
import sys
import time
from pathlib import Path
from typing import Optional

from PyQt6.QtWidgets import QMessageBox

import update_guard
from telescope import dev_profile
from telescope.config import config_path

# ── Single-instance enforcement ───────────────────────────────────────────────
# The guard checks the real app's address before an update's roll-back; a dev profile takes its own so it runs next to it
_INSTANCE_FAMILY, _INSTANCE_ADDRESS = update_guard.instance_address(dev_profile.active())


def user_token() -> bytes:
    """Who is running this copy, so a shared port (Windows) never raises another user's window."""
    user = os.environ.get("USERNAME") or os.environ.get("USER") or str(os.getuid() if hasattr(os, "getuid") else "")
    session = ""
    if sys.platform == "win32":
        try:
            import ctypes
            sid = ctypes.c_ulong()
            ctypes.windll.kernel32.ProcessIdToSessionId(os.getpid(), ctypes.byref(sid))
            session = str(sid.value)
        except Exception:
            pass
    return hashlib.sha256(f"{user}\0{session}".encode()).hexdigest()[:16].encode()


def acquire_single_instance(wait: float = 0.0) -> Optional[socket.socket]:
    """The single-instance socket, or None after asking the running copy to show itself.

    wait: keep retrying this long first, for a relaunch after an update while the old copy exits.
    """
    deadline = time.monotonic() + wait
    while True:
        srv = update_guard.instance_socket(_INSTANCE_FAMILY)
        try:
            srv.bind(_INSTANCE_ADDRESS)
            srv.listen(1)
            return srv
        except OSError:
            srv.close()
            if time.monotonic() >= deadline:
                break
            time.sleep(0.25)
    c = socket.socket(_INSTANCE_FAMILY, socket.SOCK_STREAM)
    reply = b""
    try:
        c.settimeout(2)
        c.connect(_INSTANCE_ADDRESS)
        c.sendall(b"raise:" + user_token())
        reply = c.recv(5)
    except Exception:
        pass
    finally:
        c.close()
    if reply == b"other":
        _other_user_notice()
    elif reply != b"ok":
        _port_taken_notice()
    return None


def _other_user_notice():
    QMessageBox.information(
        None, "Telescope is already running",
        "Telescope is already running for another user on this computer. Close it there, then open it again here.",
    )


def _port_taken_notice():
    # Quitting without a word looks like a crash when the lock is held by something that isn't Telescope
    where = f"port {_INSTANCE_ADDRESS[1]}" if _INSTANCE_FAMILY == socket.AF_INET else "a local socket"
    QMessageBox.warning(
        None, "Telescope can't start",
        f"Telescope couldn't start because something else is holding its single-instance lock ({where}), "
        "or another Telescope is stuck. Close that program, or restart the computer, then open Telescope again.",
    )


_ACTION = b"action:"
_MAX_REQUEST = 16384


def control_key_path() -> Path:
    """Where the running copy keeps the key `--action` has to send: in the user's own settings folder, so another
    account on the computer (which can reach the single-instance socket too) can't drive this one's controls."""
    return config_path().parent / ".control-key"


def write_control_key() -> Optional[bytes]:
    """A new key for this run, readable only by this user; None if it couldn't be written (then --action can't work)."""
    key = hashlib.sha256(os.urandom(32)).hexdigest().encode()
    path = control_key_path()
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.unlink(missing_ok=True)  # a new file gets the mode below; an old one would keep its own
        fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(fd, "wb") as f:
            f.write(key)
    except OSError:
        logging.exception("Couldn't write the key for telescope --action")
        return None
    return key


def send_action(request: dict) -> tuple:
    """For `telescope --action`: hand request ({"action", "params"} or {"list": True}) to the running copy.
    Returns (ok, text to print)."""
    try:
        key = control_key_path().read_bytes().strip()
    except OSError:
        return False, "Telescope isn't running."
    payload = _ACTION + json.dumps({**request, "key": key.decode(errors="replace")}).encode()
    c = socket.socket(_INSTANCE_FAMILY, socket.SOCK_STREAM)
    reply = b""
    try:
        c.settimeout(8)
        c.connect(_INSTANCE_ADDRESS)
        c.sendall(payload)
        c.shutdown(socket.SHUT_WR)
        while len(reply) < 1 << 20:
            chunk = c.recv(65536)
            if not chunk:
                break
            reply += chunk
    except OSError:
        return False, "Telescope isn't running."
    finally:
        c.close()
    try:
        answer = json.loads(reply.decode())
        return bool(answer["ok"]), str(answer["text"])
    except (ValueError, KeyError, TypeError):
        return False, "Telescope didn't understand that. Is it an older version? Update it to use --action."


def _read_request(conn: socket.socket, first: bytes) -> Optional[dict]:
    data = first
    deadline = time.monotonic() + 2  # a client that trickles bytes mustn't hold up every later raise
    while len(data) < _MAX_REQUEST:
        if time.monotonic() > deadline:
            return None
        chunk = conn.recv(4096)
        if not chunk:
            break
        data += chunk
    try:
        request = json.loads(data[len(_ACTION):].decode())
    except (ValueError, UnicodeDecodeError):
        return None
    return request if isinstance(request, dict) else None


def listen_for_raise(srv: socket.socket, raise_cb, action_cb=None, control_key: Optional[bytes] = None):
    """Answer later launches: raise:<user token> brings the window up; action:<json> (from --action, carrying
    control_key) runs action_cb(request) and sends back its {"ok", "text"} as JSON."""
    srv.settimeout(1.0)
    while True:
        try:
            conn, _ = srv.accept()
        except socket.timeout:
            continue
        except OSError:
            break  # closed on quit
        try:
            conn.settimeout(1.0)
            message = conn.recv(4096)
            if message.startswith(_ACTION):
                request = _read_request(conn, message)
                sent = str((request or {}).get("key", "")).encode()
                if request is not None and action_cb is not None and not control_key:
                    reply = {"ok": False, "text": "Telescope couldn't write the key --action needs next to its "
                                                  "settings when it started. Restart it."}
                elif request is None or action_cb is None or not hmac.compare_digest(sent, control_key):
                    reply = {"ok": False, "text": "Telescope turned that down: it didn't come from this user's "
                                                  "Telescope command."}
                else:
                    request.pop("key", None)
                    reply = action_cb(request)
                conn.sendall(json.dumps(reply).encode())
            elif message == b"raise:" + user_token():
                raise_cb()
                conn.sendall(b"ok")  # tells the new copy a Telescope answered, not some other program holding the lock
            elif message.startswith(b"raise:"):
                conn.sendall(b"other")  # another user's Telescope, on a port both users can reach
        except OSError:
            pass  # a connection that says nothing or resets mustn't stop every later raise
        finally:
            conn.close()
