# -*- mode: python ; coding: utf-8 -*-
#
# PyInstaller spec for Telescope Desktop (Windows).
# Build: pyinstaller telescope.spec
# Output: dist/TelescopeDesktop/  (a folder: TelescopeDesktop.exe next to lib-<build>/)
#
# Notes:
#   - A folder build, not onefile: onefile unpacks everything to %TEMP% (and past the virus scanner) on every
#     launch. The libraries go in lib-<build>, a new folder per build, so an update can move the next build's
#     in while this one's are still loaded; updates.clean_up_after_update() deletes the old one.
#   - Qt comes in through PyInstaller's own hooks, which take only the modules Telescope imports (Core, Gui,
#     Widgets, Svg) and their plugins, not all of PyQt6.
#   - No UPX: compressed DLLs have to be unpacked again on every load, and virus scanners look harder at them.
#   - pyvirtualcam's unitycapture backend calls into a system-installed
#     DirectShow COM filter (UnityCapture), so no DLLs need bundling.
#   - cv2 wheels ship their own DLLs; PyInstaller's cv2 hook handles collection.
#   - The exe's icon is resources/telescope.ico, the app icon (create_app_icon); scripts/write_icon.py
#     writes it again if the icon changes.

import os
import sys

from PyInstaller.utils.hooks import collect_submodules

sys.path.insert(0, SPECPATH)
from telescope.updates import LIB_PREFIX  # noqa: E402
from telescope.version import BUILD  # noqa: E402

a = Analysis(
    ['main.py'],
    pathex=[],
    binaries=[],
    datas=[('telescope/web', 'telescope/web')],  # the Browser camera page
    # ifaddr picks its platform backend behind an `os.name` check, so pull
    # the whole package rather than relying on that branch being followed.
    hiddenimports=collect_submodules('telescope') + collect_submodules('ifaddr') + collect_submodules('zeroconf') + collect_submodules('av') + collect_submodules('cryptography') + [
        'sounddevice',  # hooks-contrib's hook collects its PortAudio DLL
        'pyvirtualcam',
        'cv2',
        'numpy',
        'PyQt6.sip',
        'PyQt6.QtCore',
        'PyQt6.QtGui',
        'PyQt6.QtWidgets',
        'PyQt6.QtSvg',
    ],
    hookspath=[],
    hooksconfig={},
    runtime_hooks=[],
    excludes=[
        'matplotlib', 'scipy', 'PIL', 'tkinter',
        'PyQt5', 'PySide2', 'PySide6',
    ],
    noarchive=False,
)

pyz = PYZ(a.pure)

exe = EXE(
    pyz,
    a.scripts,
    [],
    exclude_binaries=True,
    name='TelescopeDesktop',
    debug=False,
    bootloader_ignore_signals=False,
    strip=False,
    upx=False,
    console=False,
    disable_windowed_traceback=False,
    argv_emulation=False,
    target_arch=None,
    codesign_identity=None,
    entitlements_file=None,
    icon=os.path.join(SPECPATH, 'resources', 'telescope.ico'),
    contents_directory=f'{LIB_PREFIX}{BUILD}',
)

coll = COLLECT(
    exe,
    a.binaries,
    a.datas,
    strip=False,
    upx=False,
    name='TelescopeDesktop',
)
