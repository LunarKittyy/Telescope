#!/usr/bin/env python3
"""Translation kit for agents: what's left to translate, putting translations in, and the checks no model needs to do.

  python automation/translation/kit.py todo sv [--all] [-o todo.json]   units still to translate (both apps)
  python automation/translation/kit.py apply sv done.json               write {id: text} into sv.po and values-sv
  python automation/translation/kit.py draft es on|off                  mark a language a first draft in both apps, or not
  python automation/translation/kit.py check sv [done.json] [--accept]  mechanical problems, whole language or one file;
                                                                       --accept marks what's left as looked at

Run from the repo root (or anywhere: paths are found from this file). `todo` runs `scripts/i18n.py extract` first.
"""

import argparse
import hashlib
import json
import re
import subprocess
import sys
import xml.etree.ElementTree as ET
from pathlib import Path
from xml.sax.saxutils import escape

REPO = Path(__file__).resolve().parents[2]
DESKTOP = REPO / "desktop"
RES = REPO / "android/app/src/main/res"
KIT = REPO / "automation/translation"
sys.path.insert(0, str(DESKTOP))

from telescope import i18n  # noqa: E402

# Texts that are the same in every language; a translation equal to the English is fine for these
SAME_EVERYWHERE = re.compile(r"^(Telescope|OBS|adb|USB|Wi-Fi|JPEG|MJPEG|H\.264|VA-API|PyAV|FFmpeg|Nightly|Zoom|"
                             r"[\w.-]+\.(apk|py|exe|json|log|zip)|[A-Z0-9 :./x+-]+|.*\d.*)$")


# ── Units ────────────────────────────────────────────────────────────────────

# Android's plural quantities for each gettext form, in gettext's order; "other" repeats the last form
ANDROID_QUANTITIES = {"uk": ["one", "few", "many"], "ru": ["one", "few", "many"], "pl": ["one", "few", "many"],
                      "cs": ["one", "few", "other"], "id": ["other"], "ja": ["other"], "ko": ["other"],
                      "zh_CN": ["other"], "zh_TW": ["other"], "vi": ["other"], "th": ["other"]}


def android_dir(lang: str) -> Path:
    """values-sv, values-pt-rBR; Indonesian is values-in, Android's old code for it."""
    lang = {"id": "in"}.get(lang, lang)
    return RES / f"values-{lang.replace('_', '-r')}"


def forms_count(lang: str) -> int:
    """How many plural forms a language's catalog wants."""
    po = i18n.LOCALE_DIR / f"{lang}.po"
    return i18n.plural_count(i18n.read_po(po)[1]) if po.exists() else 2


def to_android(lang: str, forms: list) -> dict:
    quantities = ANDROID_QUANTITIES.get(lang, ANDROID_QUANTITIES.get(lang.split("_")[0], ["one", "other"]))
    plural = {q: forms[min(i, len(forms) - 1)] for i, q in enumerate(quantities)}
    plural.setdefault("other", forms[-1])
    return plural


def from_android(lang: str, plural: dict) -> list:
    quantities = ANDROID_QUANTITIES.get(lang, ANDROID_QUANTITIES.get(lang.split("_")[0], ["one", "other"]))
    return [plural.get(q, plural.get("other", "")) for q in quantities]


def desktop_id(ctx: str, msgid: str) -> str:
    return "d:" + hashlib.sha1(f"{ctx}\x04{msgid}".encode()).hexdigest()[:10]


def source_line(where: str) -> str:
    """The code line a short text is on, so a model can tell a button from a heading without opening the repo."""
    try:
        path, line = where.rsplit(":", 1)
        return (DESKTOP / path).read_text(encoding="utf-8").splitlines()[int(line) - 1].strip()[:200]
    except (ValueError, OSError, IndexError):
        return ""


def desktop_units(lang: str, everything: bool) -> list:
    po = i18n.LOCALE_DIR / f"{lang}.po"
    have = {e.key: e for e in i18n.read_po(po)[0]} if po.exists() else {}
    units = []
    for e in i18n.read_po(i18n.LOCALE_DIR / "telescope.pot")[0]:
        old = have.get(e.key)
        done = old is not None and all(old.msgstr) and not old.fuzzy
        if done and not everything:
            continue
        u = {"id": desktop_id(e.ctx, e.msgid), "app": "desktop", "en": e.msgid}
        if e.plural:
            u["en_plural"] = e.plural
        if e.ctx:
            u["ctx"] = e.ctx
        if e.comments:
            u["note"] = " ".join(e.comments)
        where = e.locations[0] if e.locations else ""
        u["where"] = where
        if len(e.msgid.split()) <= 4 and e.ctx != "search":
            u["code"] = source_line(where)
        if old is not None and any(old.msgstr):
            u["was"] = old.msgstr if e.plural else old.msgstr[0]  # fuzzy or redone: the old translation to start from
        units.append(u)
    return units


