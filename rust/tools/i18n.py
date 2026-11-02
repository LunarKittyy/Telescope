#!/usr/bin/env python3
"""Keeps the desktop app's translations in step with the code (docs/translating.md). Run it from rust/.

  python tools/i18n.py extract          find every text the code shows, write locale/telescope.pot
                                        and bring each locale/<lang>.po up to date with it
  python tools/i18n.py extract --forget also drop the texts no longer in the code, translated or not
  python tools/i18n.py new sv           start a Swedish catalog
  python tools/i18n.py status [sv]      how much of each language is done, and any broken {fields}
  python tools/i18n.py todo sv          the Swedish texts still missing, ready to paste into a .po

The texts are the ones in tr(), n_(), trn(), tr_ctx(), tr_with() and trn_with() calls in the Rust code and in Slint's @tr().
Calls in tests, comments and the i18n code itself don't count. A `// Translators: ...` comment right above a call becomes the
text's note. A text the code only passes along as a plain string (a table the card walks, a helper that calls tr() later) stays
while the same string is still in the code and the template already had it: mark new ones with n_().

A text that leaves the code stays in each .po as an obsolete (#~) entry when it has a translation, and comes back with it if the
text returns. app/tests/texts_in_code.rs scans the code on its own and checks this tool's template against it.
"""

import argparse
import re
import sys
from pathlib import Path
from typing import Dict, Iterable, List, Optional, Tuple

RUST = Path(__file__).resolve().parent.parent
LOCALE_DIR = RUST / "locale"
TEMPLATE = LOCALE_DIR / "telescope.pot"
DRAFT_FIELD = "X-Telescope-Draft"  # in a catalog's header: a first draft, shown as (beta) in the language list

# ── .po files ────────────────────────────────────────────────────────────────


class Entry:
    """One message of a .po file."""

    def __init__(self, msgid: str = "", plural: str = "", ctx: str = ""):
        self.msgid = msgid
        self.plural = plural
        self.ctx = ctx
        self.msgstr: List[str] = []
        self.comments: List[str] = []     # "#." from a "// Translators:" comment
        self.locations: List[str] = []    # "#:" file:line
        self.flags: List[str] = []        # "#," e.g. fuzzy, python-brace-format
        self.obsolete = False

    @property
    def key(self) -> Tuple[str, str]:
        return self.ctx, self.msgid

    @property
    def fuzzy(self) -> bool:
        return "fuzzy" in self.flags


_ESCAPES = {"n": "\n", "t": "\t", '"': '"', "\\": "\\"}


def _unquote(s: str) -> str:
    s = s.strip()
    if len(s) < 2 or s[0] != '"' or s[-1] != '"':
        raise ValueError(f"not a quoted string: {s}")
    return re.sub(r'\\(.)', lambda m: _ESCAPES.get(m.group(1), m.group(1)), s[1:-1])


def _quote(s: str) -> str:
    body = s.replace("\\", "\\\\").replace('"', '\\"').replace("\t", "\\t")
    if "\n" in body.rstrip("\n"):
        lines = body.split("\n")
        parts = [line + "\\n" for line in lines[:-1]] + ([lines[-1]] if lines[-1] else [])
        return '""\n' + "\n".join(f'"{p}"' for p in parts)
    return '"' + body.replace("\n", "\\n") + '"'


