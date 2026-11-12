#!/usr/bin/env python3
"""The Rust packages' smoke check: what's in them, what the updater needs, and that the program in them starts and knows its own stamp.
Tests run from the source tree, so only this notices something the package left out.

Usage: check_package.py [--version 4.2.0 --build 123 --channel nightly] [--no-run] <folder, .tar.gz, .zip or .AppImage>...
A folder or a .tar.gz on Linux and a folder or a .zip on Windows also run, as does an AppImage on Linux; --no-run looks at a package from the other system.
"""

import argparse
import ctypes
import json
import os
import re
import stat
import subprocess
import sys
import tarfile
import tempfile
import zipfile
from pathlib import Path


# The Python updater turns a Linux tarball without main.py and telescope/ away (telescope/updates.py's install_linux)
LINUX_FILES = ("start.sh", "main.py", "telescope/_build.py", "telescope-rs", "librnnoise.so", "THIRD_PARTY_NOTICES.txt", "ui-kit/ui-kit.slint", "ui-kit/fonts/IBMPlexSans-Regular.ttf", "ui-kit/fonts/IBMPlexSans-SemiBold.ttf", "ui-kit/fonts/OFL.txt", "telescope.png",
               "lib/fallback/libva.so.2", "lib/fallback/libva-drm.so.2", "lib/fallback/libdrm.so.2", "lib/fallback-fonts/libfontconfig.so.1",
               "lib/fallback-fonts/libfreetype.so.6", "lib/fallback-fonts/fonts.conf",
               "lib/gphoto2/libgphoto2.so.6", "lib/gphoto2/libgphoto2_port.so.12", "lib/gphoto2/camlibs/ptp2.so", "lib/gphoto2/iolibs/usb1.so")
# What makes an AppImage one, and what the program needs at run time; the tarball's start.sh and Python files stay out of it
APPIMAGE_FILES = ("AppRun", "telescope.desktop", "telescope.png", "telescope-rs", "librnnoise.so", "THIRD_PARTY_NOTICES.txt", "ui-kit/ui-kit.slint", "ui-kit/fonts/IBMPlexSans-Regular.ttf", "ui-kit/fonts/IBMPlexSans-SemiBold.ttf", "ui-kit/fonts/OFL.txt",
                  "lib/fallback/libva.so.2", "lib/fallback/libva-drm.so.2", "lib/fallback/libdrm.so.2", "lib/fallback-fonts/libfontconfig.so.1",
                  "lib/fallback-fonts/libfreetype.so.6", "lib/fallback-fonts/fonts.conf",
                  "lib/gphoto2/libgphoto2.so.6", "lib/gphoto2/libgphoto2_port.so.12", "lib/gphoto2/camlibs/ptp2.so", "lib/gphoto2/iolibs/usb1.so")
LINUX_LIBRARIES = ("lib/libavcodec.so.", "lib/libavutil.so.", "lib/libswresample.so.", "lib/libswscale.so.")
# Python's windows updater wants the exe; check.ps1 and the rest of Telescope.iss want the other two folders
WINDOWS_FILES = ("TelescopeDesktop.exe", "rnnoise.dll", "vcruntime140.dll", "THIRD_PARTY_NOTICES.txt", "ui-kit/ui-kit.slint", "ui-kit/fonts/IBMPlexSans-Regular.ttf", "ui-kit/fonts/IBMPlexSans-SemiBold.ttf", "ui-kit/fonts/OFL.txt", "virtualcamera/TelescopeCamera.dll",
                 "unitycapture/UnityCaptureFilter64.dll", "unitycapture/UnityCaptureFilter32.dll", "unitycapture/LICENSE")
WINDOWS_LIBRARIES = ("avcodec-", "avutil-", "swresample-", "swscale-")
# What the app asks FFmpeg for (media/src/ffmpeg.rs, opus.rs and h264.rs), so a recipe change that drops one shows up here and not on someone's phone
DECODERS = (b"h264", b"hevc", b"mjpeg", b"vp8", b"opus", b"png", b"webp", b"bmp")
HARDWARE = {"linux": (b"vaapi", b"cuda"), "win32": (b"d3d11va", b"cuda")}
# No GPL parts in what ships, and adb's licence doesn't allow passing it on
FORBIDDEN = re.compile(r"(^|/)(libx264|libx265|x264|avfilter|avdevice|postproc|adb)[^/]*$", re.I)
# More than this and the GPL build got in by mistake (it is about 170 MB)
FFMPEG_LIMIT = 16 * 1024 * 1024

problems = []


def unpacked(package: Path, keep: list) -> Path:
    if package.is_dir():
        return package
    work = tempfile.TemporaryDirectory(prefix="telescope-package-")
    keep.append(work)
    if package.suffix == ".zip":
        with zipfile.ZipFile(package) as z:
            z.extractall(work.name)
    else:
        with tarfile.open(package) as tar:
            tar.extractall(work.name, filter="data")
    return Path(work.name)


def files_in(folder: Path) -> set:
    return {p.relative_to(folder).as_posix() for p in folder.rglob("*") if p.is_file()}


