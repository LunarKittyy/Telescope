"""The desktop log and Copy diagnostics. The log lives in the temp folder, capped in size, and every
line is sanitized first: no tokens, addresses or home paths."""

import getpass
import logging
import os
import platform
import re
import sys
import tempfile
import threading
import time
from collections import deque
from pathlib import Path
from typing import Optional

from telescope import version

MAX_BYTES = 1_000_000  # per file; the one before is kept as telescope.log.1
MAX_LINE = 300  # 200 lines of this still fit in a GitHub issue field
REPORT_LINES = 200

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


class EventLog(logging.Handler):
    """Info and up, plus notes and crashes, appended to a log file in the temp folder (so the OS
    clears it) and rotated at MAX_BYTES with one previous file kept. A repeat of the latest line is
    counted instead of written again, so a reconnect loop stays one line. Without a file (couldn't
    create it, or in tests) the lines stay in memory, as many as a report shows."""

    def __init__(self, path: Optional[Path] = None, max_bytes: int = MAX_BYTES):
        super().__init__(logging.INFO)
        self.path: Optional[Path] = None
        self._max_bytes = max_bytes
        self._file = None
        self._memory: deque = deque(maxlen=REPORT_LINES)
        self._last: Optional[tuple] = None  # (level, text) of the latest line
        self._repeats = 0
        self._events_lock = threading.Lock()
        if path is not None:
            self.open(path)

    def open(self, path: Path) -> bool:
        try:
            path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
            if hasattr(os, "getuid") and path.parent.stat().st_uid != os.getuid():
                return False  # someone else's folder in a shared /tmp
            f = open(path, "a", encoding="utf-8")
        except OSError:
            return False
        with self._events_lock:
            if self._file is not None:
                self._file.close()
            self._file, self.path = f, path
        return True

    @property
    def previous_path(self) -> Optional[Path]:
        return self.path.with_name(self.path.name + ".1") if self.path else None

    def emit(self, record: logging.LogRecord):
        if record.levelno < logging.WARNING and not record.name.startswith(("telescope", "__main__")):
            return  # other libraries' info is noise here
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
        text = sanitize(text)[:MAX_LINE]
        with self._events_lock:
            if self._last == (level, text):
                self._repeats += 1
                return
            self._flush_repeats()
            self._last = (level, text)
            stamp = time.strftime("%Y-%m-%d %H:%M:%S", time.localtime(when or time.time()))
            self._write(f"{stamp} {level} {text}")

    def close(self):
        with self._events_lock:
            self._flush_repeats()
            if self._file is not None:
                self._file.close()
                self._file = None
        super().close()

    def _flush_repeats(self):
        if self._repeats:
            self._write(f"    (repeated {self._repeats} more times)")
            self._repeats = 0

    def _write(self, line: str):
        self._memory.append(line)
        if self._file is None:
            return
        try:
            if self._file.tell() > self._max_bytes:
                self._file.close()
                os.replace(self.path, self.previous_path)
                self._file = open(self.path, "w", encoding="utf-8")
            self._file.write(line + "\n")
            self._file.flush()
        except OSError:
            self._file = None  # disk full or the folder was cleaned: carry on in memory

    def lines(self, limit: int = REPORT_LINES) -> list[str]:
        """The latest lines, across a restart when the file has them."""
        with self._events_lock:
            pending = [f"    (repeated {self._repeats} more times)"] if self._repeats else []
            if self._file is None:
                return (list(self._memory) + pending)[-limit:]
            out: list[str] = []
            for path in (self.previous_path, self.path):
                try:
                    out += path.read_text(encoding="utf-8", errors="replace").splitlines()
                except OSError:
                    pass
            return (out + pending)[-limit:]


def log_dir() -> Path:
    """Linux: a per-user folder in /tmp (cleared at reboot). Windows: %TEMP%\\Telescope, which
    Storage Sense and Disk Cleanup clear."""
    base = Path(tempfile.gettempdir())
    if sys.platform.startswith("win"):
        return base / "Telescope"
    try:
        user = getpass.getuser()
    except Exception:
        user = str(os.getuid())
    return base / f"telescope-{user}"


def link_log(link: Path, target: Path):
    """A telescope.log shortcut next to the app. Skipped where it can't be made (Windows without
    symlink rights, a read-only folder) or where a real file has that name."""
    try:
        if link.is_symlink():
            if Path(os.readlink(link)) == target:
                return
            link.unlink()
        elif link.exists():
            return
        link.symlink_to(target)
    except (OSError, NotImplementedError):
        pass


def describe_exception(exc: BaseException) -> str:
    """Type, message and where it was raised (file name and line only, no full path)."""
    where = ""
    tb = exc.__traceback__
    while tb is not None and tb.tb_next is not None:
        tb = tb.tb_next
    if tb is not None:
        where = f" at {os.path.basename(tb.tb_frame.f_code.co_filename)}:{tb.tb_lineno}"
    return f"{type(exc).__name__}: {exc}{where}"


events = EventLog()


def install(app_dir: Optional[Path] = None):
    """Log to the file from here on, including uncaught exceptions, and link it into app_dir."""
    root = logging.getLogger()
    if events in root.handlers:
        return
    if events.open(log_dir() / "telescope.log"):
        if app_dir is not None:
            link_log(app_dir / "telescope.log", events.path)
    root.setLevel(logging.INFO)
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
    events.add("START", f"Telescope {version.display_version()}")


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
    lines += ["", f"Log (last {REPORT_LINES} lines):" if recent else "Log: empty", *recent]
    return "\n".join(lines)