def read_po(path: Path, obsolete: bool = False) -> Tuple[List[Entry], str]:
    """The entries of a .po file (the obsolete ones too when asked) and its header (the msgstr of the empty msgid)."""
    entries: List[Entry] = []
    entry = Entry()
    field: Optional[Tuple[str, int]] = None
    started = False

    def finish():
        nonlocal entry, started
        if started:
            entries.append(entry)
        entry, started = Entry(), False

    for number, raw in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        line = raw.strip()
        gone = line.startswith("#~")
        if gone:
            line = line[2:].strip()
        if not line:
            finish()
            field = None
            continue
        if line.startswith("#"):
            if started and field is not None:
                finish()
                field = None
            if line.startswith("#."):
                entry.comments.append(line[2:].strip())
            elif line.startswith("#:"):
                entry.locations.extend(line[2:].split())
            elif line.startswith("#,"):
                entry.flags.extend(f.strip() for f in line[2:].split(",") if f.strip())
            continue
        if line.startswith('"'):
            if field is None:
                raise ValueError(f"{path.name}:{number}: text outside a field")
            name, index = field
            text = _unquote(line)
            if name == "msgstr":
                entry.msgstr[index] += text
            else:
                setattr(entry, name, getattr(entry, name) + text)
            continue
        keyword, _, value = line.partition(" ")
        if keyword in ("msgctxt", "msgid") and started and field and field[0] == "msgstr":
            finish()
        started = True
        entry.obsolete = entry.obsolete or gone
        if keyword == "msgctxt":
            entry.ctx, field = _unquote(value), ("ctx", 0)
        elif keyword == "msgid":
            entry.msgid, field = _unquote(value), ("msgid", 0)
        elif keyword == "msgid_plural":
            entry.plural, field = _unquote(value), ("plural", 0)
        elif keyword == "msgstr" or keyword.startswith("msgstr["):
            index = int(keyword[7:-1]) if keyword != "msgstr" else 0
            while len(entry.msgstr) <= index:
                entry.msgstr.append("")
            entry.msgstr[index] = _unquote(value)
            field = ("msgstr", index)
        else:
            raise ValueError(f"{path.name}:{number}: unknown line {raw!r}")
    finish()
    header = ""
    for e in entries:
        if e.msgid == "" and not e.ctx:
            header = e.msgstr[0] if e.msgstr else ""
    return [e for e in entries if e.msgid and (obsolete or not e.obsolete)], header


def write_po(path: Path, header: str, entries: Iterable[Entry], header_comments: Iterable[str] = ()) -> None:
    out = [f"# {c}" if c else "#" for c in header_comments]
    out += ['msgid ""', "msgstr " + _quote(header), ""]
    for e in entries:
        prefix = "#~ " if e.obsolete else ""
        out += [f"#. {c}" for c in e.comments]
        if e.locations and not e.obsolete:
            out.append("#: " + " ".join(e.locations))
        if e.flags:
            out.append("#, " + ", ".join(e.flags))
        if e.ctx:
            out.append(prefix + "msgctxt " + _quote(e.ctx))
        out.append(prefix + "msgid " + _quote(e.msgid))
        if e.plural:
            out.append(prefix + "msgid_plural " + _quote(e.plural))
            for i, s in enumerate(e.msgstr or ["", ""]):
                out.append(prefix + f"msgstr[{i}] " + _quote(s))
        else:
            out.append(prefix + "msgstr " + _quote(e.msgstr[0] if e.msgstr else ""))
        out.append("")
    path.write_text("\n".join(out), encoding="utf-8")


def header_field(header: str, name: str) -> str:
    for line in header.splitlines():
        key, _, value = line.partition(":")
        if key.strip().lower() == name.lower():
            return value.strip()
    return ""


def plural_count(header: str) -> int:
    m = re.search(r"nplurals\s*=\s*(\d+)", header_field(header, "Plural-Forms"))
    return int(m.group(1)) if m else 2


FIELD = re.compile(r"(?<!\{)\{([A-Za-z_][A-Za-z0-9_]*)(?:[:!][^{}]*)?\}(?!\})")


def fields(text: str) -> set:
    """The {fields} a text fills in."""
    return set(FIELD.findall(text))


# ── Finding the texts in the code ────────────────────────────────────────────

SKIP_DIRS = {"target", "tests", "fixtures", "example-plugins"}
# The files that read or write the calls themselves, so their examples aren't texts
READERS = {"source.rs", "texts.rs", "text.rs", "i18n.rs"}
# What a call's arguments mean: which one is the English, the plural, the context
SHAPES = {"tr": (0, None, None), "n_": (0, None, None), "tr_ctx": (1, None, 0), "tr_with": (0, None, 1),
          "trn": (0, 1, None), "trn_with": (0, 1, 3)}


