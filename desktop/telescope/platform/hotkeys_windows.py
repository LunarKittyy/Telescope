"""Windows global shortcuts: RegisterHotKey, with WM_HOTKEY caught from Qt's own message loop.

Registered with no window, so Windows posts WM_HOTKEY to the GUI thread's queue, where a native event filter
sees it before Qt would drop it. Windows never says when the key comes back up, so for "while held" a timer
watches the key (GetAsyncKeyState) once it went down. No hook, no admin rights: a key another app already
registered just fails, and the dialog says so.
"""

import ctypes
import logging
from ctypes import wintypes
from typing import Optional

from PyQt6.QtCore import QAbstractNativeEventFilter, QCoreApplication, Qt, QTimer

from telescope.platform.hotkeys import HotkeyBackend, HotkeyStatus
from telescope.shortcuts import split_keys

logger = logging.getLogger(__name__)

WM_HOTKEY = 0x0312
MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_WIN, MOD_NOREPEAT = 0x1, 0x2, 0x4, 0x8, 0x4000
_MODS = {"Ctrl": MOD_CONTROL, "Alt": MOD_ALT, "Shift": MOD_SHIFT, "Meta": MOD_WIN}
_RELEASE_POLL_MS = 30

_VK = {
    Qt.Key.Key_Space: 0x20, Qt.Key.Key_Return: 0x0D, Qt.Key.Key_Enter: 0x0D, Qt.Key.Key_Escape: 0x1B,
    Qt.Key.Key_Tab: 0x09, Qt.Key.Key_Backspace: 0x08, Qt.Key.Key_Insert: 0x2D, Qt.Key.Key_Delete: 0x2E,
    Qt.Key.Key_Home: 0x24, Qt.Key.Key_End: 0x23, Qt.Key.Key_PageUp: 0x21, Qt.Key.Key_PageDown: 0x22,
    Qt.Key.Key_Left: 0x25, Qt.Key.Key_Up: 0x26, Qt.Key.Key_Right: 0x27, Qt.Key.Key_Down: 0x28,
    Qt.Key.Key_Pause: 0x13, Qt.Key.Key_Print: 0x2C, Qt.Key.Key_ScrollLock: 0x91,
    Qt.Key.Key_VolumeMute: 0xAD, Qt.Key.Key_VolumeDown: 0xAE, Qt.Key.Key_VolumeUp: 0xAF,
    Qt.Key.Key_MediaNext: 0xB0, Qt.Key.Key_MediaPrevious: 0xB1, Qt.Key.Key_MediaStop: 0xB2,
    Qt.Key.Key_MediaPlay: 0xB3, Qt.Key.Key_MediaTogglePlayPause: 0xB3,
}


def to_windows(keys: str, vk_for_char=None) -> Optional[tuple]:
    """(modifier flags, virtual-key code) for normalized keys, or None for a key Windows can't register.
    vk_for_char(ch) gives VkKeyScanW's answer for punctuation, which depends on the keyboard layout."""
    mods, name, qkey = split_keys(keys)
    flags = 0
    for m in mods:
        flags |= _MODS[m]
    if Qt.Key.Key_A <= qkey <= Qt.Key.Key_Z or Qt.Key.Key_0 <= qkey <= Qt.Key.Key_9:
        return flags, int(qkey)  # VK codes for letters and digits are their ASCII codes
    if Qt.Key.Key_F1 <= qkey <= Qt.Key.Key_F24:
        return flags, 0x70 + int(qkey) - int(Qt.Key.Key_F1)
    if qkey in _VK:
        return flags, _VK[qkey]
    if len(name) == 1 and vk_for_char is not None:
        scan = vk_for_char(name)
        if scan == -1 or scan & 0xFF == 0xFF:
            return None
        if scan & 0x100:  # the layout needs Shift for this character
            flags |= MOD_SHIFT
        return flags, scan & 0xFF
    return None


