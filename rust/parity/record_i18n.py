"""Records what the Python app's language loading (telescope/i18n.py) makes of the real catalogs, for
rust/crates/core/fixtures/i18n.json (rust/crates/core/tests/i18n.rs replays it): each language's name, number marks, plural rule,
what it translated and a checksum of all of it, which language gets picked for a few system settings, and the pseudo language.

    python3 rust/parity/record_i18n.py            # rewrites the fixture
"""

import json
import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
OUT = ROOT / "rust" / "crates" / "core" / "fixtures" / "i18n.json"

sys.path.insert(0, str(ROOT / "desktop"))
from telescope import i18n  # noqa: E402

FORMS_UP_TO = 300

# (environment, the language picked in Settings)
RESOLVE_CASES = [
    ({"LANG": "sv_SE.UTF-8"}, ""),
    ({"LANGUAGE": "de:sv", "LANG": "en_US.UTF-8"}, ""),
    ({"LANG": "C.UTF-8"}, ""),
    ({"LANG": "C"}, ""),
    ({"LC_ALL": "pt_BR.UTF-8", "LANG": "sv_SE"}, ""),
    ({"LANGUAGE": "C:sv", "LANG": "de_DE"}, ""),
    ({"LANG": "zh_CN.UTF-8"}, ""),
    ({"LANG": "zh"}, ""),
    ({"LANG": "pt"}, ""),
    ({"LANG": "pt_PT.UTF-8"}, ""),
    ({"LANG": "zh_TW.UTF-8"}, ""),
    ({"LANG": "en_GB.UTF-8", "LANGUAGE": ""}, ""),
    ({"LANGUAGE": "nb", "LANG": "sv_SE"}, ""),
    ({"LANG": "sr_RS@latin"}, ""),
    ({"LANG": "de_DE.UTF-8", "LC_MESSAGES": "sv_SE.UTF-8"}, ""),
    ({"LANGUAGE": "sv", "LANG": "C"}, ""),
    ({"LANGUAGE": "en:sv"}, ""),
    ({}, ""),
    ({"LANG": "de_DE.UTF-8"}, "sv"),
    ({"LANG": "de_DE.UTF-8"}, "en"),
    ({"LANG": "sv_SE.UTF-8"}, "de"),
    ({"LANG": "sv_SE.UTF-8"}, "pt_BR"),
    ({"LANG": "sv_SE.UTF-8"}, "pt-BR"),
    ({"LANG": "sv_SE.UTF-8"}, "xx"),
    ({"LANG": "sv_SE.UTF-8"}, "pseudo"),
    ({"LANG": "de_DE.UTF-8", "TELESCOPE_LANG": "sv"}, ""),
    ({"LANG": "de_DE.UTF-8", "TELESCOPE_LANG": "en"}, "sv"),
    ({"LANG": "de_DE.UTF-8", "TELESCOPE_LANG": " pseudo "}, "sv"),
]

PICK_CASES = [
    (["sv-SE", "en-US"], ["pt_BR", "sv"]),
    (["pt-BR"], ["pt_BR", "sv"]),
    (["de-DE", "en-GB", "sv"], ["pt_BR", "sv"]),
    (["fr"], ["pt_BR", "sv"]),
    (["zh-Hans-CN", "zh-CN"], ["zh_CN"]),
    (["pseudo", "sv"], ["sv"]),
    ([], ["sv"]),
]

PSEUDO_TEXTS = [
    "", "Start streaming", "x", "Made by <a href=\"{url}\">LunarKittyy</a>\nsecond line", "a\n\nb", "{n} phones &amp; more &#39;", "12345 6",
    "Tab\there", "ÅÄÖ and åäö", "Hello, world.", "abcde", "abcdefghij", "abcdefghijklmnopqrstuvwxy",
]


def fnv(text: str) -> str:
    h = 0xCBF29CE484222325
    for byte in text.encode("utf-8"):
        h = ((h ^ byte) * 0x100000001B3) & 0xFFFFFFFFFFFFFFFF
    return f"{h:016x}"


def catalog_digest(catalog: dict) -> str:
    return fnv("".join("\x1f".join([ctx, msgid, *forms]) + "\x1e" for (ctx, msgid), forms in catalog.items()))


def marks(lang: str) -> list:
    from PyQt6.QtCore import QLocale
    loc = QLocale(lang)
    return [loc.decimalPoint(), loc.negativeSign(), loc.positiveSign()]


def language(lang: str) -> dict:
    entries, header = i18n.read_po(i18n.LOCALE_DIR / f"{lang}.po")
    catalog, rule = i18n._read_catalog(i18n.LOCALE_DIR / f"{lang}.po")
    i18n.load(lang, qt=False)
    english = list(i18n._english.items())
    keys = list(catalog)
    picks = keys[::max(1, len(keys) // 40)]
    out = {
        "name": i18n.native_name(lang),
        "draft": i18n.is_draft(lang),
        "marks": marks(lang),
        "nplurals": i18n.plural_count(header),
        "forms": [rule(n) for n in range(FORMS_UP_TO + 1)],
        "count": len(catalog),
        "checksum": catalog_digest(catalog),
        "samples": [[ctx, msgid, catalog[(ctx, msgid)]] for ctx, msgid in picks],
        "english": [[shown, msgid] for shown, msgid in english[::max(1, len(english) // 40)]],
        "numbers": [[v, d, s, i18n.number(v, d, s)] for v, d, s in
                    [(1, 1, False), (-0.5, 1, True), (10, 0, True), (45.5, 1, False), (-1234.5, 2, False), (0, 3, True)]],
        "percents": [[v, d, s, i18n.percent(v, d, s)] for v, d, s in [(80, 0, False), (-10, 0, True), (12.5, 1, False)]],
    }
    i18n.load("en", qt=False)
    return out


def resolved(env: dict, chosen: str) -> str:
    base = {k: v for k, v in os.environ.items() if k not in ("LANG", "LANGUAGE", "LC_ALL", "LC_MESSAGES", i18n.ENV_LANGUAGE)}
    code = ("import sys; sys.path.insert(0, %r)\nfrom telescope import i18n\nprint(i18n.resolve(%r))" % (str(ROOT / "desktop"), chosen))
    out = subprocess.run([sys.executable, "-c", code], env={**base, **env}, capture_output=True, text=True, check=True)
    return out.stdout.strip()


def main():
    template, _ = i18n.read_po(i18n.LOCALE_DIR / "telescope.pot")
    sample = [e.msgid for e in template[::9]] + [e.msgid for e in template if any(c in e.msgid for c in "<&\n") ][:60]
    pot_keys = [[e.ctx, e.msgid, e.plural] for e in template]
    data = {
        "available": i18n.available(),
        "template": {"count": len(template), "checksum": fnv("".join("\x1f".join(k) + "\x1e" for k in pot_keys))},
        "languages": {lang: language(lang) for lang in i18n.available()},
        "resolve": [{"env": env, "chosen": chosen, "result": resolved(env, chosen)} for env, chosen in RESOLVE_CASES],
        "pick": [{"wanted": w, "have": h, "result": i18n.pick(w, h)} for w, h in PICK_CASES],
        "pseudo": [[t, i18n.pseudo(t)] for t in PSEUDO_TEXTS + sample],
    }
    OUT.write_text(json.dumps(data, ensure_ascii=False, indent=1) + "\n", encoding="utf-8")
    print(f"wrote {OUT.relative_to(ROOT)} ({OUT.stat().st_size} bytes)")


if __name__ == "__main__":
    main()
