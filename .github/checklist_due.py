#!/usr/bin/env python3
"""List the sections of docs/release-checklist.md that the changes since the last release can reach.

Each section under "Only when due" names the files and packages it depends on in a comment. A section is due when one
of them changed. Shared files (in the `shared:` comment) can reach every section, so a change there lists them all as
worth a look with the size of the change. A changed file no comment names is an error: put it in a section.

Usage: checklist_due.py [SINCE] [--until REF]   (SINCE defaults to the newest v* tag)
       checklist_due.py --check-map             (fails if a tracked file isn't named anywhere)
"""

import argparse
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CHECKLIST = ROOT / "docs" / "release-checklist.md"
REQUIREMENTS = re.compile(r"^desktop/(requirements[\w-]*|constraints)\.txt$")


def git(*args) -> str:
    return subprocess.run(["git", "-C", str(ROOT), *args], capture_output=True, text=True, check=True).stdout


def pattern(glob: str) -> re.Pattern:
    """A glob where * stays inside one folder and ** crosses folders."""
    rx = re.escape(glob).replace(r"\*\*", "\0").replace(r"\*", "[^/]*").replace("\0", ".*")
    return re.compile(rx + "$")


def parse(text: str):
    """The checklist's comments: shared and ignored (patterns, packages), and (title, patterns, packages) per section."""
    def comment(kind, body):
        files, _, packages = body.partition("; packages:")
        return [pattern(g) for g in files.split()], packages.split()

    def merged(kind):
        found = [comment(kind, m.group(1)) for m in re.finditer(rf"<!-- {kind}:(.*?)-->", text, re.S)]
        return [p for f, _ in found for p in f], [p for _, pk in found for p in pk]

    sections = []
    for m in re.finditer(r"^### (.+?)\n(.*?)(?=^### |\Z)", text, re.S | re.M):
        due = re.search(r"<!-- due:(.*?)-->", m.group(2), re.S)
        if due:
            sections.append((m.group(1).strip(), *comment("due", due.group(1))))
    return merged("shared"), merged("ignore"), sections


def matches(path, patterns) -> bool:
    return any(p.match(path) for p in patterns)


def changed_packages(since, until) -> set:
    """Packages whose pin changed in the desktop's requirements and constraints files."""
    diff = git("diff", "-U0", since, until, "--", "desktop/requirements*.txt", "desktop/constraints.txt")
    names = set()
    for line in diff.splitlines():
        if line[:1] in "+-" and not line.startswith(("+++", "---")):
            m = re.match(r"[+-]\s*([A-Za-z0-9][A-Za-z0-9._-]*)", line)
            if m:
                names.add(m.group(1).lower().replace("_", "-"))
    return names


def folded(paths, sizes) -> list:
    """One line per file, or per folder for more than three files that aren't code (the code is what needs judging)."""
    by_dir = {}
    for f in paths:
        by_dir.setdefault(f.rsplit("/", 1)[0] if "/" in f else "", []).append(f)
    lines = []
    for folder, group in by_dir.items():
        if len(group) > 3 and not any(f.endswith((".py", ".kt")) for f in group):
            nums = [sizes.get(f, "+0 -0").replace("-", " ").split() for f in group]
            added = sum(int(n[0].lstrip("+")) for n in nums if n[0].lstrip("+").isdigit())
            removed = sum(int(n[1]) for n in nums if len(n) > 1 and n[1].isdigit())
            lines.append(f"{folder}/ ({len(group)} files, +{added} -{removed})")
        else:
            lines += [f"{f} {sizes.get(f, '')}".rstrip() for f in group]
    return lines


def check_map(shared, ignored, sections) -> int:
    named = shared[0] + ignored[0] + [p for _, files, _ in sections for p in files]
    missing = [f for f in git("ls-files").splitlines() if not matches(f, named)]
    for f in missing:
        print(f"Not in any section of the release checklist: {f}")
    return 1 if missing else 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("since", nargs="?", help="the last release's tag (default: the newest v* tag)")
    parser.add_argument("--until", default="HEAD")
    parser.add_argument("--check-map", action="store_true")
    args = parser.parse_args()
    shared, ignored, sections = parse(CHECKLIST.read_text())
    if args.check_map:
        return check_map(shared, ignored, sections)

    since = args.since or git("describe", "--tags", "--abbrev=0", "--match", "v*", args.until).strip()
    files = git("diff", "--name-only", since, args.until).splitlines()
    sizes = {}
    for line in git("diff", "--numstat", since, args.until).splitlines():
        added, removed, path = line.split("\t", 2)
        sizes[path] = f"+{added} -{removed}"
    packages = changed_packages(since, args.until)

    due = {title: [] for title, _, _ in sections}
    shared_changes, unmapped = [], []
    for f in files:
        hits = [title for title, globs, _ in sections if matches(f, globs)]
        for title in hits:
            due[title].append(f)
        if REQUIREMENTS.match(f):
            continue  # judged by which packages changed, below
        if matches(f, shared[0]):
            shared_changes.append(f)
        elif not hits and not matches(f, ignored[0]):
            unmapped.append(f)
    known = {p for _, _, pkgs in sections for p in pkgs}
    for title, _, pkgs in sections:
        due[title] += [f"package {p}" for p in sorted(packages & set(pkgs))]
    shared_packages = sorted(packages & set(shared[1]))
    unknown_packages = sorted(packages - known - set(shared[1]) - set(ignored[1]))

    print(f"Changes from {since} to {args.until}: {len(files)} files\n")
    listed = [t for t in due if due[t]]
    if listed:
        print("Due (something it depends on changed):")
        for title in listed:
            print(f"  {title}")
            for why in due[title]:
                print(f"      {why} {sizes.get(why, '')}".rstrip())
    if shared_changes or shared_packages or unknown_packages:
        print("\nShared code changed, so every other section is worth a look. How far it reaches is a judgement call:")
        for line in folded(shared_changes, sizes):
            print(f"      {line}")
        for p in shared_packages:
            print(f"      package {p}")
        for p in unknown_packages:
            print(f"      package {p} (in no section's list, so treated as shared)")
        rest = [t for t in due if not due[t]]
        if rest:
            print("  Not due on their own: " + ", ".join(rest))
    if not listed and not shared_changes and not shared_packages and not unknown_packages:
        print("No section is due: only the every-release pass.")
    if unmapped:
        print("\nThese changed files aren't in any section, so the list above can't be trusted until they are:")
        for f in unmapped:
            print(f"      {f}")
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