class _Filter(QAbstractNativeEventFilter):
    def __init__(self, on_hotkey):
        super().__init__()
        self._on_hotkey = on_hotkey

    def nativeEventFilter(self, event_type, message):
        if bytes(event_type) == b"windows_generic_MSG":
            msg = wintypes.MSG.from_address(int(message))
            if msg.message == WM_HOTKEY:
                self._on_hotkey(int(msg.wParam))
                return True, 0
        return False, 0


class WindowsHotkeys(HotkeyBackend):
    def __init__(self, user32=None, install_filter: bool = True):
        super().__init__()
        self._user32 = user32 if user32 is not None else _load_user32()
        self._by_number: dict = {}  # RegisterHotKey's id: (hotkey id, virtual-key code)
        self._statuses: dict = {}
        self._held: dict = {}  # hotkey id: virtual-key code, while its key is down
        self._poll = QTimer(self)
        self._poll.setInterval(_RELEASE_POLL_MS)
        self._poll.timeout.connect(self._check_released)
        self._filter = None
        if install_filter:
            self._filter = _Filter(self._on_hotkey)
            QCoreApplication.instance().installNativeEventFilter(self._filter)

    def apply(self, hotkeys: list) -> None:
        self._unregister_all()
        self._statuses = {}
        for number, hk in enumerate(hotkeys, start=1):
            native = to_windows(hk.keys, self._user32.VkKeyScanW)
            if native is None:
                self._statuses[hk.id] = HotkeyStatus(False, "Windows can't use this key everywhere")
                continue
            flags, vk = native
            if self._user32.RegisterHotKey(None, number, flags | MOD_NOREPEAT, vk):
                self._by_number[number] = (hk.id, vk)
                self._statuses[hk.id] = HotkeyStatus(True, "Everywhere")
            else:
                self._statuses[hk.id] = HotkeyStatus(False, "Another app already uses this key")
        self.changed.emit()

    def status(self, hotkey_id: str) -> Optional[HotkeyStatus]:
        return self._statuses.get(hotkey_id)

    def note(self) -> str:
        return ("Everywhere shortcuts are taken from Windows while Telescope runs. A key another app already "
                "took only works inside Telescope.")

    def stop(self) -> None:
        self._unregister_all()
        self._poll.stop()
        if self._filter is not None:
            app = QCoreApplication.instance()
            if app is not None:
                app.removeNativeEventFilter(self._filter)
            self._filter = None

    def _unregister_all(self):
        for number in self._by_number:
            self._user32.UnregisterHotKey(None, number)
        self._by_number = {}
        for hid in list(self._held):
            self._held.pop(hid)
            self.released.emit(hid)

    def _on_hotkey(self, number: int):
        entry = self._by_number.get(number)
        if entry is None:
            return
        hid, vk = entry
        if hid in self._held:
            return  # MOD_NOREPEAT should already stop these
        self._held[hid] = vk
        self._poll.start()
        self.pressed.emit(hid)

    def _check_released(self):
        for hid, vk in list(self._held.items()):
            if not self._user32.GetAsyncKeyState(vk) & 0x8000:
                del self._held[hid]
                self.released.emit(hid)
        if not self._held:
            self._poll.stop()


def _load_user32():
    user32 = ctypes.WinDLL("user32", use_last_error=True)
    user32.RegisterHotKey.argtypes = [wintypes.HWND, ctypes.c_int, wintypes.UINT, wintypes.UINT]
    user32.RegisterHotKey.restype = wintypes.BOOL
    user32.UnregisterHotKey.argtypes = [wintypes.HWND, ctypes.c_int]
    user32.UnregisterHotKey.restype = wintypes.BOOL
    user32.GetAsyncKeyState.argtypes = [ctypes.c_int]
    user32.GetAsyncKeyState.restype = ctypes.c_short
    user32.VkKeyScanW.argtypes = [wintypes.WCHAR]
    user32.VkKeyScanW.restype = ctypes.c_short
    return user32
