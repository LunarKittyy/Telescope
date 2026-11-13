#!/usr/bin/env python3
"""Tell which docs may have gone stale, and check that the paths they mention exist.

Every doc starts with a line like
    <!-- covers: rust/crates/link/src android/app/src/main/kotlin/com/telescope/Pairing.kt; checked: 1cbb8644b -->
`covers` lists the code the doc describes (git pathspecs, `*` allowed) and `checked` is the commit the doc was last
checked against. A doc is flagged when `git log <checked>..HEAD -- <covers>` shows commits. Reading a flagged doc and
finding it still right is fine: bump it with `--bump`.

Usage: docs_fresh.py                  list flagged docs with their commits (always exits 0)
       docs_fresh.py --strict         same, but exit 1 when something is flagged
       docs_fresh.py --bump DOC...    set `checked` to HEAD in those docs
       docs_fresh.py --paths          fail if a doc lacks its line, covers nothing, or mentions a path that doesn't exist
       docs_fresh.py --summary        print the flagged list as Markdown (for a job summary)
"""

import argparse
import os.path
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
LINE = re.compile(r"^<!-- covers:(.*?); checked: (\S+) -->$")
TOP = ("android/", "rust/", "docs/", ".github/", "installer/", "site/", "automation/", "tools/")
# Places a doc may name a file relative to: its own crate, the Android source root and so on
BASES = ["", "rust/", "rust/crates/", "android/", "android/app/src/main/", "android/app/src/main/kotlin/com/telescope/", "android/app/src/test/kotlin/com/telescope/"]
# Mentioned on purpose but not in the repo: things that exist only on a built, installed or user's system
IGNORE = {"lib/colours.lua", "locale/sv.po", "lib/", "lib/fallback/", "lib/fallback-fonts/", "telescope/", "telescope/_build.py", ".previous/", "values/strings.xml"}


def git(*args, check=True) -> str:
    return subprocess.run(["git", "-C", str(ROOT), *args], capture_output=True, text=True, check=check).stdout


def docs() -> list[Path]:
    """Every doc that should carry a line: docs/ without the roadmap's records, rust/ARCHITECTURE.md and the READMEs."""
    found = []
    for name in git("ls-files", "*.md").splitlines():
        if name in ("README.md", "README.es.md", "README.ru.md", "rust/ARCHITECTURE.md") or (
            name.startswith("docs/") and not name.startswith("docs/roadmap/")
        ):
            found.append(ROOT / name)
    return sorted(found)


def read(doc: Path):
    """(covers, checked) from the doc's first line, or None."""
    first = doc.read_text(encoding="utf-8").split("\n", 1)[0]
    m = LINE.match(first)
    return (m.group(1).split(), m.group(2)) if m else None


def rel(doc: Path) -> str:
    return str(doc.relative_to(ROOT))


def flagged() -> list[tuple[str, list[str]]]:
    out = []
    for doc in docs():
        line = read(doc)
        if not line or not line[0]:
            continue
        covers, checked = line
        log = git("log", "--no-merges", "--format=%h %s", f"{checked}..HEAD", "--", *covers, check=False)
        commits = log.splitlines()
        if not commits and not git("cat-file", "-t", checked, check=False).strip():
            commits = [f"(commit {checked} is unknown here: fetch full history)"]
        if commits:
            out.append((rel(doc), commits))
    return out


def report(items, markdown: bool) -> str:
    if not items:
        return "No docs flagged: nothing they cover changed since they were checked.\n"
    lines = []
    if markdown:
        lines += [f"{len(items)} doc(s) cover code that changed since they were checked. Read each one; if it still holds, run `python3 tools/docs_fresh.py --bump <doc>`.", ""]
    for doc, commits in items:
        lines.append(f"**{doc}**" if markdown else doc)
        lines += [("- " if markdown else "  ") + c for c in commits[:15]]
        if len(commits) > 15:
            lines.append(("- " if markdown else "  ") + f"... and {len(commits) - 15} more")
        lines.append("")
    return "\n".join(lines)


def bump(names) -> int:
    head = git("rev-parse", "--short=9", "HEAD").strip()
    for name in names:
        doc = (Path.cwd() / name).resolve()
        line = read(doc) if doc.is_file() else None
        if not line:
            print(f"{name}: no freshness line to bump", file=sys.stderr)
            return 1
        text = doc.read_text(encoding="utf-8")
        first, _, rest = text.partition("\n")
        doc.write_text(re.sub(r"checked: \S+ -->$", f"checked: {head} -->", first) + "\n" + rest, encoding="utf-8")
        print(f"{name}: checked {line[1]} -> {head}")
    return 0


def mentions(text: str):
    """Repo paths in backticks and the targets of Markdown links, as (kind, path)."""
    for m in re.finditer(r"`([^`\n]+)`", text):
        t = m.group(1).strip()
        if re.search(r"[\s<>{}*$|:=()\[\]\"',;!]", t) or t.startswith(("/", "~", "--", "./", "@")) or "//" in t:
            continue
        if t.startswith(TOP) or ("/" in t and re.search(r"\.[A-Za-z0-9]{1,5}$", t.rstrip("/"))):
            yield "code", t
    for m in re.finditer(r"\]\(([^)\s]+)\)", text):
        t = m.group(1).split("#")[0]
        if t and not re.match(r"[a-z]+:", t) and not t.startswith("/"):
            yield "link", t


def check_paths() -> int:
    files = set(git("ls-files").splitlines())
    known = files | {p for f in files for p in (str(q) + "/" for q in Path(f).parents) if p != "./"}
    crates = sorted({f.split("/")[2] for f in files if f.startswith("rust/crates/")})
    bases = BASES + [f"rust/crates/{c}/" for c in crates]
    problems = []
    for doc in docs():
        name = rel(doc)
        line = read(doc)
        if not line:
            problems.append(f"{name}: first line is not a freshness line")
            continue
        for spec in line[0]:
            if not git("ls-files", "--", spec).strip():
                problems.append(f"{name}: covers `{spec}` matches no file")
        text = doc.read_text(encoding="utf-8")
        here = str(doc.parent.relative_to(ROOT))
        here = "" if here == "." else here + "/"
        for kind, t in sorted(set(mentions(text))):
            if t in IGNORE:
                continue
            if kind == "link":
                candidates = [str((Path(here) / t)), t]
            else:
                candidates = [b + t for b in bases] + [here + t]
            norm = [os.path.normpath(c) for c in candidates]
            if kind == "link" and norm[0].startswith(".."):
                continue  # a GitHub-relative link such as ../../releases
            ok = any(c in known or c + "/" in known for c in norm)
            if not ok:
                problems.append(f"{name}: `{t}` does not exist")
    for p in problems:
        print(p)
    if not problems:
        print(f"Every doc has its line and every mentioned path exists ({len(docs())} docs).")
    return 1 if problems else 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--strict", action="store_true", help="exit 1 when a doc is flagged")
    ap.add_argument("--bump", nargs="+", metavar="DOC", help="set checked to HEAD in these docs")
    ap.add_argument("--paths", action="store_true", help="check the lines and the paths docs mention")
    ap.add_argument("--summary", action="store_true", help="print Markdown")
    args = ap.parse_args()
    if args.bump:
        return bump(args.bump)
    if args.paths:
        return check_paths()
    items = flagged()
    print(report(items, args.summary), end="")
    return 1 if items and args.strict else 0


if __name__ == "__main__":
    sys.exit(main())
