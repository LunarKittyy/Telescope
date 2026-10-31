#!/usr/bin/env python3
"""The Rust packages' smoke check: what's in them, what the updater needs, and that the program in them starts and knows its own stamp.
Tests run from the source tree, so only this notices something the package left out.

Usage: check_package.py [--version 4.2.0 --build 123 --channel nightly] [--no-run] <folder, .tar.gz or .zip>...
A folder or a .tar.gz on Linux and a folder or a .zip on Windows also run; --no-run looks at a package from the other system.
"""

import argparse
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
LINUX_FILES = ("start.sh", "main.py", "telescope/_build.py", "telescope-rs", "librnnoise.so", "THIRD_PARTY_NOTICES.txt", "ui-kit/ui-kit.slint",
               "lib/fallback/libva.so.2", "lib/fallback/libva-drm.so.2", "lib/fallback/libdrm.so.2")
LINUX_LIBRARIES = ("lib/libavcodec.so.", "lib/libavutil.so.", "lib/libswscale.so.")
# Python's windows updater wants the exe; check.ps1 and the rest of Telescope.iss want the other two folders
WINDOWS_FILES = ("TelescopeDesktop.exe", "rnnoise.dll", "THIRD_PARTY_NOTICES.txt", "ui-kit/ui-kit.slint", "virtualcamera/TelescopeCamera.dll",
                 "unitycapture/UnityCaptureFilter64.dll", "unitycapture/UnityCaptureFilter32.dll", "unitycapture/LICENSE")
WINDOWS_LIBRARIES = ("avcodec-", "avutil-", "swscale-")
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


def check_stamp(name: str, stamp: dict, args):
    if args.version and (stamp.get("version"), stamp.get("build"), stamp.get("channel")) != (args.version, args.build, args.channel):
        problems.append(f"{name} says {stamp}")


def build_py_stamp(folder: Path) -> dict:
    stamp = {}
    exec((folder / "telescope" / "_build.py").read_text(), stamp)
    return {key.lower(): stamp.get(key) for key in ("VERSION", "BUILD", "CHANNEL", "COMMIT")}


def run_info(program: Path, env=None) -> dict:
    result = subprocess.run([str(program), "build-info"], capture_output=True, text=True, timeout=60, env=env)
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
    for lib in ("libavcodec", "libavutil", "libswscale"):
        if not re.search(rf"{lib}\.so\.\d+ => {re.escape(str(folder.resolve()))}/lib/", linked):
            problems.append(f"{name}'s program doesn't find its own {lib}: {linked}")
    # Through start.sh, as the app's entries and the updater do: it's also what finds lib/fallback on a system without libva
    if run_info(folder / "start.sh") != stamp:
        problems.append(f"{name}'s program and _build.py disagree")


def check_windows(name: str, folder: Path, args):
    names = files_in(folder)
    check_common(name, folder, names, WINDOWS_FILES)
    check_libraries(name, folder, names, WINDOWS_LIBRARIES)
    if not (folder / "TelescopeDesktop.exe").is_file() or args.no_run or sys.platform != "win32":
        return
    check_stamp(f"{name}'s exe", run_info(folder / "TelescopeDesktop.exe"), args)


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
        folder = unpacked(package, keep)
        windows = (folder / "TelescopeDesktop.exe").is_file() or package.suffix == ".zip"
        (check_windows if windows else check_linux)(package.name, folder, args)
    for p in problems:
        print(f"::error::{p}")
    print(f"{len(problems)} problem(s) in the packages" if problems else "The packages check out")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