def _android_texts(xml: str) -> dict:
    """name -> text, or {quantity: text} for plurals; translatable ones only."""
    texts = {}
    for el in ET.fromstring(xml):
        if el.get("translatable") == "false":
            continue
        unq = lambda t: (t or "").replace("\\'", "'").replace('\\"', '"').replace("\\n", "\n")  # noqa: E731
        if el.tag == "string":
            texts[el.get("name")] = unq(el.text)
        elif el.tag == "plurals":
            texts[el.get("name")] = {i.get("quantity"): unq(i.text) for i in el}
    return texts


def _android_notes() -> dict:
    """name -> the comment right above it in values/strings.xml."""
    notes, pending = {}, ""
    parser = ET.XMLParser(target=ET.TreeBuilder(insert_comments=True))
    for el in ET.parse(RES / "values/strings.xml", parser).getroot():
        if el.tag is ET.Comment:
            pending = " ".join((el.text or "").split())
        else:
            if pending:
                notes[el.get("name")] = pending
            pending = ""
    return notes


def _git(*args) -> str:
    return subprocess.run(["git", *args], cwd=REPO, capture_output=True, text=True).stdout


def android_units(lang: str, everything: bool) -> list:
    english = _android_texts((RES / "values/strings.xml").read_text(encoding="utf-8"))
    mine = android_dir(lang) / "strings.xml"
    have = _android_texts(mine.read_text(encoding="utf-8")) if mine.exists() else {}
    # English that changed since the translation was last committed needs a new look
    last = _git("log", "-1", "--format=%H", "--", str(mine.relative_to(REPO))).strip() if mine.exists() else ""
    then = _android_texts(_git("show", f"{last}:android/app/src/main/res/values/strings.xml")) if last else {}
    notes = _android_notes()
    units = []
    for name, en in english.items():
        changed = last and name in then and then[name] != en
        if name in have and not changed and not everything:
            continue
        u = {"id": f"a:{name}", "app": "phone"}
        if isinstance(en, dict):
            u["en"], u["en_plural"] = en.get("one", ""), en.get("other", "")
        else:
            u["en"] = en
        if name in notes:
            u["note"] = notes[name]
        if name in have:
            u["was"] = from_android(lang, have[name]) if isinstance(have[name], dict) else have[name]
        units.append(u)
    return units


def todo(args) -> int:
    subprocess.run([sys.executable, "scripts/i18n.py", "extract"], cwd=DESKTOP, check=True, capture_output=True)
    units = android_units(args.lang, args.all) + desktop_units(args.lang, args.all)
    text = json.dumps(units, ensure_ascii=False, indent=1)
    if args.out:
        Path(args.out).write_text(text + "\n", encoding="utf-8")
        print(f"{len(units)} units -> {args.out}")
    else:
        print(text)
    return 0


# ── Writing translations in ──────────────────────────────────────────────────

def _android_escape(text: str) -> str:
    return escape(text).replace("\\", "\\\\").replace("'", "\\'").replace('"', '\\"').replace("\n", "\\n")