def check_common(name: str, folder: Path, names: set, required: tuple):
    for f in required:
        if f not in names:
            problems.append(f"{name} has no {f}")
    for f in sorted(names):
        if FORBIDDEN.search(f):
            problems.append(f"{name} shouldn't carry {f}")
    notices = folder / "THIRD_PARTY_NOTICES.txt"
    if notices.is_file():
        text = notices.read_text(encoding="utf-8")
        for needed in ("GNU LESSER GENERAL PUBLIC LICENSE", "FFmpeg 8", "slint"):
            if needed not in text:
                problems.append(f"{name}'s notices don't mention {needed}")
        if re.search(r"--enable-(gpl|nonfree|libx264)", text):
            problems.append(f"{name}'s FFmpeg isn't the LGPL build")


def check_libraries(name: str, folder: Path, names: set, prefixes: tuple):
    total = 0
    for prefix in prefixes:
        found = [f for f in names if f.startswith(prefix)]
        if len(found) != 1:
            problems.append(f"{name} has {found or 'nothing'} for {prefix}*")
        total += sum((folder / f).stat().st_size for f in found)
    if total > FFMPEG_LIMIT:
        problems.append(f"{name}'s FFmpeg is {total // (1024 * 1024)} MB, which is no decoder-only build")


def check_ffmpeg_holds(name: str, libraries: list):
    """Loads the libraries in order, avutil then avcodec last, and asks them what the app will."""
    loaded = [ctypes.CDLL(str(path)) for path in libraries]
    avutil, avcodec = loaded[-2], loaded[-1]
    avcodec.avcodec_find_decoder_by_name.restype = ctypes.c_void_p
    avcodec.avcodec_find_decoder_by_name.argtypes = [ctypes.c_char_p]
    avutil.av_hwdevice_find_type_by_name.argtypes = [ctypes.c_char_p]
    for decoder in DECODERS:
        if not avcodec.avcodec_find_decoder_by_name(decoder):
            problems.append(f"{name}'s FFmpeg has no {decoder.decode()} decoder")
    for device in HARDWARE[sys.platform]:
        if avutil.av_hwdevice_find_type_by_name(device) <= 0:
            problems.append(f"{name}'s FFmpeg can't use {device.decode()}")


def check_stamp(name: str, stamp: dict, args):
    if args.version and (stamp.get("version"), stamp.get("build"), stamp.get("channel")) != (args.version, args.build, args.channel):
        problems.append(f"{name} says {stamp}")


def build_py_stamp(folder: Path) -> dict:
    stamp = {}
    exec((folder / "telescope" / "_build.py").read_text(), stamp)
    return {key.lower(): stamp.get(key) for key in ("VERSION", "BUILD", "CHANNEL", "COMMIT")}


def run_info(program: Path, env=None, flags=()) -> dict:
    result = subprocess.run([str(program), *flags, "build-info"], capture_output=True, text=True, timeout=60, env=env)
    if result.returncode != 0:
        problems.append(f"{program.name} build-info exited with {result.returncode}: {result.stderr.strip()}")
        return {}
    return json.loads(result.stdout)


def check_linux(name: str, folder: Path, args):
    names = files_in(folder)
    check_common(name, folder, names, LINUX_FILES)
    check_libraries(name, folder, names, LINUX_LIBRARIES)
    stamp = build_py_stamp(folder)
    check_stamp(f"{name}'s _build.py", stamp, args)
    if args.no_run or sys.platform != "linux":
        return
    os.chmod(folder / "start.sh", 0o755)
    os.chmod(folder / "telescope-rs", 0o755)
    linked = subprocess.run(["ldd", str(folder / "telescope-rs")], capture_output=True, text=True).stdout
    for lib in ("libavcodec", "libavutil", "libswresample", "libswscale"):
        if not re.search(rf"{lib}\.so\.\d+ => {re.escape(str(folder.resolve()))}/lib/", linked):
            problems.append(f"{name}'s program doesn't find its own {lib}: {linked}")
    # Through start.sh, as the app's entries and the updater do: it's also what finds lib/fallback on a system without libva
    if run_info(folder / "start.sh") != stamp:
        problems.append(f"{name}'s program and _build.py disagree")
    lib = folder.resolve() / "lib"
    # The system's libva when it has one, as start.sh does
    va = [lib / "fallback" / f for f in ("libdrm.so.2", "libva.so.2", "libva-drm.so.2")] if "not found" in linked else []
    check_ffmpeg_holds(name, va + [next(lib.glob("libavutil.so.*")), next(lib.glob("libavcodec.so.*"))])
    check_gphoto2(name, folder)


def check_gphoto2(name: str, folder: Path):
    """The bundled libgphoto2 and its two drivers must resolve everything they link to, against the folder and the system (no camera needed)."""
    gp = folder.resolve() / "lib" / "gphoto2"
    for lib in ("libgphoto2.so.6", "libgphoto2_port.so.12", "camlibs/ptp2.so", "iolibs/usb1.so"):
        env = dict(os.environ, LD_LIBRARY_PATH=str(gp))
        out = subprocess.run(["ldd", str(gp / lib)], capture_output=True, text=True, env=env).stdout
        if "not found" in out:
            problems.append(f"{name}'s lib/gphoto2/{lib} lacks something it links to: {out}")


