#!/usr/bin/env python3
"""Check a release's files before they're published: what the release checklist used to ask a person to look at.

Checks the APK is signed with Telescope's key (any other key can't install over an existing copy), that the APK and
both desktop bundles say this version and build, that both bundles carry the notices and the APK, and that
manifest.json matches the files.

Usage: check_release.py --version 4.2.0 --build 123 --channel nightly out/
"""

import argparse
import json
import os
import re
import subprocess
import sys
import tarfile
import zipfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from write_manifest import sha256  # noqa: E402

# The release key's certificate. Without that key no future APK installs as an update (docs/building.md#ci)
APK_CERT_SHA256 = "f26c7c9a76e51d3f3789dc25ce7301994b24b53292d24db39d432e084a4d7a99"
BUNDLE_FILES = ("THIRD_PARTY_NOTICES.txt", "Telescope.apk")

problems = []


def android_tool(name: str) -> str:
    sdk = Path(os.environ.get("ANDROID_HOME", Path.home() / "Android" / "Sdk"))
    found = sorted((sdk / "build-tools").glob(f"*/{name}"))
    if not found:
        raise SystemExit(f"No {name} under {sdk}/build-tools")
    return str(found[-1])


def check_apk(apk: Path, version_name: str, build: int):
    certs = subprocess.run([android_tool("apksigner"), "verify", "--print-certs", str(apk)],
                           capture_output=True, text=True)
    digests = re.findall(r"certificate SHA-256 digest: ([0-9a-f]{64})", certs.stdout)
    if certs.returncode != 0 or digests != [APK_CERT_SHA256]:
        problems.append(f"Telescope.apk isn't signed with the release key (got {digests or certs.stderr.strip()})")
    badging = subprocess.run([android_tool("aapt2"), "dump", "badging", str(apk)],
                             capture_output=True, text=True, check=True).stdout
    m = re.search(r"versionCode='(\d+)' versionName='([^']+)'", badging)
    if not m or (int(m.group(1)), m.group(2)) != (build, version_name):
        problems.append(f"Telescope.apk is {m and m.groups()}, expected ({build}, {version_name!r})")


def check_bundle(name: str, names: set, build_py: str, build: int, version: str, channel: str):
    for f in BUNDLE_FILES:
        if f not in names:
            problems.append(f"{name} has no {f}")
    if build_py is not None:
        stamp = {}
        exec(build_py, stamp)
        if (stamp.get("VERSION"), stamp.get("BUILD"), stamp.get("CHANNEL")) != (version, build, channel):
            problems.append(f"{name} says {stamp.get('VERSION')} {stamp.get('CHANNEL')} {stamp.get('BUILD')}")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--version", required=True)
    parser.add_argument("--build", type=int, required=True)
    parser.add_argument("--channel", required=True)
    parser.add_argument("out", type=Path)
    args = parser.parse_args()
    out = args.out
    version_name = args.version if args.channel == "stable" else f"{args.version}-{args.channel}.{args.build}"

    check_apk(out / "Telescope.apk", version_name, args.build)

    with tarfile.open(out / "Telescope-linux.tar.gz") as tar:
        names = {n.removeprefix("./") for n in tar.getnames()}
        build_py = tar.extractfile("./telescope/_build.py").read().decode()
    check_bundle("Telescope-linux.tar.gz", names, build_py, args.build, args.version, args.channel)

    with zipfile.ZipFile(out / "Telescope-windows-portable.zip") as z:
        names = set(z.namelist())
    check_bundle("Telescope-windows-portable.zip", names, None, args.build, args.version, args.channel)
    # The Windows app's stamp is compiled into it, but its folder carries the build number
    if not any(n.startswith(f"lib-{args.build}/") for n in names):
        problems.append(f"Telescope-windows-portable.zip has no lib-{args.build} folder")

    manifest = json.loads((out / "manifest.json").read_text())
    if (manifest.get("versionName"), manifest.get("build")) != (version_name, args.build):
        problems.append(f"manifest.json is for {manifest.get('versionName')} build {manifest.get('build')}")
    for asset in manifest.get("assets", []):
        f = out / asset["url"].rsplit("/", 1)[-1]
        if not f.is_file() or sha256(f) != asset["sha256"]:
            problems.append(f"manifest.json's checksum for {asset['name']} doesn't match {f.name}")

    for p in problems:
        print(f"::error::{p}")
    print(f"{len(problems)} problem(s) in the release files" if problems else "Release files check out")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
