#!/usr/bin/env python3
"""Build the matting pack (ONNX Runtime and the Robust Video Matting model, for Background's High quality cutout) for Linux and Windows.

    python tools/build_matting_pack.py --check            # CI: both packs still match crates/vision/matting_pack.json
    python tools/build_matting_pack.py --write            # after changing a pin: update matting_pack.json
    python tools/build_matting_pack.py --out ../vision    # also write the release zips there

Works from any OS: every file is downloaded from a pinned address and checked against a pinned sha256. The pack's folder tree is hashed the way
build_vision_pack.py and crates/vision/src/pack.rs do, so the zip itself can differ without the pack changing.
"""
import argparse
import hashlib
import io
import json
import shutil
import sys
import tarfile
import tempfile
import urllib.request
import zipfile
from pathlib import Path

from build_vision_pack import tree_hash, write_zip

TOOLS = Path(__file__).resolve().parent
PACK_INFO = TOOLS.parent / "crates" / "vision" / "matting_pack.json"
ORT = "1.31.1"
_ORT = f"https://github.com/microsoft/onnxruntime/releases/download/v{ORT}"
_RVM = "https://github.com/PeterL1n/RobustVideoMatting"
MODEL = "rvm_mobilenetv3_fp32.onnx"
# name: (url, sha256), downloaded as they are
FILES = {
    "ort-linux": (f"{_ORT}/onnxruntime-linux-x64-{ORT}.tgz", "c849e36a1ef3e22d4ff217b9d1ce4a3cf5b9467b975610913ae959ad5b16bdc9"),
    "ort-windows": (f"{_ORT}/onnxruntime-win-x64-{ORT}.zip", "5869e26bb25314375f2d5ef1877e3ea06b1543d6c710e6e4a1ec2f7177ab4b2c"),
    "model": (f"{_RVM}/releases/download/v1.0.0/{MODEL}", "88d4531297118f595bf2fd60f6f566aec2e559393802d1f436c380f0cbbd2828"),
    "rvm-licence": (f"https://raw.githubusercontent.com/PeterL1n/RobustVideoMatting/v1.0.0/LICENSE",
                    "8b1ba204bb69a0ade2bfcf65ef294a920f6bb361b317dba43c7ef29d96332b9b"),
}
PLATFORMS = ("linux", "windows")
# platform: (the archive's name in FILES, the folder inside it, [(file inside lib/, name in the pack)])
RUNTIME = {
    "linux": ("ort-linux", f"onnxruntime-linux-x64-{ORT}",
              [(f"libonnxruntime.so.{ORT}", "libonnxruntime.so"), ("libonnxruntime_providers_shared.so", "libonnxruntime_providers_shared.so")]),
    "windows": ("ort-windows", f"onnxruntime-win-x64-{ORT}",
                [("onnxruntime.dll", "onnxruntime.dll"), ("onnxruntime_providers_shared.dll", "onnxruntime_providers_shared.dll")]),
}
NOTICES = f"""Telescope's matting pack, used by the Background plugin's High quality cutout.

Robust Video Matting (https://github.com/PeterL1n/RobustVideoMatting), the model rvm_mobilenetv3_fp32.onnx
    Copyright (C) Peter Lin. GNU General Public License v3.0, the full text is licenses/RVM-LICENSE.txt.
    The model's source is in that repository; it is used here as the project released it, without changes.
ONNX Runtime {ORT} (https://github.com/microsoft/onnxruntime), the files in lib/
    Copyright (c) Microsoft Corporation. MIT License, in licenses/ONNXRUNTIME-LICENSE.txt, with the licences of what it
    includes in licenses/ONNXRUNTIME-ThirdPartyNotices.txt.
"""


def fetch(name: str, cache: Path) -> bytes:
    url, sha256 = FILES[name]
    cached = cache / f"{sha256[:16]}-{url.rsplit('/', 1)[-1]}"
    if not cached.is_file() or hashlib.sha256(cached.read_bytes()).hexdigest() != sha256:
        with urllib.request.urlopen(url, timeout=120) as r:
            cached.write_bytes(r.read())
    data = cached.read_bytes()
    if hashlib.sha256(data).hexdigest() != sha256:
        sys.exit(f"{name} from {url} doesn't match its pinned sha256")
    return data


