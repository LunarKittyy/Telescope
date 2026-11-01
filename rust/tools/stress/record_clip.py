#!/usr/bin/env python3
"""Records the H.264 stream of a paired phone over USB for stress.py, with the digital crop sweeping around so the whole
picture moves and the phone's own noise stays in. Needs a profile folder that has the phone paired, and adb set up.

  record_clip.py OUT.h264 --profile FOLDER --phone-name "Galaxy S10+" [--seconds 32] [--lens 0]
"""

import argparse
import http.client
import json
import math
import re
import ssl
import subprocess
import threading
import time
from pathlib import Path


def adb_forward(remote: int) -> int:
    out = subprocess.run(["adb", "forward", "tcp:0", f"tcp:{remote}"], check=True, capture_output=True, text=True).stdout
    return int(re.search(r"\d+", out).group())


def connection(port: int) -> http.client.HTTPSConnection:
    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE
    return http.client.HTTPSConnection("127.0.0.1", port, context=ctx, timeout=10)


def post(port: int, token: str, path: str, body: dict) -> dict:
    con = connection(port)
    con.request("POST", path, json.dumps(body), {"Authorization": f"Bearer {token}", "Content-Type": "application/json"})
    reply = json.loads(con.getresponse().read() or b"{}")
    con.close()
    return reply


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("out", type=Path)
    parser.add_argument("--profile", type=Path, required=True)
    parser.add_argument("--phone-name", required=True)
    parser.add_argument("--seconds", type=float, default=32)
    parser.add_argument("--lens", default="0")
    args = parser.parse_args()
    config = json.loads((args.profile / "telescope_config.json").read_text())
    phone = next(p for p in config["plugin_configs"]["connection"]["phones"] if p["name"] == args.phone_name)
    token = phone["token"]
    session, stream = adb_forward(8766), adb_forward(8080)
    print(post(session, token, "/v1/session", {"action": "start", "width": 1920, "height": 1080, "fps": 30}), flush=True)
    # The lens used last could be anything, and the wrong one on a phone lying on a desk sees nothing
    print(post(stream, token, "/v1/control", {"action": "camera", "id": args.lens}), flush=True)
    time.sleep(3)
    done = threading.Event()

    def sweep():
        start = time.monotonic()
        while not done.is_set():
            t = time.monotonic() - start
            crop = 1.45 + 0.4 * math.sin(t * 0.5)
            x, y = 0.5 + 0.28 * math.sin(t * 0.9), 0.5 + 0.28 * math.cos(t * 0.63)
            try:
                post(stream, token, "/v1/control", {"action": "zoom", "ratio": 1.0, "crop": round(crop, 3), "x": round(x, 3), "y": round(y, 3)})
            except OSError:
                pass
            time.sleep(0.05)

    con = connection(stream)
    con.request("GET", "/v1/video.h264", headers={"Authorization": f"Bearer {token}"})
    reply = con.getresponse()
    print("stream:", reply.status, flush=True)
    threading.Thread(target=sweep, daemon=True).start()
    until = time.monotonic() + args.seconds
    with args.out.open("wb") as f:
        # The phone puts no access unit delimiter before its first frame, and fake_phone.py splits on them
        f.write(b"\x00\x00\x00\x01\x09\xf0")
        while time.monotonic() < until:
            chunk = reply.read1(1 << 16)
            if not chunk:
                break
            f.write(chunk)
    done.set()
    con.close()
    time.sleep(0.3)
    print(post(stream, token, "/v1/control", {"action": "zoom", "ratio": 1.0, "crop": 1.0, "x": 0.5, "y": 0.5}))
    print(post(session, token, "/v1/session", {"action": "stop"}))
    subprocess.run(["adb", "forward", "--remove", f"tcp:{session}"])
    subprocess.run(["adb", "forward", "--remove", f"tcp:{stream}"])
    print(f"{args.out}: {args.out.stat().st_size / 1e6:.1f} MB")


if __name__ == "__main__":
    main()
