#!/usr/bin/env python3
"""Build the vision pack (MediaPipe and its three models) for Linux and Windows, from vision-pack-requirements.txt.

    python tools/build_vision_pack.py --check            # CI: both packs still match crates/vision/vision_pack.json
    python tools/build_vision_pack.py --write            # after changing a pin: update vision_pack.json
    python tools/build_vision_pack.py --out dist/vision  # also write the release zips there

Works from any OS (pip downloads the other platform's wheels). The pack's folder tree is hashed the way crates/vision/src/pack.rs does.
"""
import argparse
import hashlib
import json
import shutil
import struct
import subprocess
import sys
import tempfile
import urllib.request
import zipfile
from pathlib import Path

TOOLS = Path(__file__).resolve().parent
PACK_INFO = TOOLS.parent / "crates" / "vision" / "vision_pack.json"
SEGMENTER_MODEL = "selfie_segmenter_landscape.tflite"
POSE_MODEL = "pose_landmarker_lite.task"
FACE_MODEL = "blaze_face_short_range.tflite"
_DONE = ".verified"  # written last by the app, holding the pack's id: it is not part of the hash


def tree_hash(folder: Path) -> str:
    """What's in a folder, as one sha256 over each file's path and sha256 (the zip itself can differ by zlib)."""
    lines = []
    # WindowsPath sorts without case; use the same case-sensitive component order as PosixPath.
    for path in sorted((p for p in folder.rglob("*") if p.is_file()), key=lambda p: p.relative_to(folder).parts):
        rel = path.relative_to(folder).as_posix()
        if rel == _DONE:
            continue
        digest = hashlib.sha256()
        with path.open("rb") as f:
            for chunk in iter(lambda: f.read(1024 * 1024), b""):
                digest.update(chunk)
        lines.append(f"{rel}\0{digest.hexdigest()}\n")
    return hashlib.sha256("".join(lines).encode()).hexdigest()


PLATFORMS = {"linux": "manylinux_2_28_x86_64", "windows": "win_amd64"}
_MODELS = "https://storage.googleapis.com/mediapipe-models"
MODELS = {  # name: (url, sha256); version 1 of each, as tested
    SEGMENTER_MODEL: (f"{_MODELS}/image_segmenter/selfie_segmenter_landscape/float16/1/"
                             f"selfie_segmenter_landscape.tflite",
                             "490e9ea734313e0de10fa0cd9e3c6133e36ea4db2b7a49bde9ef019f72796b8e"),
    POSE_MODEL: (f"{_MODELS}/pose_landmarker/pose_landmarker_lite/float16/1/pose_landmarker_lite.task",
                        "59929e1d1ee95287735ddd833b19cf4ac46d29bc7afddbbf6753c459690d574a"),
    FACE_MODEL: (f"{_MODELS}/face_detector/blaze_face_short_range/float16/1/blaze_face_short_range.tflite",
                 "b4578f35940bf5a1a655214a1cce5cab13eba73c1297cd78e1a04c2380b0152f"),
}
# How many older pack ids vision_pack.json keeps, which the app still runs so an update doesn't make everyone download again.
OLDER_KEPT = 3
NOTICES = """Telescope's vision pack, used by the Auto framing and Background plugins.

MediaPipe (https://github.com/google-ai-edge/mediapipe), Apache License 2.0
The selfie segmenter, pose landmarker and BlazeFace short range face detector models from MediaPipe, Apache License 2.0

MediaPipe's full licence and third-party notices are in its .dist-info/licenses folder under site/.
"""
_EPOCH = (1980, 1, 1, 0, 0, 0)
# MediaPipe 1.1.1 reports usage to this address from inside its native library and has no switch for it.
# Same length, so nothing in the library moves; curl refuses the made-up scheme before it opens a socket.
TELEMETRY_URL = b"https://play.googleapis.com/log"
TELEMETRY_OFF = b"invalid://telescope.invalid/log"
assert len(TELEMETRY_URL) == len(TELEMETRY_OFF)
NATIVE_LIB = {"linux": "mediapipe/tasks/c/libmediapipe.so", "windows": "mediapipe/tasks/c/libmediapipe.dll"}