def check_appimage(image: Path, args):
    name = image.name
    with image.open("rb") as f:
        head = f.read(1 << 20)
    if head[:4] != b"\x7fELF" or head[8:11] != b"AI\x02":
        problems.append(f"{name} isn't a type 2 AppImage")
        return
    # A static runtime asks for no loader and no libfuse2, the whole point of it
    if b"ld-linux" in head or b"libfuse.so.2" in head:
        problems.append(f"{name}'s runtime isn't the static one")
    os.chmod(image, 0o755)
    with tempfile.TemporaryDirectory(prefix="telescope-appimage-") as work:
        # Unpacking needs no FUSE, which CI's containers don't have
        unpacked_in = subprocess.run([str(image.resolve()), "--appimage-extract"], cwd=work, capture_output=True, text=True, timeout=120)
        if unpacked_in.returncode != 0:
            problems.append(f"{name} won't unpack: {unpacked_in.stderr.strip()}")
            return
        folder = Path(work) / "squashfs-root"
        names = files_in(folder)
        check_common(name, folder, names, APPIMAGE_FILES)
        check_libraries(name, folder, names, LINUX_LIBRARIES)
        for entry in ("AppRun", "telescope-rs"):
            if not os.access(folder / entry, os.X_OK):
                problems.append(f"{name}'s {entry} can't run")
        if not (folder / ".DirIcon").exists():
            problems.append(f"{name} has no .DirIcon")
        desktop = (folder / "telescope.desktop").read_text(encoding="utf-8")
        for needed in ("Icon=telescope", "StartupWMClass=telescope"):
            if needed not in desktop:
                problems.append(f"{name}'s desktop entry has no {needed}")
        if args.no_run or sys.platform != "linux":
            return
        linked = subprocess.run(["ldd", str(folder / "telescope-rs")], capture_output=True, text=True).stdout
        for lib in ("libavcodec", "libavutil", "libswresample", "libswscale"):
            if not re.search(rf"{lib}\.so\.\d+ => {re.escape(str(folder.resolve()))}/lib/", linked):
                problems.append(f"{name}'s program doesn't find its own {lib}: {linked}")
    check_stamp(f"{name}'s program", run_info(image.resolve(), flags=("--appimage-extract-and-run",)), args)


def pe_imports(data: bytes) -> set[str]:
    """The DLLs a 64-bit Windows program loads at start, lower case. The program's own text may name others without needing them."""
    u16 = lambda at: int.from_bytes(data[at:at + 2], "little")
    u32 = lambda at: int.from_bytes(data[at:at + 4], "little")
    header = u32(0x3C)
    optional = header + 24
    sections = [optional + u16(header + 20) + 40 * i for i in range(u16(header + 6))]

    def offset(rva: int) -> int:
        for at in sections:
            start, size, raw = u32(at + 12), u32(at + 16), u32(at + 20)
            if start <= rva < start + size:
                return rva - start + raw
        raise ValueError(f"nothing is mapped at {rva:#x}")

    names, entry = set(), offset(u32(optional + 120))
    while u32(entry + 12):
        at = offset(u32(entry + 12))
        names.add(data[at:data.index(b"\0", at)].decode("ascii").lower())
        entry += 20
    return names


def check_windows(name: str, folder: Path, args):
    names = files_in(folder)
    check_common(name, folder, names, WINDOWS_FILES)
    check_libraries(name, folder, names, WINDOWS_LIBRARIES)
    # The Python app brought its own, but a clean Windows may have no VC++ redistributable, and then the program can't start or roll back
    if (folder / "TelescopeDesktop.exe").is_file():
        needs = pe_imports((folder / "TelescopeDesktop.exe").read_bytes())
        for dll in ("vcruntime140.dll", "vcruntime140_1.dll", "msvcp140.dll"):
            if dll in needs:
                problems.append(f"{name}'s program needs {dll}")
    if not (folder / "TelescopeDesktop.exe").is_file() or args.no_run or sys.platform != "win32":
        return
    check_stamp(f"{name}'s exe", run_info(folder / "TelescopeDesktop.exe"), args)
    os.add_dll_directory(folder.resolve())
    check_ffmpeg_holds(name, [next(folder.resolve().glob("avutil-*.dll")), next(folder.resolve().glob("avcodec-*.dll"))])


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--version")
    parser.add_argument("--build", type=int)
    parser.add_argument("--channel")
    parser.add_argument("--no-run", action="store_true")
    parser.add_argument("packages", nargs="+", type=Path)
    args = parser.parse_args()
    keep = []
    for package in args.packages:
        if package.suffix == ".AppImage":
            check_appimage(package, args)
            continue
        folder = unpacked(package, keep)
        windows = (folder / "TelescopeDesktop.exe").is_file() or package.suffix == ".zip"
        (check_windows if windows else check_linux)(package.name, folder, args)
    for p in problems:
        print(f"::error::{p}")
    print(f"{len(problems)} problem(s) in the packages" if problems else "The packages check out")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
