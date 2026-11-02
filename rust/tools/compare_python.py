#!/usr/bin/env python3
"""Runs tools/measure.py on the Rust app and the last Python app under the same conditions, round after round, and
prints one JSON line per run (with the load average around it) plus a summary table of medians and ranges.

  compare_python.py --rust BIN --python-desktop DIR --python VENV_PYTHON --clip CLIP.h264 [--rounds 3] [--rows a,b]
  compare_python.py ... --startup --runs 7

Both apps get their own throw-away dev profile (TELESCOPE_DEV_PROFILE and every XDG folder pointed into --scratch), a
phone already paired at 127.0.0.2 served by fake_phone.py, and resume streaming by themselves: the Rust app with
--autostart, the Python app with TELESCOPE_DEV_RESUME. Rows run in turn within a round, so a busy minute on the machine
touches every row, not one.

Rows: rust-window, rust-window-vaapi, rust-headless, rust-headless-vaapi, python, python-vaapi (needs the app's own
VA-API PyAV build in SCRATCH/xdg-data-vaapi/Telescope/pyav-vaapi, made by telescope/pyav_vaapi.py). --effect blur or autoframing turns that plugin on in both apps (they need their vision packs)."""

import argparse
import json
import os
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

TOOLS = Path(__file__).resolve().parent
ROWS = ["rust-window", "rust-window-vaapi", "rust-headless", "rust-headless-vaapi", "python", "python-vaapi"]
EFFECTS = {"none": {}, "blur": {"background": {"mode": "blur"}}, "autoframing": {"auto_framing": {"on": True}}}


def config(entry, vaapi, effect):
    connection = {"phones": [entry], "route": "auto", "udp": False, "selected_phone": entry["id"]}
    plugins = {"connection": connection, "onboarding": {"streamed": True}, "setup": {"language": "en", "hw_decode": vaapi}}
    plugins.update(EFFECTS[effect])
    return {"version": 3, "selected_device": entry["id"], "plugin_configs": plugins, "devices": {},
            "plugins": {"on": list(EFFECTS[effect])}}


def command(row, args, mode):
    probe = mode == "startup"
    if row.startswith("python"):
        script = TOOLS / "python_probe.py" if probe else Path(args.python_desktop) / "main.py"
        return [args.python, str(script)], Path(args.python_desktop)
    window = row.startswith("rust-window")
    vaapi = row.endswith("vaapi")
    cmd = [args.rust] if window else [args.rust, "stream", "--seconds", "90"] + (["--vaapi"] if vaapi else [])
    if window and not probe:
        cmd.append("--autostart")
    return cmd, None


def run_row(row, args, mode):
    scratch = Path(args.scratch)
    phone = subprocess.Popen([sys.executable, "-I", str(TOOLS / "fake_phone.py"), str(args.clip), "--fps", "30"],
                             stdout=subprocess.PIPE, text=True, env=dict(os.environ, TMPDIR=str(scratch)),
                             start_new_session=True)
    try:
        entry = json.loads(phone.stdout.readline())
        profile = Path(tempfile.mkdtemp(prefix="profile-", dir=scratch))
        vaapi = row.endswith("vaapi")
        (profile / "telescope_config.json").write_text(json.dumps(config(entry, vaapi, args.effect), indent=2))
        env = dict(os.environ, TELESCOPE_DEV_PROFILE=str(profile), TELESCOPE_DEV_SUPERVISED="1",
                   XDG_CONFIG_HOME=str(scratch / "xdg-config"), XDG_DATA_HOME=str(scratch / ("xdg-data-vaapi" if row == "python-vaapi" else "xdg-data")),
                   XDG_STATE_HOME=str(scratch / "xdg-state"), XDG_CACHE_HOME=str(scratch / "xdg-cache"))
        if mode == "stream":
            env["TELESCOPE_DEV_RESUME"] = "1"
        if args.vision_pack:
            env["TELESCOPE_VISION_PACK"] = args.vision_pack
            (profile / "vision").mkdir()
        cmd, cwd = command(row, args, mode)
        measure = [sys.executable, str(TOOLS / "measure.py"), mode, "--camera", args.camera]
        measure += ["--runs", str(args.runs)] if mode == "startup" else ["--warmup", "10", "--seconds", "30"]
        before = os.getloadavg()
        out = subprocess.run(measure + ["--"] + cmd, env=env, cwd=cwd, capture_output=True, text=True)
        after = os.getloadavg()
        shutil.rmtree(profile, ignore_errors=True)
        try:
            result = json.loads(out.stdout.strip().splitlines()[-1])
        except (IndexError, ValueError):
            result = {"error": (out.stdout + out.stderr)[-400:]}
        result.update(row=row, mode=mode, effect=args.effect, load_before=before, load_after=after)
        return result
    finally:
        phone.terminate()
        phone.wait()


def summary(results, key):
    for row in ROWS:
        values = [r[key] for r in results if r["row"] == row and r.get(key) is not None]
        if values:
            print(f"{row:22} {key:28} median {statistics.median(values):8.1f}  range {min(values):8.1f} to {max(values):8.1f}  n={len(values)}")


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--rust", required=True)
    p.add_argument("--python", required=True, help="the venv's python")
    p.add_argument("--python-desktop", required=True, help="desktop/ of the Python checkout")
    p.add_argument("--clip", required=True)
    p.add_argument("--scratch", default=None)
    p.add_argument("--rounds", type=int, default=3)
    p.add_argument("--rows", default=",".join(ROWS))
    p.add_argument("--startup", action="store_true")
    p.add_argument("--runs", type=int, default=7, help="startup runs per row")
    p.add_argument("--effect", choices=list(EFFECTS), default="none")
    p.add_argument("--vision-pack", default=None)
    p.add_argument("--camera", default="/dev/video11")
    args = p.parse_args()
    args.scratch = args.scratch or tempfile.mkdtemp(prefix="compare-")
    Path(args.scratch).mkdir(parents=True, exist_ok=True)
    rows = args.rows.split(",")
    mode = "startup" if args.startup else "stream"
    results = []
    for round_ in range(args.rounds):
        for row in rows:
            result = run_row(row, args, mode)
            result["round"] = round_
            results.append(result)
            print(json.dumps(result), flush=True)
            time.sleep(3)
    print()
    for key in (["first_paint_median_ms"] if args.startup else
                ["cpu_percent_of_one_core", "rss_mb_mean", "pss_mb_mean", "reader_frames"]):
        summary(results, key)


if __name__ == "__main__":
    main()
