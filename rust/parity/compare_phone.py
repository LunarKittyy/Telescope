#!/usr/bin/env python3
"""Compares two fake-phone recordings (phone_session.py or fake_phone.py --record) phase by phase.

What a desktop tells the phone has to match exactly: every POST (session, control, still, unpair) with its body, and
each action's values in the same order. Different actions may interleave differently, since the burst of settings
at the start of a stream comes from several threads (--strict-order holds that to the same order too, --unordered
lets go of order altogether). What it asks has to cover the same requests: the set of GETs (path and query names),
since how often a desktop polls /v1/ping or /v1/state depends on timing. --counts shows how often each GET came.

    python compare_phone.py python.jsonl rust.jsonl [--strict-order | --unordered] [--counts]

Exits 1 when they differ.
"""

import argparse
import difflib
import json
import sys
from collections import Counter
from pathlib import Path


def phases(path: Path) -> dict:
    """{phase name: [requests]}, split at the recording's marks; requests before the first mark go under "start"."""
    found, current = {"start": []}, "start"
    for line in path.read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        entry = json.loads(line)
        if "mark" in entry:
            current = entry["mark"]
            found.setdefault(current, [])
        else:
            found[current].append(entry)
    return {k: v for k, v in found.items() if v or k != "start"}


def told(entry) -> str:
    return f"{entry['method']} :{entry['port']} {entry['path']} {json.dumps(entry.get('body'), sort_keys=True, ensure_ascii=False)}"


def asked(entry) -> str:
    query = "&".join(sorted(entry.get("query") or {}))
    return f"GET :{entry['port']} {entry['path']}" + (f"?{query}" if query else "")


def per_action(sent: list) -> list:
    """sent regrouped so each action's requests stay in order but different actions don't interleave."""
    groups = {}
    for entry in sent:
        body = entry.get("body")
        action = body.get("action") if isinstance(body, dict) else None
        groups.setdefault((entry["method"], entry["port"], entry["path"], str(action)), []).append(told(entry))
    return [line for key in sorted(groups) for line in groups[key]]


def compare(a: dict, b: dict, unordered=False, counts=False, names=("a", "b"), strict=False) -> list:
    lines = []
    for phase in dict.fromkeys(list(a) + list(b)):
        if phase not in a or phase not in b:
            lines.append(f"[{phase}] only in {names[0] if phase in a else names[1]}")
            continue
        out = []
        order = (lambda sent: [told(e) for e in sent]) if strict or unordered else per_action
        sent_a = order([e for e in a[phase] if e["method"] != "GET"])
        sent_b = order([e for e in b[phase] if e["method"] != "GET"])
        if unordered:
            extra_a, extra_b = Counter(sent_a) - Counter(sent_b), Counter(sent_b) - Counter(sent_a)
            out += [f"  only {names[0]}: {k}" + (f" (x{n})" if n > 1 else "") for k, n in sorted(extra_a.items())]
            out += [f"  only {names[1]}: {k}" + (f" (x{n})" if n > 1 else "") for k, n in sorted(extra_b.items())]
        elif sent_a != sent_b:
            out += [f"  {line}" for line in difflib.unified_diff(sent_a, sent_b, names[0], names[1], lineterm="", n=1)
                    if not line.startswith(("---", "+++"))]
        get_a = Counter(asked(e) for e in a[phase] if e["method"] == "GET")
        get_b = Counter(asked(e) for e in b[phase] if e["method"] == "GET")
        out += [f"  only {names[0]} asks: {k}" for k in sorted(set(get_a) - set(get_b))]
        out += [f"  only {names[1]} asks: {k}" for k in sorted(set(get_b) - set(get_a))]
        if out:
            lines.append(f"[{phase}] differs")
            lines += out
        if counts:
            for k in sorted(set(get_a) | set(get_b)):
                lines.append(f"  [{phase}] {k}: {get_a[k]} vs {get_b[k]}")
    return lines


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("a", type=Path)
    parser.add_argument("b", type=Path)
    order = parser.add_mutually_exclusive_group()
    order.add_argument("--strict-order", action="store_true", help="every POST in the same order, across actions too")
    order.add_argument("--unordered", action="store_true", help="compare what's told as a set, not a sequence")
    parser.add_argument("--counts", action="store_true", help="also show how often each GET came")
    args = parser.parse_args(argv)
    lines = compare(phases(args.a), phases(args.b), args.unordered, args.counts, (args.a.name, args.b.name),
                    args.strict_order)
    differs = any(not line.startswith("  [") for line in lines)
    print("\n".join(lines) if lines else "")
    print("The recordings differ" if differs else "The recordings match")
    return 1 if differs else 0


if __name__ == "__main__":
    sys.exit(main())