def apply(args) -> int:
    done = json.loads(Path(args.file).read_text(encoding="utf-8"))
    used = set()

    po = i18n.LOCALE_DIR / f"{args.lang}.po"
    entries, header = i18n.read_po(po)
    for e in entries:
        text = done.get(desktop_id(e.ctx, e.msgid))
        if text is None or e.obsolete:
            continue
        used.add(desktop_id(e.ctx, e.msgid))
        e.msgstr = list(text) if isinstance(text, list) else [text]
        e.flags = [f for f in e.flags if f != "fuzzy"]
    i18n.write_po(po, header, entries)

    english = _android_texts((RES / "values/strings.xml").read_text(encoding="utf-8"))
    mine = android_dir(args.lang) / "strings.xml"
    have = _android_texts(mine.read_text(encoding="utf-8")) if mine.exists() else {}
    for name in english:
        if f"a:{name}" in done:
            used.add(f"a:{name}")
            text = done[f"a:{name}"]
            have[name] = to_android(args.lang, text) if isinstance(text, list) else text
    lines = ['<?xml version="1.0" encoding="utf-8"?>', "<resources>"]
    for name in english:  # the English file's order; texts gone from it are dropped
        text = have.get(name)
        if isinstance(text, dict):
            lines.append(f'    <plurals name="{name}">')
            lines += [f'        <item quantity="{q}">{_android_escape(t)}</item>' for q, t in text.items()]
            lines.append("    </plurals>")
        elif text is not None:
            lines.append(f'    <string name="{name}">{_android_escape(text)}</string>')
    lines.append("</resources>")
    mine.parent.mkdir(exist_ok=True)
    mine.write_text("\n".join(lines) + "\n", encoding="utf-8")

    unknown = sorted(set(done) - used)
    print(f"wrote {len(used)} texts into {po.relative_to(REPO)} and {mine.relative_to(REPO)}")
    if unknown:
        print(f"{len(unknown)} ids matched nothing (stale todo?): {', '.join(unknown[:10])}", file=sys.stderr)
        return 1
    return 0


# ── Checks ───────────────────────────────────────────────────────────────────

FIELD = re.compile(r"\{\w+\}|%(\d+\$)?[sdf]|%%")
TAG = re.compile(r"</?\w+[^>]*>")
END = re.compile(r"(\.\.\.|…|[.:?!])$")


def _avoid(lang: str) -> list:
    """Words the glossary bans, from its "Avoid:" line."""
    try:
        text = (KIT / f"glossary/{lang}.md").read_text(encoding="utf-8")
    except OSError:
        return []
    m = re.search(r"^Avoid:(.*)$", text, re.M)
    return [w.strip() for w in m.group(1).split(",") if w.strip()] if m else []


def problems(u: dict, text, avoid: list, count: int = 2) -> list:
    forms = text if isinstance(text, list) else [text]
    english = [u["en"]] + ([u["en_plural"]] if u.get("en_plural") else [])
    found = []
    if u.get("en_plural") and len(forms) != count:
        found.append(f"has {len(forms)} plural forms, the language needs {count}")
    if not u.get("en_plural") and isinstance(text, list):
        found.append("a list for a text that isn't a plural")
    for t in forms:
        if not isinstance(t, str) or not t.strip():
            return ["empty"]
        en = english[-1] if len(english) > 1 and t is not forms[0] else english[0]
        want = {m.group() for m in FIELD.finditer(" ".join(english))}
        got = {m.group() for m in FIELD.finditer(t)}
        if got - want:
            found.append(f"has fields the English doesn't: {sorted(got - want)} (the app would show the English)")
        if want - got and t is forms[-1]:
            found.append(f"drops {sorted(want - got)}")
        if sorted(TAG.findall(t)) != sorted(TAG.findall(en)):
            found.append("markup differs")
        if t != t.strip() and en == en.strip():
            found.append("leading/trailing space")
        if t.count("\n") != en.count("\n"):
            found.append("line breaks differ")
        e_end, t_end = END.search(en), END.search(t)
        if bool(e_end) != bool(t_end) or (e_end and t_end and e_end.group() != t_end.group()
                                           and {e_end.group(), t_end.group()} != {"...", "…"}):
            found.append(f"ends differently ({e_end.group() if e_end else 'nothing'} vs {t_end.group() if t_end else 'nothing'})")
        units_only = len(re.sub(r"[^A-Za-z]", "", FIELD.sub("", en))) <= 3  # "{n} m", "{n} EV", " °C"
        if t == en and not units_only and not SAME_EVERYWHERE.match(en) and u.get("ctx") != "search":
            found.append("same as the English")
        words = len(en.split())
        if words <= 3 and len(t) > max(len(en) * 1.6, len(en) + 8) and u.get("ctx") != "search":
            found.append(f"much longer than the English ({len(t)} vs {len(en)} characters): a button or label may clip")
        if any(d in t and d not in en for d in "—–"):
            found.append("em/en dash (the project uses hyphens)")
        for word in avoid if u.get("ctx") != "search" else ():  # search words list what people might type
            if re.search(rf"\b{re.escape(word)}\b", t, re.I):
                found.append(f"uses '{word}', which the glossary avoids")
    return found


