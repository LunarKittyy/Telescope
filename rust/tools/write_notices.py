#!/usr/bin/env python3
"""Writes the package's THIRD_PARTY_NOTICES.txt: what it carries, FFmpeg's build and licence, and every Rust crate in the program.

Usage: write_notices.py windows|linux FFMPEG_DIR OUT
"""

import json
import subprocess
import sys
from pathlib import Path

RUST = Path(__file__).resolve().parents[1]
TARGETS = {"windows": "x86_64-pc-windows-msvc", "linux": "x86_64-unknown-linux-gnu"}

HEADER = """Telescope - Third-Party Notices
================================

This file lists third-party software distributed with or required to run
Telescope. Telescope's own license is unchanged by anything below - see
LICENSE in the repository root.

Bundled binaries
-----------------

FFmpeg (libavcodec, libavutil, libswresample, libswscale)
    Decodes the phone's video. A decoder-only build under the GNU LGPL v2.1 or
    later, with no GPL or non-free parts. Its licence is at the end of this
    file. How it was built:
    {ffmpeg}
    Source: https://ffmpeg.org/download.html
    You can replace these libraries with your own build of the same major
    version, and rust/tools/build_ffmpeg.sh in Telescope's repository is the
    exact recipe.

RNNoise (pyrnnoise's wheel)
    Noise reduction for the microphone.
    Copyright (c) Xiph.Org Foundation, Mozilla Corporation and others.
    License: BSD 3-Clause. Source: https://github.com/xiph/rnnoise
"""

WINDOWS = """
UnityCapture (unitycapture/)
    DirectShow virtual camera filter for Windows.
    Copyright (c) 2018 Bernhard Schelling.
    License: MIT (see unitycapture/LICENSE).
    Source: https://github.com/schellingb/UnityCapture

Windows 11 camera (virtualcamera/TelescopeCamera.dll, source in desktop/virtualcamera/)
    Telescope's own code, under Telescope's license. Its structure follows
    Microsoft's VirtualCamera sample.
    Sample: Copyright (c) Microsoft Corporation. License: MIT.
    Source: https://github.com/microsoft/Windows-Camera/tree/master/Samples/VirtualCamera

Microsoft Visual C++ runtime (vcruntime140.dll)
    Copyright (c) Microsoft Corporation.
    Redistributed under the Microsoft Visual Studio license terms, for RNNoise.

Android SDK Platform Tools (not included)
    On Windows, adb.exe is downloaded from Google when you ask for it (Get adb), and kept in
    %LOCALAPPDATA%\\Telescope\\platform-tools with Google's NOTICE.txt.
    Copyright (c) Google LLC.
    License: Android Software Development Kit License Agreement.
    https://developer.android.com/studio/terms
"""

LINUX = """
libva and libdrm (lib/fallback/, used only on a system without its own)
    Reach the graphics card for hardware decoding.
    License: MIT. Source: https://github.com/intel/libva, https://gitlab.freedesktop.org/mesa/drm

fontconfig, FreeType, Expat, libpng, Brotli, libuuid and zlib (lib/fallback-fonts/, used only on a system without its own)
    Find the system's fonts. As Ubuntu 22.04 ships them.
    License: MIT (fontconfig, Expat, Brotli), FreeType License, libpng License, BSD 3-Clause (libuuid), zlib License.
    Source: https://gitlab.freedesktop.org/fontconfig/fontconfig, https://freetype.org, https://libexpat.github.io,
    http://www.libpng.org, https://github.com/google/brotli, https://github.com/util-linux/util-linux, https://zlib.net

AppImage runtime (Telescope-x86_64.AppImage only)
    Mounts the AppImage. Statically linked with libfuse, squashfuse and zstd.
    License: MIT, with libfuse under the LGPL 2.1 and squashfuse under a BSD licence. One message is reworded, nothing else.
    Source: https://github.com/AppImage/type2-runtime
"""

CRATES = """
Rust crates
-----------

These are compiled into the program. Each crate's own repository has the
authoritative license text. Slint is used under the GNU GPL v3, which allows
combining with Telescope's AGPL-3.0.

"""


def crates(target: str) -> list:
    """Every crate the program's own code reaches on this target, without what's only for tests or build scripts."""
    data = json.loads(subprocess.run(["cargo", "metadata", "--locked", "--format-version", "1", "--filter-platform", target],
                                     cwd=RUST, capture_output=True, encoding="utf-8", check=True).stdout)
    packages = {p["id"]: p for p in data["packages"]}
    nodes = {n["id"]: n for n in data["resolve"]["nodes"]}
    start = next(i for i, p in packages.items() if p["name"] == "telescope-app")
    seen, todo = set(), [start]
    while todo:
        pid = todo.pop()
        if pid in seen:
            continue
        seen.add(pid)
        for dep in nodes[pid]["deps"]:
            if any(k["kind"] is None for k in dep["dep_kinds"]):
                todo.append(dep["pkg"])
    ours = [p for p in (packages[i] for i in seen) if not p["name"].startswith("telescope-")]
    return sorted(f"{p['name']} {p['version']}: {p['license'] or 'see its repository'}" for p in ours)


def main():
    target, ffmpeg, out = sys.argv[1], Path(sys.argv[2]), Path(sys.argv[3])
    build = (ffmpeg / "BUILD.txt").read_text(encoding="utf-8").strip().replace("\n", "\n    ")
    text = HEADER.format(ffmpeg=build) + (WINDOWS if target == "windows" else LINUX) + CRATES
    text += "\n".join(crates(TARGETS[target]))
    text += "\n\n\nFFmpeg's licence\n----------------\n\n" + (ffmpeg / "COPYING.LGPLv2.1").read_text(encoding="utf-8")
    out.write_text(text, encoding="utf-8")


if __name__ == "__main__":
    main()