def source_files() -> List[Path]:
    files: List[Path] = []

    def walk(folder: Path):
        for path in sorted(folder.iterdir()):
            if path.is_dir():
                if path.name not in SKIP_DIRS:
                    walk(path)
            elif path.suffix == ".slint" or (path.suffix == ".rs" and not path.name.endswith("_tests.rs")
                                             and path.name != "tests.rs" and path.name not in READERS):
                files.append(path)

    walk(RUST / "crates")
    walk(RUST / "app" / "src")
    return sorted(files, key=lambda p: p.relative_to(RUST).as_posix())


def literal(s: str) -> Optional[Tuple[str, int]]:
    """The string literal at the start of s (Rust or Slint escapes) and its length."""
    if not s.startswith('"'):
        return None
    out: List[str] = []
    i = 1
    while i < len(s):
        c = s[i]
        if c == '"':
            return "".join(out), i + 1
        if c == "\\":
            i += 1
            if i >= len(s):
                return None
            e = s[i]
            if e == "n":
                out.append("\n")
            elif e == "t":
                out.append("\t")
            elif e == "\n":
                while i + 1 < len(s) and s[i + 1] in " \t":
                    i += 1
            elif e == "u":
                end = s.find("}", i)
                if end < 0:
                    return None
                try:
                    out.append(chr(int(s[i + 2:end], 16)))
                except ValueError:
                    return None
                i = end
            else:
                out.append(e)
        else:
            out.append(c)
        i += 1
    return None


def split_args(text: str, start: int) -> Optional[List[str]]:
    """The arguments of the call whose opening parenthesis is just before text[start], at its own depth."""
    depth, args, begin, i = 0, [], start, start
    while i < len(text):
        c = text[i]
        if c == '"':
            lit = literal(text[i:])
            if lit is None:
                return None
            i += lit[1]
            continue
        if c in "([{":
            depth += 1
        elif c in ")]}":
            if depth == 0:
                args.append(text[begin:i])
                return args
            depth -= 1
        elif c == "," and depth == 0:
            args.append(text[begin:i])
            begin = i + 1
        i += 1
    return None


def only_literal(arg: str) -> Optional[str]:
    arg = arg.strip()
    lit = literal(arg)
    return lit[0] if lit and lit[1] == len(arg) else None


def in_comment(prefix: str) -> bool:
    """Whether the text before a call, on its line, has opened a // comment outside a string."""
    i = 0
    while i < len(prefix):
        if prefix[i] == '"':
            lit = literal(prefix[i:] + '"')
            i += lit[1] if lit else len(prefix)
            continue
        if prefix.startswith("//", i):
            return True
        i += 1
    return False


def translator_note(lines: List[str], lineno: int) -> List[str]:
    """The "// Translators: ..." comment block right above a line, as one note."""
    notes: List[str] = []
    i = lineno - 2
    while i >= 0 and lines[i].strip().startswith("//"):
        text = lines[i].strip().lstrip("/").strip()
        if text.lower().startswith("translators:"):
            notes.insert(0, text.split(":", 1)[1].strip())
            return [" ".join(notes)]
        notes.insert(0, text)
        i -= 1
    return []


