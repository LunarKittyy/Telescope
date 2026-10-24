"""The Windows build's start-up splash: PyInstaller's launcher shows it before Python starts (telescope.spec).

A first start after installing spends a minute or more before the window shows while Windows checks each new program
file, so the splash says why. Only light modules here: whatever this imports is still waiting on that check.
"""

import os
from typing import Callable, List

from telescope import i18n
from telescope.i18n import tr


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
    # Translators: under "Starting Telescope…" while the app starts on Windows. The scan is Windows checking new program files
    why = tr("This can take a minute while Windows runs a security scan.")
    splash.update_text(f"{starting}\n{why}")


def close():
    splash = _splash()
    if splash is not None:
        splash.close()