def pins() -> list:
    lines = (TOOLS / "vision-pack-requirements.txt").read_text().splitlines()
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


def strip_elf64(path: Path) -> int:
    """Drop trailing non-allocated symbol and debug sections (.symtab, .strtab, .debug_*) deterministically."""
    data = path.read_bytes()
    if data[:4] != b"\x7fELF" or data[4] != 2 or data[5] != 1:
        sys.exit(f"{path} is not a 64-bit little-endian ELF shared library")
    e_shoff, = struct.unpack_from("<Q", data, 40)
    e_shentsize, e_shnum, e_shstrndx = struct.unpack_from("<HHH", data, 58)
    if e_shentsize != 64:
        sys.exit(f"{path} has unexpected ELF section header size {e_shentsize}")
    shdrs = [list(struct.unpack_from("<IIQQQQIIQQ", data, e_shoff + i * 64)) for i in range(e_shnum)]
    shstr_off, shstr_size = shdrs[e_shstrndx][4], shdrs[e_shstrndx][5]
    shstr = data[shstr_off:shstr_off + shstr_size]

    def sec_name(off: int) -> str:
        return shstr[off:shstr.index(b"\x00", off)].decode("latin1")

    keep = []
    for i, s in enumerate(shdrs):
        name = sec_name(s[0])
        sh_type, sh_flags = s[1], s[2]
        if i == 0 or (sh_flags & 2) or name in (".comment", ".shstrtab"):
            keep.append((i, name, s))
        elif sh_type not in (2, 3) and not name.startswith(".debug"):
            sys.exit(f"{path} has unexpected non-ALLOC section {name}")
    max_end = max(s[4] + s[5] for i, name, s in keep if i != 0 and name != ".shstrtab" and s[1] != 8)
    new_shstr = bytearray(b"\x00")
    name_offs = {}
    for _, name, _ in keep:
        if name not in name_offs:
            name_offs[name] = len(new_shstr)
            new_shstr.extend(name.encode("latin1") + b"\x00")
    old_to_new = {old_i: new_i for new_i, (old_i, _, _) in enumerate(keep)}
    out = bytearray(data[:max_end])
    new_shstr_off = len(out)
    out.extend(new_shstr)
    while len(out) % 8:
        out.append(0)
    new_shoff = len(out)
    new_shstrndx = 0
    for new_i, (_, name, s) in enumerate(keep):
        s = list(s)
        s[0] = 0 if new_i == 0 else name_offs[name]
        if name == ".shstrtab":
            s[4], s[5], new_shstrndx = new_shstr_off, len(new_shstr), new_i
        if s[6] in old_to_new:
            s[6] = old_to_new[s[6]]
        if s[1] in (4, 9) and s[7] in old_to_new:
            s[7] = old_to_new[s[7]]
        out.extend(struct.pack("<IIQQQQIIQQ", *s))
    struct.pack_into("<Q", out, 40, new_shoff)
    struct.pack_into("<HHH", out, 58, 64, len(keep), new_shstrndx)
    path.write_bytes(out)
    return len(data) - len(out)


def silence_telemetry(site: Path) -> int:
    """Point MediaPipe's usage report at an address that can't be reached; how many libraries had it."""
    found = 0
    for lib in sorted((site / "mediapipe").rglob("*")):
        if lib.suffix not in (".so", ".dll", ".dylib") or not lib.is_file():
            continue
        data = lib.read_bytes()
        if TELEMETRY_URL in data:
            lib.write_bytes(data.replace(TELEMETRY_URL, TELEMETRY_OFF))
            found += 1
    return found


