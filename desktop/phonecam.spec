# -*- mode: python ; coding: utf-8 -*-
#
# PyInstaller spec for PhoneCam Desktop (Windows).
# Build: pyinstaller phonecam.spec
# Output: dist/PhoneCamDesktop.exe  (~60-80 MB onefile)
#
# Notes:
#   - pyvirtualcam's unitycapture backend calls into a system-installed
#     DirectShow COM filter (UnityCapture), so no DLLs need bundling.
#   - cv2 wheels ship their own DLLs; PyInstaller's cv2 hook handles collection.
#   - PyQt6 is well-supported but we explicitly collect it to avoid missing plugins.

a = Analysis(
    ['phonecam_desktop.py'],
    pathex=[],
    binaries=[],
    datas=[],
    hiddenimports=[
        'pyvirtualcam',
        'pyvirtualcam.backends',
        'cv2',
        'numpy',
        'PyQt6.sip',
        'PyQt6.QtCore',
        'PyQt6.QtGui',
        'PyQt6.QtWidgets',
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

# Collect all PyQt6 data (platform plugins, styles, etc.)
from PyInstaller.utils.hooks import collect_all  # noqa: E402
qt_datas, qt_bins, qt_hidden = collect_all('PyQt6')
a.datas    += qt_datas
a.binaries += qt_bins
a.hiddenimports += qt_hidden

pyz = PYZ(a.pure)

exe = EXE(
    pyz,
    a.scripts,
    a.binaries,
    a.datas,
    [],
    name='PhoneCamDesktop',
    debug=False,
    bootloader_ignore_signals=False,
    strip=False,
    upx=True,
    upx_exclude=[],
    runtime_tmpdir=None,
    console=False,
    disable_windowed_traceback=False,
    argv_emulation=False,
    target_arch=None,
    codesign_identity=None,
    entitlements_file=None,
    icon=None,
)
