# -*- mode: python ; coding: utf-8 -*-
#
# PyInstaller spec for Telescope Desktop (Windows).
# Build: pyinstaller telescope.spec
# Output: dist/TelescopeDesktop.exe  (~60-80 MB onefile)
#
# Notes:
#   - pyvirtualcam's unitycapture backend calls into a system-installed
#     DirectShow COM filter (UnityCapture), so no DLLs need bundling.
#   - cv2 wheels ship their own DLLs; PyInstaller's cv2 hook handles collection.
#   - collect_all results are passed into Analysis directly; in PyInstaller 6.x
#     appending them to a.datas/a.binaries after the fact causes a 2-vs-3-tuple
#     mismatch in normalize_toc.

from PyInstaller.utils.hooks import collect_all, collect_submodules

qt_datas, qt_bins, qt_hidden = collect_all('PyQt6')
mat_datas, mat_bins, mat_hidden = collect_all('qt_material')

a = Analysis(
    ['main.py'],
    pathex=[],
    binaries=qt_bins + mat_bins,
    datas=qt_datas + mat_datas,
    hiddenimports=qt_hidden + mat_hidden + collect_submodules('telescope') + [
        'pyvirtualcam',
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

pyz = PYZ(a.pure)

exe = EXE(
    pyz,
    a.scripts,
    a.binaries,
    a.datas,
    [],
    name='TelescopeDesktop',
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
