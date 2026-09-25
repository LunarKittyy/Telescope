"""Copy diagnostics: a short, sanitized report for bug reports. Recent warnings, errors and statuses are kept in memory
only (a bounded list, nothing written to disk) and never include tokens, addresses or paths."""

import logging
import os
import platform
import re
import sys
import threading
import time
from collections import deque
from pathlib import Path
from typing import Optional

from telescope import version

MAX_EVENTS = 50

_URL = re.compile(r"\b[a-z][a-z0-9+.-]*://\S+", re.I)
_IPV4 = re.compile(r"\b\d{1,3}(?:\.\d{1,3}){3}(?::\d+)?\b")
# Full form, or compressed with "::"; plain "12:30:45" times don't match.
_IPV6 = re.compile(r"(?<![\w:])(?:(?:[0-9a-f]{1,4}:){7}[0-9a-f]{1,4}|[0-9a-f:]*::[0-9a-f:]*)(?![\w:])", re.I)
_BEARER = re.compile(r"(bearer|token|nonce|secret)([\"'=: ]+)[^\s\"',}]+", re.I)
_LONG_SECRET = re.compile(r"\b[A-Za-z0-9_\-+/]{24,}={0,2}")


def sanitize(text: str) -> str:
    """Strip what shouldn't end up in a public issue: URLs, IPs, tokens and the user's home path."""
    home = str(Path.home())
    if len(home) > 1:
        text = text.replace(home, "~")
    text = _URL.sub("<url>", text)
    text = _BEARER.sub(r"\1\2<redacted>", text)
    text = _IPV4.sub("<ip>", text)
    text = _IPV6.sub("<ip>", text)
    return _LONG_SECRET.sub("<redacted>", text)


class RecentEvents(logging.Handler):
    """The last MAX_EVENTS warnings, errors and notes. A repeat of the latest one bumps its count
    instead of taking another slot, so a reconnect loop can't push everything else out."""

    def __init__(self, maxlen: int = MAX_EVENTS):
        super().__init__(logging.WARNING)
        self._events: deque = deque(maxlen=maxlen)  # [time, level, text, count]
        self._events_lock = threading.Lock()

    def emit(self, record: logging.LogRecord):
        try:
            text = f"{record.name}: {record.getMessage()}"
            if record.exc_info and record.exc_info[1] is not None:
                text += f" ({describe_exception(record.exc_info[1])})"
        except Exception:
            return
        self.add(record.levelname, text, record.created)

    def note(self, text: str):
        """Something the user saw (a status or banner), not a log record."""
        self.add("NOTE", text)

    def add(self, level: str, text: str, when: Optional[float] = None):
        text = sanitize(text)[:300]
        with self._events_lock:
            last = self._events[-1] if self._events else None
            if last is not None and last[1] == level and last[2] == text:
                last[3] += 1
                last[0] = when or time.time()
                return
            self._events.append([when or time.time(), level, text, 1])

    def lines(self) -> list[str]:
        with self._events_lock:
            events = [list(e) for e in self._events]
        out = []
        for when, level, text, count in events:
            stamp = time.strftime("%H:%M:%S", time.localtime(when))
            out.append(f"{stamp} {level} {text}" + (f" (x{count})" if count > 1 else ""))
        return out


def describe_exception(exc: BaseException) -> str:
    """Type, message and where it was raised (file name and line only, no full path)."""
    where = ""
    tb = exc.__traceback__
    while tb is not None and tb.tb_next is not None:
        tb = tb.tb_next
    if tb is not None:
        where = f" at {os.path.basename(tb.tb_frame.f_code.co_filename)}:{tb.tb_lineno}"
    return f"{type(exc).__name__}: {exc}{where}"


events = RecentEvents()


def install():
    """Collect warnings, errors and uncaught exceptions from here on."""
    root = logging.getLogger()
    if events in root.handlers:
        return
    if not root.handlers:
        # Adding a handler turns off logging's stderr fallback; keep printing to the terminal.
        console = logging.StreamHandler()
        console.setLevel(logging.WARNING)
        console.setFormatter(logging.Formatter("%(levelname)s %(name)s: %(message)s"))
        root.addHandler(console)
    root.addHandler(events)

    prev_hook = sys.excepthook

    def excepthook(exc_type, exc, tb):
        events.add("CRASH", describe_exception(exc.with_traceback(tb)))
        prev_hook(exc_type, exc, tb)

    prev_thread_hook = threading.excepthook

    def thread_excepthook(args):
        if args.exc_value is not None:
            events.add("CRASH", describe_exception(args.exc_value))
        prev_thread_hook(args)

    sys.excepthook = excepthook
    threading.excepthook = thread_excepthook


def system_lines() -> list[str]:
    if sys.platform.startswith("linux"):
        try:
            os_name = platform.freedesktop_os_release().get("PRETTY_NAME", "Linux")
        except OSError:
            os_name = "Linux"
        session = os.environ.get("XDG_SESSION_TYPE", "?")
        desktop = os.environ.get("XDG_CURRENT_DESKTOP", "?")
        return [f"OS: {os_name} ({platform.release()})", f"Desktop: {desktop}, {session}"]
    return [f"OS: {platform.system()} {platform.release()} ({platform.version()})"]


def report(state: dict, qt_platform: str = "") -> str:
    """The text Copy diagnostics puts on the clipboard. state is what the plugins report."""
    build = f"Telescope {version.display_version()}"
    if version.COMMIT:
        build += f" ({version.COMMIT[:7]})"
    lines = [build, *system_lines()]
    lines.append(f"Python {platform.python_version()}" + (f", Qt {qt_platform}" if qt_platform else "")
                 + (", bundled" if getattr(sys, "frozen", False) else ", from source"))
    lines += [sanitize(f"{key}: {value}") for key, value in state.items()]
    recent = events.lines()
    lines += ["", "Recent events:" if recent else "Recent events: none", *recent]
    return "\n".join(lines)
