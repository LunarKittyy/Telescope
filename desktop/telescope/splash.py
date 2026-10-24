"""The Windows build's start-up splash: PyInstaller's launcher shows it before Python starts (telescope.spec).

A first start after installing spends a minute or more before the window shows while Windows checks each new program
file, so the splash says why once it's been up a while (a warm start is gone well before). Only light modules here: whatever this imports is still waiting on that check.
"""

import os
import textwrap
import threading
from typing import Callable, List

from telescope import i18n
from telescope.i18n import tr

WHY_AFTER_S = 5  # since launch: the line about the security scan only shows when the start is slow
WRAP_AT = 58  # characters: about what fits across the splash in its font, which can't measure text before Qt loads


def _splash():
    try:
        import pyi_splash
    except ImportError:  # not the packaged build
        return None
    return pyi_splash if pyi_splash.is_alive() else None


def _windows_languages() -> List[str]:
    """Windows' display languages, most wanted first ("sv-SE", "en-US"), asked without loading Qt."""
    import ctypes
    kernel32 = ctypes.windll.kernel32
    count, size = ctypes.c_ulong(), ctypes.c_ulong()
    MUI_LANGUAGE_NAME = 0x8
    if not kernel32.GetUserPreferredUILanguages(MUI_LANGUAGE_NAME, ctypes.byref(count), None, ctypes.byref(size)):
        return []
    buf = ctypes.create_unicode_buffer(size.value)
    if not kernel32.GetUserPreferredUILanguages(MUI_LANGUAGE_NAME, ctypes.byref(count), buf, ctypes.byref(size)):
        return []
    return [tag for tag in buf[:size.value].split("\0") if tag]


def show_text(chosen_language: Callable[[], str]):
    """Puts the status text on the splash in the user's language, before Qt loads (most of the wait)."""
    splash = _splash()
    if splash is None:
        return
    chosen = os.environ.get(i18n.ENV_LANGUAGE, "").strip() or chosen_language()
    try:
        wanted = [chosen] if chosen else _windows_languages()
    except (AttributeError, OSError):
        wanted = []
    i18n.load(i18n.pick(wanted, i18n.available()), qt=False)
    starting = tr("Starting Telescope…")
    # Translators: added under "Starting Telescope…" while the app starts on Windows. The scan is Windows checking new program files
    why = tr("This can take a minute while Windows runs a security scan.")
    both = f"{starting}\n{textwrap.fill(why, WRAP_AT)}"
    wait = WHY_AFTER_S - _seconds_since_launch()
    if wait <= 0:
        splash.update_text(both)
        return
    splash.update_text(starting)
    timer = threading.Timer(wait, _update, args=(both,))
    timer.daemon = True
    timer.start()


def _update(text: str):
    splash = _splash()
    try:
        if splash is not None:
            splash.update_text(text)
    except Exception:  # closed meanwhile
        pass


def _seconds_since_launch() -> float:
    """How long ago Windows started this process (the launcher showed the splash right then); 0 if it won't say."""
    import ctypes
    from ctypes import wintypes
    try:
        kernel32 = ctypes.windll.kernel32
        created, ended, kernel, user, now = (wintypes.FILETIME() for _ in range(5))
        if not kernel32.GetProcessTimes(kernel32.GetCurrentProcess(), ctypes.byref(created), ctypes.byref(ended),
                                        ctypes.byref(kernel), ctypes.byref(user)):
            return 0.0
        kernel32.GetSystemTimeAsFileTime(ctypes.byref(now))
    except (AttributeError, OSError):
        return 0.0
    ticks = ((now.dwHighDateTime - created.dwHighDateTime) << 32) + now.dwLowDateTime - created.dwLowDateTime
    return ticks / 10_000_000


def close():
    splash = _splash()
    if splash is not None:
        splash.close()