def find_texts(path: Path) -> Tuple[List[Entry], List[str]]:
    """Every text one source file shows through a call, and what's wrong with any that can't be found."""
    text = path.read_text(encoding="utf-8")
    lines = text.splitlines()
    where = path.relative_to(RUST.parent).as_posix()
    slint = path.suffix == ".slint"
    # Some .rs files write Slint source as text, so @tr() is looked for there too
    names = ["@tr"] if slint else list(SHAPES) + ["@tr"]
    found: List[Tuple[int, Entry]] = []
    problems: List[str] = []
    for name in names:
        for m in re.finditer(r"@tr\(" if name == "@tr" else r"(?<![\w@])" + re.escape(name) + r"\(", text):
            at, line_start = m.start(), text.rfind("\n", 0, m.start()) + 1
            if in_comment(text[line_start:at]) or text[:at].rstrip().endswith("fn"):
                continue
            args = split_args(text, m.end())
            lineno = text.count("\n", 0, at) + 1
            here = f"{where}:{lineno}"
            if args is None:
                continue
            entry = Entry()
            if name == "@tr":
                first, arrow, second = args[0].partition("=>")
                msgid = only_literal(second if arrow else first)
                ctx = only_literal(first) if arrow else ""
                if msgid is None or ctx is None:
                    if slint:
                        problems.append(f"{here}: @tr() needs plain string literals")
                    continue
                entry.msgid, entry.ctx = msgid, ctx
            else:
                text_at, plural_at, ctx_at = SHAPES[name]
                values = [only_literal(a) if i < len(args) else None for i, a in enumerate(args)]
                msgid = values[text_at] if text_at < len(values) else None
                if msgid is None:
                    if name != "tr" and name != "tr_with" and name != "tr_ctx" and name != "trn_with":
                        problems.append(f"{here}: {name}() needs plain string literals, so the text can be translated")
                    continue  # tr(variable) translates a text marked with n_() elsewhere
                entry.msgid = msgid
                if plural_at is not None:
                    plural = values[plural_at] if plural_at < len(values) else None
                    if plural is None:
                        problems.append(f"{here}: {name}() needs plain string literals, so the text can be translated")
                        continue
                    entry.plural = plural
                if ctx_at is not None and ctx_at < len(values):
                    entry.ctx = values[ctx_at] or ""
            entry.locations.append(here)
            entry.comments = translator_note(lines, lineno)
            if fields(entry.msgid) or fields(entry.plural):
                entry.flags.append("python-brace-format")
            found.append((at, entry))
    return [e for _, e in sorted(found, key=lambda f: f[0])], problems


def plain_literals(path: Path) -> Dict[str, str]:
    """Every string literal in a source file outside comments: text -> file:line of its first place."""
    text = path.read_text(encoding="utf-8")
    where = path.relative_to(RUST.parent).as_posix()
    found: Dict[str, str] = {}
    i = 0
    while i < len(text):
        if text.startswith("//", i):
            i = text.find("\n", i)
            if i < 0:
                break
            continue
        if text[i] == '"':
            lit = literal(text[i:])
            if lit:
                found.setdefault(lit[0], f"{where}:{text.count(chr(10), 0, i) + 1}")
                i += lit[1]
                continue
        i += 1
    return found


def find_all(previous: List[Entry]) -> Tuple[List[Entry], List[str]]:
    """Every text in the code, merged by (context, English), in the order they first appear."""
    merged: Dict[Tuple[str, str], Entry] = {}
    problems: List[str] = []
    literals: Dict[str, str] = {}
    for path in source_files():
        entries, bad = find_texts(path)
        # The plugin host hands on what a plugin passes, which is no text of the app's
        problems += [] if "plugin-host" in path.parts else bad
        for e in entries:
            have = merged.get(e.key)
            if have is None:
                merged[e.key] = e
                continue
            have.locations += e.locations
            have.comments += [c for c in e.comments if c not in have.comments]
            if e.plural and have.plural and e.plural != have.plural:
                problems.append(f"{e.locations[0]}: {e.msgid!r} has another plural at {have.locations[0]}")
            have.plural = have.plural or e.plural
        for text, place in plain_literals(path).items():
            literals.setdefault(text, place)
    # The i18n code isn't scanned for calls, but it says a few texts of its own (the joining of a list, a percentage)
    for text, place in plain_literals(RUST / "crates" / "core" / "src" / "i18n.rs").items():
        literals.setdefault(text, place)
    for old in previous:
        if old.key in merged:
            if not merged[old.key].comments:
                merged[old.key].comments = list(old.comments)
        elif old.msgid in literals:
            kept = Entry(old.msgid, old.plural, old.ctx)
            kept.comments, kept.flags = list(old.comments), list(old.flags)
            kept.locations = [literals[old.msgid]]
            merged[old.key] = kept
    return list(merged.values()), problems