def build(key: str, work: Path, cache: Path) -> Path:
    """The pack for one platform, unpacked into work/key."""
    wheels = cache / f"wheels-{key}"
    subprocess.run([sys.executable, "-m", "pip", "download", "--quiet", "--disable-pip-version-check", "--no-deps",
                    "--only-binary=:all:", "--platform", PLATFORMS[key], "--platform", "any",
                    "--python-version", "3.12", "--implementation", "py", "-d", str(wheels), *pins()], check=True)
    root = work / key
    site = root / "site"
    site.mkdir(parents=True)
    # The cache keeps wheels of older pins too, so match the version as well as the name.
    # Only the C library and its licence notices are extracted: the Rust app loads the library directly and never runs Python.
    wanted = {tuple(p.lower().replace("-", "_").split("==")) for p in pins()}
    native = NATIVE_LIB[key]
    for wheel in sorted(wheels.glob("*.whl")):
        if tuple(wheel.name.lower().split("-")[:2]) not in wanted:
            continue
        with zipfile.ZipFile(wheel) as z:
            for member in z.namelist():
                if member.endswith("/"):
                    continue
                if member == native or (member.startswith("mediapipe-") and ".dist-info/licenses/" in member):
                    dest = site / member
                    dest.parent.mkdir(parents=True, exist_ok=True)
                    dest.write_bytes(z.read(member))
    lib = site / native
    if not lib.is_file():
        sys.exit(f"Couldn't find {native} in the downloaded MediaPipe wheel")
    if not silence_telemetry(site):
        sys.exit("Couldn't find MediaPipe's usage report address in the pack. A new MediaPipe may send it from "
                 "somewhere else: check with strace before releasing.")
    if key == "linux":
        strip_elf64(lib)
    fetch_models(root / "models", cache)
    (root / "NOTICES.txt").write_text(NOTICES, encoding="utf-8", newline="\n")
    return root


def write_zip(root: Path, dest: Path):
    """Same files in the same order with the same dates, so it only changes when what's in it does. LZMA, which is a fifth smaller
    than deflate here: the app reads it through the zip crate's lzma feature (crates/vision/src/pack.rs) and still reads deflate packs."""
    with zipfile.ZipFile(dest, "w", zipfile.ZIP_LZMA) as z:
        for path in sorted((p for p in root.rglob("*") if p.is_file()),
                           key=lambda p: p.relative_to(root).parts):
            info = zipfile.ZipInfo(path.relative_to(root).as_posix(), _EPOCH)
            info.compress_type = zipfile.ZIP_LZMA
            info.external_attr = 0o644 << 16
            z.writestr(info, path.read_bytes())


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--check", action="store_true", help="fail when a pack doesn't match vision_pack.json")
    ap.add_argument("--write", action="store_true", help="write vision_pack.json from what was built")
    ap.add_argument("--out", type=Path, help="write the release zips into this folder")
    ap.add_argument("--cache", type=Path, default=TOOLS.parent / "target" / "vision-cache")
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
            # The pack this replaces still runs (without the newer model), so people who have it aren't asked to download again
            before = known.get(key, {})
            older = [i for i in [before.get("id"), *before.get("older", [])] if i and i != tree]
            if older:
                info[key]["older"] = list(dict.fromkeys(older))[:OLDER_KEPT]
            unpacked = sum(p.stat().st_size for p in root.rglob("*") if p.is_file())
            print(f"{key}: id {tree[:16]}, {files} files, {zipped.stat().st_size / 1e6:.1f} MB zipped, {unpacked / 1e6:.1f} MB unpacked")
            if args.check and known.get(key, {}).get("id") != tree:
                problems.append(key)
            if args.out:
                args.out.mkdir(parents=True, exist_ok=True)
                name = f"Telescope-vision-{key}-{tree[:16]}.zip"
                shutil.move(zipped, args.out / name)  # the temp folder can be on another filesystem
                print(f"  wrote {args.out / name}")
    if args.write:
        PACK_INFO.write_text(json.dumps(info, indent=2, sort_keys=True) + "\n")
        print(f"wrote {PACK_INFO}")
    if problems:
        sys.exit(f"The {' and '.join(problems)} pack doesn't match crates/vision/vision_pack.json. If a pin in "
                 f"vision-pack-requirements.txt changed on purpose, run this with --write and commit the result.")


if __name__ == "__main__":
    main()
