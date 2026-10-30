"""Records what the Python app's sanitize() does with text around zero-width, combining and other odd characters, and which characters
Python's re calls word, digit and space, for rust/crates/core/fixtures/sanitize.json (diagnostics_tests.rs reads it).

    python3 rust/parity/record_sanitize.py            # rewrites the fixture
"""

import json
import os
import re
import sys
import unicodedata
from pathlib import Path

HOME = "/home/someone"
ROOT = Path(__file__).resolve().parents[2]
OUT = ROOT / "rust" / "crates" / "core" / "fixtures" / "sanitize.json"

os.environ["HOME"] = HOME
sys.path.insert(0, str(ROOT / "desktop"))
from telescope import diagnostics  # noqa: E402

# Zero-width and format, combining marks, connectors, other numbers, circled letters, digits of other scripts, the four letters
# ignore-case takes as ASCII ones, and every kind of space
ODD = [
    0x200B, 0x200C, 0x200D, 0x200E, 0x200F, 0x2060, 0x2061, 0x2062, 0x2063, 0x2064, 0xFEFF, 0xAD, 0x61C, 0x180E, 0x202A, 0x202B,
    0x202C, 0x202D, 0x202E, 0x2066, 0x2067, 0x2068, 0x2069, 0x34F, 0x17B4, 0x17B5, 0xFE00, 0xFE0F, 0xE0100, 0xE0020, 0x1D173,
    0x300, 0x301, 0x308, 0x345, 0x483, 0x591, 0x5BF, 0x610, 0x64B, 0x670, 0x903, 0x93C, 0x9BE, 0x1AB0, 0x1DC0, 0x20D0, 0x20DD,
    0x20E3, 0x302A, 0x3099, 0xFE20,
    0x203F, 0x2040, 0x2054, 0xFE33, 0xFE4D, 0xFF3F,
    0xB2, 0xB3, 0xB9, 0xBC, 0x2070, 0x2080, 0x2460, 0x2776, 0x3220, 0x2160, 0x3007,
    0x660, 0x6F0, 0x966, 0xFF10, 0x1D7CE,
    0x24B6, 0x24D0, 0x1F130, 0x1F170,
    0xE9, 0xDF, 0x3B1, 0x4E2D, 0x1E9E, 0x5D0, 0xB7, 0x1F600,
    0x130, 0x131, 0x17F, 0x212A,
    0x1C, 0x1D, 0x1E, 0x1F, 0x85, 0xA0, 0x1680, 0x2000, 0x2003, 0x200A, 0x2028, 0x2029, 0x202F, 0x205F, 0x3000,
]
SECRET = "AbCdEfGhIjKlMnOpQrStUvWx"
TEMPLATES = [
    "code={x}" + SECRET,
    "{x}/" + SECRET + "yz",
    "a{x}-" + SECRET,
    "x{x}http://host/path?q=1 end",
    "{x}ttp://host.example/p",
    "http://host/a{x}b c",
    "{x}10.0.0.5 and 10.0.0.6{x}",
    "{x}10.0.0.5:8080{x}x",
    "{x}.1.1.1 and 1.1.1.{x}",
    "{x}fe80::1c2:3aff:fe44:5566{x}",
    "{x}1:2:3:4:5:6:7:8{x}",
    "token{x}secret99 and tok{x}en=abc",
    "bearer{x}abc123 {x}token=xyz",
    "to{x}en=abc and {x}ecret: def",
]
PLAIN = [
    "", "code=" + SECRET, "GET http://192.168.1.20:8765/v1/state failed", "phone at 10.0.0.5:8766 didn't answer",
    "reached fe80::1c2:3aff:fe44:5566 over wifi", "token=Zx9_kq2-LmN8pQrS7tUvWxYz0aBcDeF", "12:30:45 stream dropped, version 0.5.0",
]


def ranges(pattern):
    found, start = [], None
    for cp in range(sys.maxunicode + 2):
        hit = cp <= sys.maxunicode and not 0xD800 <= cp < 0xE000 and bool(pattern.fullmatch(chr(cp)))
        if hit and start is None:
            start = cp
        if not hit and start is not None:
            found.append([start, cp - 1])
            start = None
    return found


def main():
    inputs = PLAIN + [t.format(x=chr(cp)) for t in TEMPLATES for cp in ODD]
    out = {
        "python": sys.version.split()[0],
        "unicode": unicodedata.unidata_version,
        "home": HOME,
        "cases": [[text, diagnostics.sanitize(text)] for text in inputs],
        "word": ranges(re.compile(r"\w")),
        "digit": ranges(re.compile(r"\d")),
        "space": ranges(re.compile(r"\s")),
    }
    OUT.parent.mkdir(parents=True, exist_ok=True)
    lines = ['  "%s": %s' % (k, json.dumps(v)) for k, v in out.items() if k != "cases"]
    cases = ",\n".join("    " + json.dumps(c) for c in out["cases"])
    OUT.write_text("{\n" + ",\n".join(lines) + ',\n  "cases": [\n' + cases + "\n  ]\n}\n")
    print(f"{len(out['cases'])} cases written to {OUT}")


if __name__ == "__main__":
    main()