# ── The commands ─────────────────────────────────────────────────────────────

# Plural-Forms for some languages; anything else, look up and fix the header by hand
_LIKE_ENGLISH = "nplurals=2; plural=(n != 1);"
PLURALS = {
    "": _LIKE_ENGLISH,
    **{lang: _LIKE_ENGLISH for lang in ("sv", "de", "nl", "da", "nb", "nn", "no", "fi", "et", "es", "it", "pt", "el",
                                        "hu", "bg", "ca", "eu", "gl", "eo", "af", "hi", "bn", "ta", "te", "ur", "sw")},
    "fr": "nplurals=2; plural=(n > 1);", "pt_BR": "nplurals=2; plural=(n > 1);",
    "ja": "nplurals=1; plural=0;", "ko": "nplurals=1; plural=0;", "zh_CN": "nplurals=1; plural=0;",
    "zh_TW": "nplurals=1; plural=0;", "vi": "nplurals=1; plural=0;", "th": "nplurals=1; plural=0;",
    "id": "nplurals=1; plural=0;", "tr": "nplurals=2; plural=(n != 1);",
    "ru": "nplurals=3; plural=(n%10==1 && n%100!=11 ? 0 : n%10>=2 && n%10<=4 && (n%100<10 || n%100>=20) ? 1 : 2);",
    "uk": "nplurals=3; plural=(n%10==1 && n%100!=11 ? 0 : n%10>=2 && n%10<=4 && (n%100<10 || n%100>=20) ? 1 : 2);",
    "pl": "nplurals=3; plural=(n==1 ? 0 : n%10>=2 && n%10<=4 && (n%100<10 || n%100>=20) ? 1 : 2);",
    "cs": "nplurals=3; plural=(n==1) ? 0 : (n>=2 && n<=4) ? 1 : 2;",
    "ar": "nplurals=6; plural=(n==0 ? 0 : n==1 ? 1 : n==2 ? 2 : n%100>=3 && n%100<=10 ? 3 : n%100>=11 ? 4 : 5);",
}


def rel(path: Path) -> str:
    return path.relative_to(RUST).as_posix()


def catalogs() -> List[Path]:
    return sorted(LOCALE_DIR.glob("*.po"))


def header(lang: str = "") -> str:
    plural = PLURALS.get(lang) or PLURALS.get(lang.split("_")[0]) or PLURALS[""]
    lines = [
        "Project-Id-Version: Telescope",
        f"Language: {lang}" if lang else "Language: ",
        "MIME-Version: 1.0",
        "Content-Type: text/plain; charset=UTF-8",
        "Content-Transfer-Encoding: 8bit",
        f"Plural-Forms: {plural}",
    ]
    return "".join(line + "\n" for line in lines)


def merge(po: Path, template, forget: bool = False) -> None:
    """Brings one language up to date: new texts come in empty, gone ones with a translation are kept at the end as #~."""
    old, head = read_po(po, obsolete=True)
    by_key = {e.key: e for e in old}
    count = plural_count(head)
    merged = []
    for t in template:
        e = Entry(t.msgid, t.plural, t.ctx)
        e.comments, e.locations = list(t.comments), list(t.locations)
        e.flags = list(t.flags)
        prior = by_key.pop(t.key, None)
        if prior is not None:
            e.msgstr = list(prior.msgstr)
            if prior.fuzzy:
                e.flags.append("fuzzy")
        if e.plural:
            e.msgstr = (e.msgstr + [""] * count)[:count]
        merged.append(e)
    for gone in by_key.values():
        if any(gone.msgstr) and not forget:
            gone.obsolete = True
            gone.comments, gone.locations = [], []
            merged.append(gone)
    write_po(po, head, merged)


