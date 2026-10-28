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
#     Widgets, Svg, and Multimedia for the webcam list) and their plugins, not all of PyQt6. Multimedia runs on its
#     Media Foundation backend (QT_MEDIA_BACKEND in main.py), so its FFmpeg plugin and DLLs are left out.
#   - No UPX: compressed DLLs have to be unpacked again on every load, and virus scanners look harder at them.
#   - pyvirtualcam's unitycapture backend calls into a system-installed
#     DirectShow COM filter (UnityCapture), so no DLLs need bundling. The Windows 11 camera's DLL
#     (virtualcamera/) is built with CMake and copied next to the exe by build-windows.yml, not by this spec.
#   - cv2 wheels ship their own DLLs; PyInstaller's cv2 hook handles collection.
#   - Windows checks every program file it hasn't seen before when it first loads, and that takes seconds per file
#     (minutes for the first start after an install). So the bundle holds as few files as it can: zeroconf runs as
#     plain Python instead of its ~18 compiled modules, and what the app never uses is left out (see below).
#   - build-windows.yml installs PyInstaller with a launcher built by scripts/build_bootloader.py, which says to
#     extract the zip first when the exe is opened from inside it. A stock PyInstaller builds a working app without that.
#   - The exe's icon is resources/telescope.ico, the app icon (create_app_icon); scripts/write_icon.py
#     writes it again if the icon changes.

import importlib.util
import os
import sys

from PyInstaller.utils.hooks import collect_submodules

sys.path.insert(0, SPECPATH)
from telescope.updates import LIB_PREFIX  # noqa: E402
from telescope.version import BUILD  # noqa: E402

# Mic noise reduction loads pyrnnoise's RNNoise DLL itself (telescope/denoise.py), so only the DLL comes along
_rnnoise = importlib.util.find_spec('pyrnnoise')
_rnnoise_dll = os.path.join(_rnnoise.submodule_search_locations[0], 'rnnoise.dll') if _rnnoise else ''
if not os.path.isfile(_rnnoise_dll):
    raise SystemExit('pyrnnoise is missing: pip install --no-deps -r requirements-nodeps.txt')

a = Analysis(
    ['main.py'],
    pathex=[],
    binaries=[(_rnnoise_dll, 'pyrnnoise')],
    # The Browser camera page, which vision pack this build downloads for Auto framing and Background,
    # and the translations
    datas=[('telescope/web', 'telescope/web'), ('telescope/vision_pack.json', 'telescope'),
           ('telescope/locale', 'telescope/locale')],
    # ifaddr picks its platform backend behind an `os.name` check, so pull
    # the whole package rather than relying on that branch being followed.
    # dnspython (dns), which aioice uses, loads its record types by name
    hiddenimports=collect_submodules('telescope') + collect_submodules('ifaddr') + collect_submodules('zeroconf') + collect_submodules('av') + collect_submodules('cryptography') + collect_submodules('aiortc') + collect_submodules('aioice') + collect_submodules('dns') + [
        'sounddevice',  # hooks-contrib's hook collects its PortAudio DLL
        'pyvirtualcam',
        'cv2',
        'numpy',
        'PyQt6.sip',
        'PyQt6.QtCore',
        'PyQt6.QtGui',
        'PyQt6.QtWidgets',
        'PyQt6.QtSvg',
        'PyQt6.QtMultimedia',  # imported only once the window is up (telescope/webcams.py)
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

# zeroconf ships each of its hot-path modules twice, compiled (Cython) and as plain Python, and works either way.
# The compiled ones are only a speed-up for a busy network and cost a first start 18 more files to check, so the
# bundle takes the plain ones: they go into the PYZ and the compiled files stay out.
def use_plain_zeroconf(analysis):
    compiled, plain = set(), []
    for dest, src, kind in analysis.binaries:
        parts = dest.replace('\\', '/').split('/')
        if kind != 'EXTENSION' or parts[0] != 'zeroconf':
            continue
        module = parts[:-1] + [parts[-1].split('.')[0]]
        source = os.path.join(os.path.dirname(src), module[-1] + '.py')
        if not os.path.isfile(source):
            raise SystemExit(f'zeroconf has no plain Python for {dest}')
        if module[-1] == '__init__':
            module.pop()
        compiled.add(dest)
        plain.append(('.'.join(module), source, 'PYMODULE'))
    analysis.binaries = [b for b in analysis.binaries if b[0] not in compiled]
    known = {name for name, _, _ in analysis.pure}
    analysis.pure.extend(entry for entry in plain if entry[0] not in known)


# Files the app never opens: cv2 reads video files through the first, the next three are PortAudio builds for
# 32-bit, ARM and ASIO, Telescope doesn't load PDFs as images, and Qt Multimedia runs without FFmpeg (PyAV has its own)
NEVER_USED = ('cv2/opencv_videoio_ffmpeg',
              '_sounddevice_data/portaudio-binaries/libportaudio32bit',
              '_sounddevice_data/portaudio-binaries/libportaudioarm64',
              '_sounddevice_data/portaudio-binaries/libportaudio64bit-asio',
              'PyQt6/Qt6/bin/Qt6Pdf', 'PyQt6/Qt6/plugins/imageformats/qpdf',
              'PyQt6/Qt6/plugins/multimedia/ffmpegmediaplugin', 'PyQt6/Qt6/bin/avcodec', 'PyQt6/Qt6/bin/avformat',
              'PyQt6/Qt6/bin/avutil', 'PyQt6/Qt6/bin/swresample', 'PyQt6/Qt6/bin/swscale')

use_plain_zeroconf(a)
a.binaries = [b for b in a.binaries if not b[0].replace('\\', '/').startswith(NEVER_USED)]

pyz = PYZ(a.pure)

# Shown by the launcher straight away, before Python starts: a first start takes a minute or more to get the window up.
# telescope/splash.py puts the text on in the user's language and closes it once the window shows
splash = Splash(
    os.path.join(SPECPATH, 'resources', 'splash.png'),
    binaries=a.binaries,
    datas=a.datas,
    text_pos=(28, 156),  # bottom left, at write_splash.py's margin; the text grows upwards from here
    text_size=10,
    text_color='#a1abb8',
    text_default='Starting Telescope…',
    always_on_top=False,
)

exe = EXE(
    pyz,
    a.scripts,
    splash,
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
    splash.binaries,
    strip=False,
    upx=False,
    name='TelescopeDesktop',
)
