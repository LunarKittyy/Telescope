#!/usr/bin/env python3
"""Build the vision pack (MediaPipe and the two models) for Linux and Windows, from requirements-vision.txt.

    python scripts/build_vision_pack.py --check            # CI: both packs still match telescope/vision_pack.json
    python scripts/build_vision_pack.py --write            # after changing a pin: update vision_pack.json
    python scripts/build_vision_pack.py --out dist/vision  # also write the release zips there
    python scripts/build_vision_pack.py --install          # running from source: unpack this computer's pack

Works from any OS (pip downloads the other platform's wheels).
"""
import argparse
import hashlib
import json
import shutil
import subprocess
import sys
import tempfile
import urllib.request
import zipfile
from pathlib import Path

DESKTOP = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(DESKTOP))

from telescope import vision_pack  # noqa: E402  (no OpenCV: the release runs this on a bare Python)

PLATFORMS = {"linux": "manylinux_2_28_x86_64", "windows": "win_amd64"}
_MODELS = "https://storage.googleapis.com/mediapipe-models"
MODELS = {  # name: (url, sha256); version 1 of each, as tested
    vision_pack.SEGMENTER_MODEL: (f"{_MODELS}/image_segmenter/selfie_segmenter_landscape/float16/1/"
                             f"selfie_segmenter_landscape.tflite",
                             "490e9ea734313e0de10fa0cd9e3c6133e36ea4db2b7a49bde9ef019f72796b8e"),
    vision_pack.POSE_MODEL: (f"{_MODELS}/pose_landmarker/pose_landmarker_lite/float16/1/pose_landmarker_lite.task",
                        "59929e1d1ee95287735ddd833b19cf4ac46d29bc7afddbbf6753c459690d574a"),
}
NOTICES = """Telescope's vision pack, used by the Auto framing and Background plugins.

MediaPipe (https://github.com/google-ai-edge/mediapipe), Apache License 2.0
The selfie segmenter and pose landmarker models from MediaPipe, Apache License 2.0
absl-py (https://github.com/abseil/abseil-py), Apache License 2.0
flatbuffers (https://github.com/google/flatbuffers), Apache License 2.0
certifi (https://github.com/certifi/python-certifi), Mozilla Public License 2.0

Each package's full licence text is in its .dist-info folder under site/.
"""
_EPOCH = (1980, 1, 1, 0, 0, 0)


def pins() -> list:
    lines = (DESKTOP / "requirements-vision.txt").read_text().splitlines()
    return [ln.strip() for ln in lines if ln.strip() and not ln.startswith("#")]


def fetch_models(folder: Path, cache: Path):
    folder.mkdir(parents=True)
    for name, (url, sha256) in MODELS.items():
        cached = cache / name
        if not cached.is_file() or hashlib.sha256(cached.read_bytes()).hexdigest() != sha256:
            with urllib.request.urlopen(url, timeout=60) as r:
                cached.write_bytes(r.read())
        data = cached.read_bytes()
        if hashlib.sha256(data).hexdigest() != sha256:
            sys.exit(f"{name} from {url} doesn't match its pinned sha256")
        (folder / name).write_bytes(data)


def build(key: str, work: Path, cache: Path) -> Path:
    """The pack for one platform, unpacked into work/key."""
    wheels = cache / f"wheels-{key}"
    subprocess.run([sys.executable, "-m", "pip", "download", "--quiet", "--disable-pip-version-check", "--no-deps",
                    "--only-binary=:all:", "--platform", PLATFORMS[key], "--platform", "any",
                    "--python-version", "3.11", "--implementation", "py", "-d", str(wheels), *pins()], check=True)
    root = work / key
    site = root / "site"
    site.mkdir(parents=True)
    wanted = {p.split("==")[0].lower().replace("-", "_") for p in pins()}
    for wheel in sorted(wheels.glob("*.whl")):
        if wheel.name.split("-")[0].lower() not in wanted:
            continue
        with zipfile.ZipFile(wheel) as z:
            z.extractall(site)
    fetch_models(root / "models", cache)
    (root / "NOTICES.txt").write_text(NOTICES)
    return root


def write_zip(root: Path, dest: Path):
    """Same files in the same order with the same dates, so it only changes when what's in it does."""
    with zipfile.ZipFile(dest, "w", zipfile.ZIP_DEFLATED, compresslevel=9) as z:
        for path in sorted(p for p in root.rglob("*") if p.is_file()):
            info = zipfile.ZipInfo(path.relative_to(root).as_posix(), _EPOCH)
            info.compress_type = zipfile.ZIP_DEFLATED
            info.external_attr = 0o644 << 16
            z.writestr(info, path.read_bytes(), compresslevel=9)


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--check", action="store_true", help="fail when a pack doesn't match vision_pack.json")
    ap.add_argument("--write", action="store_true", help="write vision_pack.json from what was built")
    ap.add_argument("--out", type=Path, help="write the release zips into this folder")
    ap.add_argument("--install", action="store_true", help="unpack this computer's pack where Telescope looks")
    ap.add_argument("--cache", type=Path, default=DESKTOP / "build" / "vision-cache")
    args = ap.parse_args()
    args.cache.mkdir(parents=True, exist_ok=True)
    info, problems = {}, []
    try:
        known = json.loads(vision_pack.PACK_INFO.read_text())
    except (OSError, ValueError):
        known = {}
    with tempfile.TemporaryDirectory() as tmp:
        work = Path(tmp)
        for key in PLATFORMS:
            root = build(key, work, args.cache)
            tree = vision_pack.tree_hash(root)
            zipped = work / f"{key}.zip"
            write_zip(root, zipped)
            files = sum(1 for p in root.rglob("*") if p.is_file())
            info[key] = {"id": tree, "size": zipped.stat().st_size, "files": files}
            print(f"{key}: id {tree[:16]}, {files} files, {zipped.stat().st_size / 1e6:.1f} MB zipped")
            if args.check and known.get(key, {}).get("id") != tree:
                problems.append(key)
            if args.out:
                args.out.mkdir(parents=True, exist_ok=True)
                name = f"Telescope-vision-{key}-{tree[:16]}.zip"
                shutil.move(zipped, args.out / name)  # the temp folder can be on another filesystem
                print(f"  wrote {args.out / name}")
            if args.install and key == vision_pack.platform_key():
                dest = vision_pack.default_dir() / tree[:16]
                if dest.exists():
                    shutil.rmtree(dest)
                dest.parent.mkdir(parents=True, exist_ok=True)
                shutil.move(root, dest)
                (dest / ".verified").write_text(tree[:16])
                print(f"  installed in {dest}")
    if args.write:
        vision_pack.PACK_INFO.write_text(json.dumps(info, indent=2, sort_keys=True) + "\n")
        print(f"wrote {vision_pack.PACK_INFO}")
    if problems:
        sys.exit(f"The {' and '.join(problems)} pack doesn't match telescope/vision_pack.json. If a pin in "
                 f"requirements-vision.txt changed on purpose, run this with --write and commit the result.")


if __name__ == "__main__":
    main()
