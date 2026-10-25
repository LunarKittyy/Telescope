#!/usr/bin/env python3
# Small driver for the emulator: screenshots plus the texts on screen, kept in OUT/phone.json
import json
import re
import subprocess
import sys
import time
import xml.etree.ElementTree as ET
from pathlib import Path

OUT = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(".")
STATE = OUT / "phone.json"


def adb(*a, check=True):
    return subprocess.run(["adb", "-e", *a], capture_output=True, text=True, check=check).stdout


def nodes():
    adb("shell", "uiautomator", "dump", "/sdcard/ui.xml", check=False)
    xml = adb("shell", "cat", "/sdcard/ui.xml", check=False)
    try:
        root = ET.fromstring(xml[xml.index("<?xml"):] if "<?xml" in xml else xml)
    except (ET.ParseError, ValueError):
        return []
    out = []
    for n in root.iter("node"):
        b = re.findall(r"\d+", n.get("bounds", "0,0,0,0"))
        out.append({"text": n.get("text", ""), "desc": n.get("content-desc", ""), "bounds": list(map(int, b)),
                    "pkg": n.get("package", ""), "scroll": n.get("scrollable") == "true"})
    return out


def shoot(name, title, note=""):
    time.sleep(1.2)
    data = json.loads(STATE.read_text()) if STATE.exists() else {"shots": []}
    data["shots"] = [s for s in data["shots"] if s["name"] != name]
    path = OUT / f"phone-{len(data['shots']):02d}-{name}.png"
    subprocess.run(["adb", "-e", "emu", "screenrecord", "screenshot", str(path)], capture_output=True, check=True)
    texts = sorted({t for n in nodes() for t in (n["text"], n["desc"]) if t})
    data["shots"].append({"name": name, "file": path.name, "title": title, "note": note, "texts": texts})
    STATE.write_text(json.dumps(data, ensure_ascii=False, indent=1))
    print(path.name, len(texts), "texts")


def find(text):
    for n in nodes():
        if text in (n["text"], n["desc"]) or (len(text) > 3 and text in n["text"]):
            b = n["bounds"]
            return (b[0] + b[2]) // 2, (b[1] + b[3]) // 2
    return None


def tap(text):
    at = find(text)
    if not at:
        print("not found:", text)
        return False
    adb("shell", "input", "tap", str(at[0]), str(at[1]))
    time.sleep(1)
    return True


def swipe_up(dist=1300):
    adb("shell", "input", "swipe", "540", "1900", "540", str(1900 - dist), "600")
    time.sleep(1)


if __name__ == "__main__":
    cmd = sys.argv[2]
    if cmd == "shoot":
        shoot(sys.argv[3], sys.argv[4], sys.argv[5] if len(sys.argv) > 5 else "")
    elif cmd == "tap":
        tap(sys.argv[3])
    elif cmd == "texts":
        for n in nodes():
            if n["text"] or n["desc"]:
                print(n["bounds"], repr(n["text"] or n["desc"]))
    elif cmd == "swipe":
        swipe_up(int(sys.argv[3]) if len(sys.argv) > 3 else 1300)