def extract(args) -> int:
    previous = read_po(TEMPLATE)[0] if TEMPLATE.is_file() else []
    entries, problems = find_all(previous)
    for p in problems:
        print(p, file=sys.stderr)
    if problems:
        return 1
    LOCALE_DIR.mkdir(exist_ok=True)
    write_po(TEMPLATE, header(), entries, ["Telescope desktop app texts. Made by tools/i18n.py extract: translate the msgstr "
                                           "lines, keep everything", "else as it is. See docs/translating.md."])
    for po in catalogs():
        merge(po, entries, args.forget)
    print(f"{len(entries)} texts in {rel(TEMPLATE)}; updated {len(catalogs())} languages")
    return 0


def new(args) -> int:
    po = LOCALE_DIR / f"{args.lang}.po"
    if po.exists():
        print(f"{rel(po)} already exists", file=sys.stderr)
        return 1
    if not TEMPLATE.is_file():
        print(f"No {rel(TEMPLATE)} yet: run extract first", file=sys.stderr)
        return 1
    write_po(po, header(args.lang), [], [f"Telescope desktop app texts in {args.lang}. See docs/translating.md."])
    merge(po, read_po(TEMPLATE)[0])
    if args.lang not in PLURALS and args.lang.split("_")[0] not in PLURALS:
        print(f"Check the Plural-Forms line in {po.name}: it has English's rule")
    print(f"Wrote {rel(po)}")
    return 0


def problems_in(entry) -> list:
    """Translations that would show the English instead: {fields} that don't match."""
    want = fields(entry.msgid) | fields(entry.plural)
    bad = []
    for i, text in enumerate(entry.msgstr):
        if text and not fields(text) <= want:
            bad.append(f"{entry.msgid!r} form {i}: {sorted(fields(text) - want)} isn't in the English")
    return bad


def status(args) -> int:
    broken = 0
    for po in catalogs():
        if args.lang and po.stem != args.lang:
            continue
        entries, _ = read_po(po)
        done = sum(1 for e in entries if all(e.msgstr) and e.msgstr and not e.fuzzy)
        fuzzy = sum(1 for e in entries if e.fuzzy)
        print(f"{po.stem}: {done}/{len(entries)} translated" + (f", {fuzzy} to check (fuzzy)" if fuzzy else ""))
        for e in entries:
            for p in problems_in(e):
                print(f"  broken: {p}")
                broken += 1
    return 1 if broken else 0


def todo(args) -> int:
    po = LOCALE_DIR / f"{args.lang}.po"
    entries, head = read_po(po)
    left = [e for e in entries if not (e.msgstr and all(e.msgstr)) or e.fuzzy]
    sys.stdout.reconfigure(encoding="utf-8")
    print(f"# {len(left)} of {len(entries)} still to translate in {po.name}\n")
    for e in left:
        for c in e.comments:
            print(f"#. {c}")
        print(f"#: {' '.join(e.locations[:3])}")
        if e.flags:
            print(f"#, {', '.join(e.flags)}")
        if e.ctx:
            print(f"msgctxt {_quote(e.ctx)}")
        print(f"msgid {_quote(e.msgid)}")
        if e.plural:
            print(f"msgid_plural {_quote(e.plural)}")
            for i in range(plural_count(head)):
                print(f'msgstr[{i}] ""')
        else:
            print('msgstr ""')
        print()
    return 0


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    for name, run, arg in (("extract", extract, None), ("new", new, "lang"), ("status", status, "?"),
                           ("todo", todo, "lang")):
        p = sub.add_parser(name)
        if arg == "lang":
            p.add_argument("lang", help="language code, e.g. sv, de, pt_BR")
        elif arg:
            p.add_argument("lang", nargs="?")
        if name == "extract":
            p.add_argument("--forget", action="store_true", help="drop gone texts instead of keeping the translated ones")
        p.set_defaults(run=run)
    args = parser.parse_args(argv)
    return args.run(args)


if __name__ == "__main__":
    try:
        sys.exit(main())
    except BrokenPipeError:  # piped into head
        sys.exit(0)
