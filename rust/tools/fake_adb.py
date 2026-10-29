#!/usr/bin/env python3
"""A stand-in adb for the fake phone (fake_phone.py): lists it while it's plugged in, forwards a free local port
to its session and stream ports, and takes reverse and the pairing broadcast. It reads what's plugged in from the
file FAKE_ADB_STATE names (the fake phone writes it) and adds each command it gets to the phone's recording.

phone_session.py puts it first on PATH as `adb` for the desktop it drives (TELESCOPE_ADB also points at it).
"""

import base64
import json
import os
import sys
import time
from pathlib import Path

SERIAL = "FAKE0001"


def load_state() -> dict:
    try:
        return json.loads(Path(os.environ["FAKE_ADB_STATE"]).read_text(encoding="utf-8"))
    except (KeyError, OSError, ValueError):
        return {"devices": [], "ports": {}}


def recorded(args: list, state: dict) -> list:
    """args as recorded: what changes every run shows as a stand-in, the pairing broadcast's payload as <payload>
    and a local port a forward got as tcp:<local>."""
    local = {f"tcp:{port}" for port in (state.get("ports") or {}).values()}
    out = ["tcp:<local>" if arg in local else arg for arg in args]
    for i, arg in enumerate(out[:-1]):
        if arg == "payload" and i > 0 and out[i - 1] == "--es":
            out[i + 1] = "<payload>"
    return out


def record(state: dict, args: list):
    path = state.get("record")
    if not path:
        return
    rest = [a for a in args if a not in ("-s", SERIAL)]
    line = {"t": round(time.monotonic() - state.get("start", 0), 3), "port": 5037, "method": "ADB",
            "path": rest[0] if rest else "", "query": {}, "body": recorded(args, state), "via": "usb"}
    with open(path, "a", encoding="utf-8") as f:
        f.write(json.dumps(line) + "\n")


def broadcast_payload(args: list):
    """The pairing offer in an `am broadcast ... --es payload B64`, written next to the state file for the phone."""
    if "payload" not in args:
        return
    i = args.index("payload")
    try:
        offer = base64.b64decode(args[i + 1]).decode()
    except (IndexError, ValueError):
        return
    Path(os.environ["FAKE_ADB_STATE"]).with_name("broadcast.json").write_text(offer, encoding="utf-8")


def run(args: list, state: dict) -> tuple:
    """(exit code, stdout, stderr) for one adb command line."""
    devices = state.get("devices") or []
    rest = list(args)
    while rest[:1] in (["-s"], ["-P"], ["-H"]):
        rest = rest[2:]
    command = rest[0] if rest else ""
    if command in ("start-server", "kill-server", "version", "nodaemon"):
        return 0, "", ""
    if command == "devices":
        lines = ["List of devices attached"]
        for serial, state_name, model in devices:
            extra = f" product:fake model:{model.replace(' ', '_')} device:fake transport_id:1" if "-l" in rest else ""
            lines.append(f"{serial}\t{state_name}{extra}" if not extra else f"{serial}       {state_name}{extra}")
        return 0, "\n".join(lines) + "\n\n", ""
    if not any(d[1] == "device" for d in devices):
        return 1, "", "adb: no devices/emulators found\n"
    if command == "forward":
        if "--remove" in rest or "--remove-all" in rest or "--list" in rest:
            return 0, "", ""
        local, remote = rest[1], rest[2]
        port = state.get("ports", {}).get(remote.removeprefix("tcp:"))
        if port is None:
            return 1, "", f"adb: error: cannot bind listener: {remote}\n"
        return 0, (f"{port}\n" if local == "tcp:0" else ""), ""
    if command == "reverse":
        return 0, "", ""
    if command == "shell":
        if rest[1:3] == ["getprop", "ro.product.model"]:
            return 0, devices[0][2] + "\n", ""
        if rest[1:3] == ["am", "broadcast"]:
            broadcast_payload(rest)
            return 0, "Broadcasting: Intent { act=com.telescope.action.PAIR }\nBroadcast completed: result=0\n", ""
        return 0, "", ""
    return 0, "", ""


def main() -> int:
    args = sys.argv[1:]
    state = load_state()
    record(state, args)
    code, out, err = run(args, state)
    sys.stdout.write(out)
    sys.stderr.write(err)
    return code


if __name__ == "__main__":
    sys.exit(main())