def check(args) -> int:
    units = {u["id"]: u for u in android_units(args.lang, True) + desktop_units(args.lang, True)}
    if args.file:
        texts = json.loads(Path(args.file).read_text(encoding="utf-8"))
    else:  # the whole language as it is in the repo
        texts = {}
        for e in i18n.read_po(i18n.LOCALE_DIR / f"{args.lang}.po")[0]:
            if any(e.msgstr) and not e.obsolete:
                texts[desktop_id(e.ctx, e.msgid)] = e.msgstr if e.plural else e.msgstr[0]
        mine = android_dir(args.lang) / "strings.xml"
        for name, t in (_android_texts(mine.read_text(encoding="utf-8")) if mine.exists() else {}).items():
            texts[f"a:{name}"] = from_android(args.lang, t) if isinstance(t, dict) else t
    avoid = _avoid(args.lang)
    count = forms_count(args.lang)
    accepted_file = KIT / f"glossary/{args.lang}.accepted.json"
    try:
        accepted = json.loads(accepted_file.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        accepted = {}
    bad, found = 0, {}
    for uid, text in texts.items():
        u = units.get(uid)
        if u is None:
            print(f"{uid}: no such text (stale id)")
            bad += 1
            continue
        for p in problems(u, text, avoid, count):
            key = f"{uid} {p.split(' (')[0]}"
            found[key] = text
            if accepted.get(key) == text:  # looked at before and fine, as long as the translation hasn't changed
                continue
            print(f"{uid} {u['en'][:60]!r} -> {str(text)[:60]!r}: {p}")
            bad += 1
    # The same English (no context) translated two ways reads as two different things
    seen: dict = {}
    for uid, text in texts.items():
        u = units.get(uid)
        if u and not u.get("ctx") and isinstance(text, str):
            seen.setdefault(u["en"], set()).add(text)
    for en, ts in seen.items():
        if len(ts) > 1:
            print(f"inconsistent: {en[:60]!r} is {sorted(ts)}")
            bad += 1
    if args.accept:
        accepted.update(found)
        accepted_file.write_text(json.dumps(accepted, ensure_ascii=False, indent=1) + "\n", encoding="utf-8")
        print(f"accepted {len(found)} findings into {accepted_file.relative_to(REPO)}", file=sys.stderr)
        return 0
    print(f"{bad} problems in {len(texts)} texts", file=sys.stderr)
    return 1 if bad else 0


def draft(args) -> int:
    """The .po header's X-Telescope-Draft and Android's translation_is_draft bool, which both apps show."""
    po = i18n.LOCALE_DIR / f"{args.lang}.po"
    entries, header = i18n.read_po(po)
    lines = [line for line in header.splitlines() if not line.lower().startswith(i18n.DRAFT_FIELD.lower() + ":")]
    if args.state == "on":
        lines.append(f"{i18n.DRAFT_FIELD}: yes")
    i18n.write_po(po, "".join(line + "\n" for line in lines), entries)
    bools = android_dir(args.lang) / "translation.xml"
    if args.state == "on":
        bools.parent.mkdir(exist_ok=True)
        bools.write_text('<?xml version="1.0" encoding="utf-8"?>\n<resources>\n'
                         '    <bool name="translation_is_draft">true</bool>\n</resources>\n', encoding="utf-8")
    elif bools.exists():
        bools.unlink()
    print(f"{args.lang}: draft {args.state}")
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("todo")
    p.add_argument("lang")
    p.add_argument("--all", action="store_true", help="every text, for a new language or a full review")
    p.add_argument("-o", "--out")
    p.set_defaults(fn=todo)
    p = sub.add_parser("apply")
    p.add_argument("lang")
    p.add_argument("file")
    p.set_defaults(fn=apply)
    p = sub.add_parser("draft")
    p.add_argument("lang")
    p.add_argument("state", choices=["on", "off"])
    p.set_defaults(fn=draft)
    p = sub.add_parser("check")
    p.add_argument("lang")
    p.add_argument("file", nargs="?")
    p.add_argument("--accept", action="store_true", help="remember today's findings as fine (after a person or model looked)")
    p.set_defaults(fn=check)
    args = ap.parse_args()
    return args.fn(args)


if __name__ == "__main__":
    sys.exit(main())
