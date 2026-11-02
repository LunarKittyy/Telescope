#!/usr/bin/env python3
"""Measures a desktop app: time to its first painted window, and CPU, memory and delivered frame rate while it streams.

  measure.py startup [--runs 5] -- CMD...       prints first-paint-ms per run (the app must print it, see python_probe)
  measure.py stream [--warmup 10] [--seconds 30] [--camera /dev/video11] -- CMD...

CPU and memory are summed over the process and its children. A reader (ffmpeg) holds the virtual camera open while
sampling, the way a video call would, and its frame count is the delivered rate."""

import argparse
import json
import os
import re
import signal
import statistics
import subprocess
import time

TICK = os.sysconf("SC_CLK_TCK")


def tree(root: int) -> list:
    children = {}
    for name in os.listdir("/proc"):
        if name.isdigit():
            try:
                stat = open(f"/proc/{name}/stat").read()
                ppid = int(stat.rsplit(")", 1)[1].split()[1])
                children.setdefault(ppid, []).append(int(name))
            except (OSError, ValueError, IndexError):
                pass
    out, todo = [], [root]
    while todo:
        pid = todo.pop()
        out.append(pid)
        todo += children.get(pid, [])
    return out


def cpu_ticks(pids) -> int:
    total = 0
    for pid in pids:
        try:
            fields = open(f"/proc/{pid}/stat").read().rsplit(")", 1)[1].split()
            total += int(fields[11]) + int(fields[12])
        except (OSError, IndexError):
            pass
    return total


def memory(pids) -> tuple:
    rss = pss = 0
    for pid in pids:
        try:
            for line in open(f"/proc/{pid}/smaps_rollup"):
                if line.startswith("Rss:"):
                    rss += int(line.split()[1])
                elif line.startswith("Pss:"):
                    pss += int(line.split()[1])
        except OSError:
            pass
    return rss / 1024, pss / 1024


def startup(cmd, runs):
    times = []
    for _ in range(runs):
        env = dict(os.environ, TELESCOPE_T0=str(time.monotonic_ns()))
        proc = subprocess.Popen(cmd, env=env, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True,
                                start_new_session=True)
        found = {}
        deadline = time.monotonic() + 30
        for line in proc.stdout:
            m = re.match(r"(first-\w+-ms) ([\d.]+)", line)
            if m:
                found[m.group(1)] = float(m.group(2))
            if "first-paint-ms" in found or time.monotonic() > deadline:
                break
        os.killpg(proc.pid, signal.SIGTERM)
        proc.wait()
        times.append(found)
        time.sleep(1.5)
    print(json.dumps({"runs": times, "first_paint_median_ms": statistics.median(t["first-paint-ms"] for t in times)}))


def stream(cmd, warmup, seconds, camera):
    proc = subprocess.Popen(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)
    time.sleep(warmup / 2)
    reader = subprocess.Popen(["ffmpeg", "-hide_banner", "-f", "v4l2", "-i", camera, "-f", "null", "-"],
                              stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True, start_new_session=True)
    time.sleep(warmup / 2)
    pids = tree(proc.pid)
    start_cpu, start = cpu_ticks(pids), time.monotonic()
    samples = []
    while time.monotonic() - start < seconds:
        time.sleep(1)
        samples.append(memory(tree(proc.pid)))
    cpu = (cpu_ticks(tree(proc.pid)) - start_cpu) / TICK / (time.monotonic() - start)
    reader.send_signal(signal.SIGINT)
    err = reader.communicate(timeout=10)[1]
    frames = [int(f) for f in re.findall(r"frame=\s*(\d+)", err)]
    os.killpg(proc.pid, signal.SIGTERM)
    try:
        proc.wait(15)
    except subprocess.TimeoutExpired:
        os.killpg(proc.pid, signal.SIGKILL)
    rss = [s[0] for s in samples]
    pss = [s[1] for s in samples]
    print(json.dumps({
        "cpu_percent_of_one_core": round(cpu * 100, 1),
        "rss_mb_mean": round(statistics.mean(rss), 1), "rss_mb_max": round(max(rss), 1),
        "pss_mb_mean": round(statistics.mean(pss), 1),
        "reader_frames": frames[-1] if frames else None,
        "reader_seconds": round(warmup / 2 + seconds, 1),
        "processes": len(pids),
    }))


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("mode", choices=["startup", "stream"])
    parser.add_argument("--runs", type=int, default=5)
    parser.add_argument("--warmup", type=float, default=10)
    parser.add_argument("--seconds", type=float, default=30)
    parser.add_argument("--camera", default="/dev/video11")
    parser.add_argument("cmd", nargs="+")
    args = parser.parse_args()
    if args.mode == "startup":
        startup(args.cmd, args.runs)
    else:
        stream(args.cmd, args.warmup, args.seconds, args.camera)


if __name__ == "__main__":
    main()