def runtime_files(key: str, cache: Path) -> dict:
    """The library files of ONNX Runtime and its two licence files, by path inside the archive's folder."""
    archive, folder, _ = RUNTIME[key]
    data = fetch(archive, cache)
    wanted = {f"{folder}/lib/{src}" for src, _ in RUNTIME[key][2]} | {f"{folder}/LICENSE", f"{folder}/ThirdPartyNotices.txt"}
    found = {}
    if archive.endswith("linux"):
        with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as t:
            for member in t.getmembers():
                if member.name in wanted and member.isfile():
                    found[member.name] = t.extractfile(member).read()
    else:
        with zipfile.ZipFile(io.BytesIO(data)) as z:
            for name in z.namelist():
                if name in wanted:
                    found[name] = z.read(name)
    if set(found) != wanted:
        sys.exit(f"{archive} lacks {sorted(wanted - set(found))}")
    return found


def build(key: str, work: Path, cache: Path) -> Path:
    """The pack for one platform, unpacked into work/key."""
    root = work / key
    (root / "lib").mkdir(parents=True)
    (root / "models").mkdir()
    (root / "licenses").mkdir()
    _, folder, libs = RUNTIME[key]
    found = runtime_files(key, cache)
    for src, dest in libs:
        (root / "lib" / dest).write_bytes(found[f"{folder}/lib/{src}"])
    (root / "licenses" / "ONNXRUNTIME-LICENSE.txt").write_bytes(found[f"{folder}/LICENSE"])
    (root / "licenses" / "ONNXRUNTIME-ThirdPartyNotices.txt").write_bytes(found[f"{folder}/ThirdPartyNotices.txt"])
    (root / "models" / MODEL).write_bytes(fetch("model", cache))
    (root / "licenses" / "RVM-LICENSE.txt").write_bytes(fetch("rvm-licence", cache))
    (root / "NOTICES.txt").write_text(NOTICES, encoding="utf-8", newline="\n")
    return root


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--check", action="store_true", help="fail when a pack doesn't match matting_pack.json")
    ap.add_argument("--write", action="store_true", help="write matting_pack.json from what was built")
    ap.add_argument("--out", type=Path, help="write the release zips into this folder")
    ap.add_argument("--cache", type=Path, default=TOOLS.parent / "target" / "matting-cache")
    args = ap.parse_args()
    args.cache.mkdir(parents=True, exist_ok=True)
    info, problems = {}, []
    try:
        known = json.loads(PACK_INFO.read_text())
    except (OSError, ValueError):
        known = {}
    with tempfile.TemporaryDirectory() as tmp:
        work = Path(tmp)
        for key in PLATFORMS:
            root = build(key, work, args.cache)
            tree = tree_hash(root)
            zipped = work / f"{key}.zip"
            write_zip(root, zipped)
            files = sum(1 for p in root.rglob("*") if p.is_file())
            info[key] = {"id": tree, "size": zipped.stat().st_size, "files": files}
            unpacked = sum(p.stat().st_size for p in root.rglob("*") if p.is_file())
            print(f"{key}: id {tree[:16]}, {files} files, {zipped.stat().st_size / 1e6:.1f} MB zipped, {unpacked / 1e6:.1f} MB unpacked")
            if args.check and known.get(key, {}).get("id") != tree:
                problems.append(key)
            if args.out:
                args.out.mkdir(parents=True, exist_ok=True)
                name = f"Telescope-matting-{key}-{tree[:16]}.zip"
                shutil.move(zipped, args.out / name)  # the temp folder can be on another filesystem
                print(f"  wrote {args.out / name}")
    if args.write:
        PACK_INFO.write_text(json.dumps(info, indent=2, sort_keys=True) + "\n")
        print(f"wrote {PACK_INFO}")
    if problems:
        sys.exit(f"The {' and '.join(problems)} pack doesn't match crates/vision/matting_pack.json. If a pin in "
                 f"build_matting_pack.py changed on purpose, run this with --write and commit the result.")


if __name__ == "__main__":
    main()
