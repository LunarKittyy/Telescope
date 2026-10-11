"""Global shortcuts: keys that work while another app has focus.

Each system has its own way in. Windows lets an app claim a key outright (RegisterHotKey, hotkeys_windows.py);
on Linux the GlobalShortcuts desktop portal does it (hotkeys_portal.py), which Plasma 5.27 and later, GNOME 48
and later and Hyprland offer, X11 sessions included. With the portal the app only suggests a key: the desktop
asks the user to confirm it and they can change it in its own settings, so what's actually bound comes back as
text to show. Elsewhere there's no backend, and `telescope --action` bound in the desktop's own shortcut
settings stands in.
"""

import logging
from dataclasses import dataclass
from typing import Optional

from PyQt6.QtCore import QObject, Qt, pyqtSignal

from telescope.platform import IS_LINUX, IS_WINDOWS

logger = logging.getLogger(__name__)


@dataclass(frozen=True)
class Hotkey:
    id: str
    keys: str
    """Qt portable text, "Ctrl+Alt+M"."""
    description: str


@dataclass(frozen=True)
class HotkeyStatus:
    working: bool
    text: str
    """What to show next to the binding: the key the system bound, or why it doesn't work."""


class HotkeyBackend(QObject):
    """Claims keys from the system and says when they're pressed and let go. Signals always come on the GUI thread
    (a backend that listens on a thread of its own sends them through the queued _from_thread ones)."""

    pressed = pyqtSignal(str)
    released = pyqtSignal(str)
    changed = pyqtSignal()
    """Statuses changed (the system confirmed keys, or the user changed them in its settings)."""

    _pressed_from_thread = pyqtSignal(str)
    _released_from_thread = pyqtSignal(str)
    _changed_from_thread = pyqtSignal()

    system_picks_keys = False
    """True when the system decides the final key (the portal): the editor then says so, and offers configure()."""

    def __init__(self):
        super().__init__()
        queued = Qt.ConnectionType.QueuedConnection
        self._pressed_from_thread.connect(self.pressed, queued)
        self._released_from_thread.connect(self.released, queued)
        self._changed_from_thread.connect(self.changed, queued)

    def apply(self, hotkeys: list) -> None:
        """Make exactly these Hotkeys the global ones."""

    def status(self, hotkey_id: str) -> Optional[HotkeyStatus]:
        """How the hotkey with this id is doing, or None before the system answered."""
        return None

    def note(self) -> str:
        """A line for the shortcuts dialog about how global shortcuts work here."""
        return ""

    def can_configure(self) -> bool:
        return False

    def configure(self) -> bool:
        """Open the system's own settings for these shortcuts; False if that didn't work."""
        return False

    def stop(self) -> None:
        """Let every key go (quitting)."""


def create_backend(app_id: str) -> Optional[HotkeyBackend]:
    """This system's backend, or None where there's none (the dialog then points at telescope --action)."""
    try:
        if IS_WINDOWS:
            from telescope.platform.hotkeys_windows import WindowsHotkeys
            return WindowsHotkeys()
        if IS_LINUX:
            from telescope.platform import autostart
            from telescope.platform.hotkeys_portal import PortalHotkeys, portal_possible
            if portal_possible():
                return PortalHotkeys(app_id, prepare=lambda: autostart.ensure_portal_entry(app_id))
    except Exception:
        logger.exception("Global shortcuts aren't available")
    return None
