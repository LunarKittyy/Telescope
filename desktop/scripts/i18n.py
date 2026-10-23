#!/usr/bin/env python3
"""Keeps the desktop app's translations in step with the code (docs/translating.md).

  python scripts/i18n.py extract          find every tr()/trn() text, write telescope/locale/telescope.pot
                                          and bring each <lang>.po up to date with it
  python scripts/i18n.py new sv           start a Swedish catalog
  python scripts/i18n.py status [sv]      how much of each language is done, and any broken {fields}
  python scripts/i18n.py todo sv          the Swedish texts still missing, ready to paste into a .po
"""

import argparse
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT))

from telescope import i18n  # noqa: E402

TEMPLATE = i18n.LOCALE_DIR / "telescope.pot"
HEADER_COMMENTS = [
    "Telescope desktop app texts. Made by scripts/i18n.py extract: translate the msgstr lines, keep everything",
    "else as it is. See docs/translating.md.",
]

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


def catalogs():
    return sorted(i18n.LOCALE_DIR.glob("*.po"))


def merge(po: Path, template) -> None:
    """Brings one language up to date: new texts come in empty, gone ones are kept at the end as #~ for reuse."""
    old, head = i18n.read_po(po)
    by_key = {e.key: e for e in old}
    count = i18n.plural_count(head)
    merged = []
    for t in template:
        e = i18n.Entry(t.msgid, t.plural, t.ctx)
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
        if any(gone.msgstr):
            gone.obsolete = True
            gone.comments, gone.locations = [], []
            merged.append(gone)
    i18n.write_po(po, head, merged)


def extract(_args) -> int:
    entries, problems = i18n.find_all(ROOT)
    for p in problems:
        print(p, file=sys.stderr)
    if problems:
        return 1
    i18n.LOCALE_DIR.mkdir(exist_ok=True)
    i18n.write_po(TEMPLATE, header(), entries, HEADER_COMMENTS)
    for po in catalogs():
        merge(po, entries)
    print(f"{len(entries)} texts in {TEMPLATE.relative_to(ROOT)}; updated {len(catalogs())} languages")
    return 0


def new(args) -> int:
    po = i18n.LOCALE_DIR / f"{args.lang}.po"
    if po.exists():
        print(f"{po.relative_to(ROOT)} already exists", file=sys.stderr)
        return 1
    i18n.write_po(po, header(args.lang), [], [f"Telescope desktop app in {args.lang}. See docs/translating.md."])
    merge(po, i18n.read_po(TEMPLATE)[0])
    if args.lang not in PLURALS and args.lang.split("_")[0] not in PLURALS:
        print(f"Check the Plural-Forms line in {po.name}: it has English's rule")
    print(f"Wrote {po.relative_to(ROOT)}")
    return 0


def problems_in(entry) -> list:
    """Translations that would show the English instead: {fields} that don't match."""
    want = i18n.fields(entry.msgid) | i18n.fields(entry.plural)
    bad = []
    for i, text in enumerate(entry.msgstr):
        if text and not i18n.fields(text) <= want:
            bad.append(f"{entry.msgid!r} form {i}: {sorted(i18n.fields(text) - want)} isn't in the English")
    return bad


def status(args) -> int:
    broken = 0
    for po in catalogs():
        if args.lang and po.stem != args.lang:
            continue
        entries, _ = i18n.read_po(po)
        done = sum(1 for e in entries if all(e.msgstr) and e.msgstr and not e.fuzzy)
        fuzzy = sum(1 for e in entries if e.fuzzy)
        print(f"{po.stem}: {done}/{len(entries)} translated" + (f", {fuzzy} to check (fuzzy)" if fuzzy else ""))
        for e in entries:
            for p in problems_in(e):
                print(f"  broken: {p}")
                broken += 1
    return 1 if broken else 0


def todo(args) -> int:
    po = i18n.LOCALE_DIR / f"{args.lang}.po"
    entries, head = i18n.read_po(po)
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
            print(f"msgctxt {i18n._quote(e.ctx)}")
        print(f"msgid {i18n._quote(e.msgid)}")
        if e.plural:
            print(f"msgid_plural {i18n._quote(e.plural)}")
            for i in range(i18n.plural_count(head)):
                print(f'msgstr[{i}] ""')
        else:
            print('msgstr ""')
        print()
    return 0


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("extract").set_defaults(run=extract)
    p = sub.add_parser("new")
    p.add_argument("lang", help="language code, e.g. sv, de, pt_BR")
    p.set_defaults(run=new)
    p = sub.add_parser("status")
    p.add_argument("lang", nargs="?")
    p.set_defaults(run=status)
    p = sub.add_parser("todo")
    p.add_argument("lang")
    p.set_defaults(run=todo)
    args = parser.parse_args(argv)
    return args.run(args)


if __name__ == "__main__":
    try:
        sys.exit(main())
    except BrokenPipeError:  # piped into head
        sys.exit(0)
