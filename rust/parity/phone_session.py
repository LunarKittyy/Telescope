#!/usr/bin/env python3
"""Drives a desktop through a scripted session against the fake phone and saves every request it sends.

The desktop runs in a throwaway dev profile (TELESCOPE_DEV_PROFILE) that already has the fake phone paired, so the
real install is left alone. Each step goes through the desktop's own command line (`--action ID NAME=VALUE`), the
way a sign-in script or a desktop without global shortcuts would, and leaves a mark in the recording so
compare_phone.py can line the phases up.

    python phone_session.py OUT.jsonl                                  # the Python desktop, the basic script
    python phone_session.py OUT.jsonl --desktop "../target/debug/telescope-rs"
    python phone_session.py OUT.jsonl --script controls --show         # with its window on screen

Scripts: basic (start, a few controls, stop), controls (the camera and stream controls a phone on H.264 gets).
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

PYTHON_DESKTOP = f"{shlex.quote(sys.executable)} {shlex.quote(str(REPO / 'desktop' / 'main.py'))}"

# Each step: ("mark", name), ("action", id, "name=value", ...), ("wait_for", method, path, seconds), ("sleep", s)
START = [("mark", "pair"), ("wait_for", "GET", "/v1/ping", 30), ("sleep", 2),
         ("mark", "stream"), ("action", "streaming.start_stop"), ("wait_for", "GET", "/v1/video.h264", 30), ("sleep", 3)]
STOP = [("mark", "stop"), ("action", "streaming.start_stop"), ("wait_for", "POST", "/v1/session", 15), ("sleep", 2)]
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
}


def profile_config(entry: dict) -> dict:
    """A dev profile with the fake phone paired and first-run hints already seen."""
    return {"version": 3, "plugin_configs": {
        "connection": {"phones": [entry], "selected_phone": entry["id"], "route": "auto", "udp": False},
        "onboarding": {"streamed": True}, "setup": {"language": "en"}}}


def requests(path: Path) -> list:
    with contextlib.suppress(OSError):
        return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]
    return []


def seen_since_mark(lines: list, method: str, path: str) -> bool:
    last = max((i for i, r in enumerate(lines) if "mark" in r), default=-1)
    return any(r.get("method") == method and r.get("path") == path for r in lines[last + 1:])


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
    _phone, entry, stop_phone = fake_phone.serve(None, args.host, 30.0, "", recorder)
    profile = Path(tempfile.mkdtemp(prefix="telescope-parity-profile-"))
    (profile / "telescope_config.json").write_text(json.dumps(profile_config(entry), indent=2), encoding="utf-8")
    env = {**os.environ, "TELESCOPE_DEV_PROFILE": str(profile)}
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
                with recorder.lock:
                    recorder.file.write(json.dumps({"t": round(time.monotonic() - recorder.start, 3), "mark": step[1]}) + "\n")
                print(f"{step[1]}", flush=True)
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
                _, method, path, seconds = step
                deadline = time.monotonic() + seconds
                while not seen_since_mark(requests(args.out), method, path):
                    if time.monotonic() > deadline or app.poll() is not None:
                        failed = f"no {method} {path} within {seconds} s"
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
