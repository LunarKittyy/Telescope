#!/usr/bin/env python3
"""Drives a desktop through a scripted session against the fake phone and saves every request it sends.

The desktop runs in a throwaway dev profile (TELESCOPE_DEV_PROFILE) that already has the fake phone paired, so the
real install is left alone. Each step goes through the desktop's own command line (`--action ID NAME=VALUE`), the
way a sign-in script or a desktop without global shortcuts would, and leaves a mark in the recording so
compare_phone.py can line the phases up.

    python phone_session.py OUT.jsonl                                  # the Python desktop, the basic script
    python phone_session.py OUT.jsonl --desktop "../target/debug/telescope-rs"
    python phone_session.py OUT.jsonl --script controls --show         # with its window on screen

Scripts: basic (start, a few controls, stop), controls (the camera and stream controls a phone on H.264 gets),
pair (no phone yet: Add phone, the phone answers the QR offer, then a stream), lens (switch to the second lens and
back), handover (streaming over Wi-Fi, plugged in over USB, pulled out again) and recover (the phone drops off the
network mid-stream and comes back).

The fake adb (../tools/fake_adb.py) is first on PATH for every script, so a real phone on the cable stays out of it.
The pair script needs a desktop that takes `--action connection.add_phone` and writes its QR offer to the file
TELESCOPE_PARITY_OFFER names; python_desktop.py adds both to the Python desktop.
"""

import argparse
import contextlib
import json
import os
import shlex
import signal
import subprocess
import sys
import tempfile
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[1]
sys.path.insert(0, str(REPO / "rust" / "tools"))
import fake_phone  # noqa: E402

PYTHON_DESKTOP = f"{shlex.quote(sys.executable)} {shlex.quote(str(HERE / 'python_desktop.py'))}"

# Each step: ("mark", name), ("action", id, "name=value", ...), ("wait_for", method, path, seconds[, via]),
# ("sleep", s) or ("phone", "pair" | "plug" | "unplug" | "drop" | "back")
STREAM = [("mark", "stream"), ("action", "streaming.start_stop"), ("wait_for", "GET", "/v1/video.h264", 30), ("sleep", 3)]
START = [("mark", "pair"), ("wait_for", "GET", "/v1/ping", 30), ("sleep", 2)] + STREAM
STOP = [("mark", "stop"), ("action", "streaming.start_stop"), ("wait_for", "POST", "/v1/session", 15), ("sleep", 2)]
# Scripts that start with no phone paired
UNPAIRED = {"pair"}
SCRIPTS = {
    "basic": START + [
        ("mark", "controls"),
        ("action", "camera.compensation", "op=up", "amount=1"), ("sleep", 1),
        ("action", "camera.stabilization", "mode=toggle"), ("sleep", 1),
        ("action", "camera.noise_reduction", "op=next"), ("sleep", 1),
        ("action", "camera.torch", "mode=on"), ("sleep", 1),
    ] + STOP,
    "controls": START + [("mark", "controls")] + [step for action in (
        ("camera.exposure_mode", "op=next"), ("camera.iso", "op=up"), ("camera.shutter", "op=up"),
        ("camera.exposure_mode", "op=next"), ("camera.compensation", "op=down", "amount=1"),
        ("camera.wb_mode", "op=next"), ("camera.temperature", "op=up"), ("camera.tint", "op=up"),
        ("camera.wb_mode", "op=next"), ("camera.focus_mode", "op=next"), ("camera.focus_mode", "op=next"),
        ("camera.focus", "op=up"), ("camera.focus_mode", "op=next"), ("camera.stabilization", "mode=toggle"),
        ("camera.noise_reduction", "op=next"), ("camera.sharpening", "op=next"), ("camera.black_level_lock", "mode=toggle"),
        ("camera.low_light", "mode=toggle"), ("camera.torch", "mode=toggle"), ("stream.fps", "op=next"),
        ("stream.bitrate", "op=up"), ("transforms.zoom", "op=up"),
    ) for step in (("action",) + action, ("sleep", 1))] + STOP,
    "pair": [("mark", "pair"), ("action", "connection.add_phone"), ("phone", "pair"),
             ("wait_for", "GET", "/v1/ping", 30), ("sleep", 3)] + STREAM + STOP,
    "lens": START + [("mark", "lens"), ("action", "camera.lens", "op=next"), ("sleep", 2),
                     ("action", "camera.lens", "op=next"), ("sleep", 2)] + STOP,
    "handover": START + [
        ("mark", "plug"), ("phone", "plug"), ("wait_for", "GET", "/v1/video.h264", 30, "usb"), ("sleep", 4),
        ("mark", "unplug"), ("phone", "unplug"), ("wait_for", "GET", "/v1/video.h264", 40, "wifi"), ("sleep", 4),
    ] + STOP,
    "recover": START + [
        ("mark", "drop"), ("phone", "drop"), ("sleep", 8),
        ("mark", "recover"), ("phone", "back"), ("wait_for", "GET", "/v1/video.h264", 40), ("sleep", 4),
    ] + STOP,
}


