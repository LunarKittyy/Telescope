#!/usr/bin/env python3
"""Write manifest.json for a release: what the apps' updaters read to decide whether to update.

Usage: write_manifest.py --version 0.5.0 --build 123 --channel nightly --commit <sha> --tag nightly
                         --repo owner/name --out out/manifest.json out/Telescope.apk out/Telescope-windows.zip ...
"""

import argparse
import datetime
import hashlib
import json
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def session_protocol() -> int:
    """The desktop's SESSION_PROTOCOL; the phone's SessionServer.PROTOCOL_VERSION must match it."""
    source = (ROOT / "desktop" / "telescope" / "session_client.py").read_text()
    desktop = int(re.search(r"^SESSION_PROTOCOL = (\d+)", source, re.M).group(1))
    kotlin = (ROOT / "android" / "app" / "src" / "main" / "kotlin" / "com" / "telescope" / "SessionServer.kt").read_text()
    phone = int(re.search(r"PROTOCOL_VERSION = (\d+)", kotlin).group(1))
    if desktop != phone:
        raise SystemExit(f"Session protocol differs: desktop {desktop}, phone {phone}")
    return desktop


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def main():
    parser = argparse.ArgumentParser()
    for name in ("--version", "--channel", "--commit", "--tag", "--repo", "--out"):
        parser.add_argument(name, required=True)
    parser.add_argument("--build", type=int, required=True)
    parser.add_argument("files", nargs="+", type=Path)
    args = parser.parse_args()

    version_name = args.version if args.channel == "stable" else f"{args.version}-{args.channel}.{args.build}"
    base = f"https://github.com/{args.repo}/releases/download/{args.tag}"
    manifest = {
        "schema": 1,
        "version": args.version,
        "build": args.build,
        "channel": args.channel,
        "versionName": version_name,
        "commit": args.commit,
        "date": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "protocol": session_protocol(),
        "notes": f"https://github.com/{args.repo}/releases/tag/{args.tag}",
        "assets": [
            {"name": f.name, "url": f"{base}/{f.name}", "sha256": sha256(f), "size": f.stat().st_size}
            for f in args.files
        ],
        # The phone compares versionCode (the build number), the same number the desktop compares.
        "android": {"versionCode": args.build, "versionName": version_name, "asset": "Telescope.apk"},
    }
    Path(args.out).write_text(json.dumps(manifest, indent=2) + "\n")
    print(json.dumps(manifest, indent=2))


if __name__ == "__main__":
    main()