def profile_config(entry) -> dict:
    """A dev profile with the fake phone paired (none for entry None) and first-run hints already seen."""
    connection = {"phones": [entry] if entry else [], "route": "auto", "udp": False}
    if entry:
        connection["selected_phone"] = entry["id"]
    return {"version": 3, "plugin_configs": {
        "connection": connection, "onboarding": {"streamed": True}, "setup": {"language": "en"}}}


def fake_adb_folder(folder: Path) -> Path:
    """A folder with an `adb` that runs fake_adb.py, to put first on PATH."""
    folder.mkdir(parents=True, exist_ok=True)
    adb = folder / "adb"
    adb.write_text(f"#!/bin/sh\nexec {shlex.quote(sys.executable)} -I "
                   f"{shlex.quote(str(REPO / 'rust' / 'tools' / 'fake_adb.py'))} \"$@\"\n", encoding="utf-8")
    adb.chmod(0o755)
    return adb


def wait_for_file(path: Path, seconds: float, app) -> str:
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline and app.poll() is None:
        with contextlib.suppress(OSError):
            text = path.read_text(encoding="utf-8")
            if text.strip():
                return text
        time.sleep(0.25)
    return ""


def requests(path: Path) -> list:
    with contextlib.suppress(OSError):
        return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]
    return []


def seen_since_mark(lines: list, method: str, path: str, via=None) -> bool:
    last = max((i for i, r in enumerate(lines) if "mark" in r), default=-1)
    return any(r.get("method") == method and r.get("path") == path and via in (None, r.get("via", "wifi"))
               for r in lines[last + 1:])


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("out", type=Path)
    parser.add_argument("--desktop", default=PYTHON_DESKTOP, help="the command that starts the desktop")
    parser.add_argument("--script", choices=sorted(SCRIPTS), default="basic")
    parser.add_argument("--host", default="127.0.0.2")
    parser.add_argument("--show", action="store_true", help="put the desktop's window on screen")
    args = parser.parse_args(argv)
    desktop = shlex.split(args.desktop)
    recorder = fake_phone.Recorder(args.out)
    profile = Path(tempfile.mkdtemp(prefix="telescope-parity-profile-"))
    adb = fake_adb_folder(profile / "bin")
    phone, entry, stop_phone = fake_phone.serve(None, args.host, 30.0, "", recorder, adb_state=profile / "adb.json")
    paired = None if args.script in UNPAIRED else entry
    (profile / "telescope_config.json").write_text(json.dumps(profile_config(paired), indent=2), encoding="utf-8")
    offer_file = profile / "offer.json"
    env = {**os.environ, "TELESCOPE_DEV_PROFILE": str(profile), "TELESCOPE_ADB": str(adb),
           "FAKE_ADB_STATE": str(profile / "adb.json"), "TELESCOPE_PARITY_OFFER": str(offer_file),
           "PATH": f"{adb.parent}{os.pathsep}{os.environ.get('PATH', '')}"}
    if not args.show:
        env["QT_QPA_PLATFORM"] = "offscreen"
    log = (profile / "desktop.log").open("w")
    app = subprocess.Popen(desktop, env=env, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
    failed = None
    try:
        for step in SCRIPTS[args.script]:
            if app.poll() is not None:
                failed = f"the desktop quit (exit code {app.returncode})"
                break
            if step[0] == "mark":
                recorder.mark(step[1])
                print(f"{step[1]}", flush=True)
            elif step[0] == "phone":
                if step[1] == "pair":
                    offer = wait_for_file(offer_file, 20, app)
                    if not offer:
                        failed = f"no pairing offer in {offer_file} within 20 s"
                        break
                    status = phone.pair(json.loads(offer), args.host)
                    print(f"  paired: HTTP {status}", flush=True)
                elif step[1] == "drop":
                    phone.drop()
                elif step[1] == "back":
                    phone.back()
                else:
                    phone.plug(step[1] == "plug")
            elif step[0] == "sleep":
                time.sleep(step[1])
            elif step[0] == "action":
                deadline = time.monotonic() + 20  # the desktop may still be starting up and not listening yet
                while True:
                    done = subprocess.run(desktop + ["--action", *step[1:]], env=env, capture_output=True, text=True)
                    if done.returncode == 0 or time.monotonic() > deadline:
                        break
                    time.sleep(0.5)
                print(f"  --action {' '.join(step[1:])}: {(done.stdout or done.stderr).strip()}", flush=True)
            elif step[0] == "wait_for":
                method, path, seconds, via = (step[1:] + (None,))[:4]
                deadline = time.monotonic() + seconds
                while not seen_since_mark(requests(args.out), method, path, via):
                    if time.monotonic() > deadline or app.poll() is not None:
                        failed = f"no {method} {path}{f' over {via}' if via else ''} within {seconds} s"
                        break
                    time.sleep(0.25)
                if failed:
                    break
    finally:
        with contextlib.suppress(ProcessLookupError):
            os.killpg(app.pid, signal.SIGTERM)
        try:
            app.wait(10)
        except subprocess.TimeoutExpired:
            os.killpg(app.pid, signal.SIGKILL)
            app.wait()
        stop_phone()
        recorder.close()
        log.close()
    print(f"{len(requests(args.out))} lines in {args.out}; the desktop's log is {profile / 'desktop.log'}")
    if failed:
        print(f"Stopped early: {failed}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
