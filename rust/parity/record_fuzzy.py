"""Records what the Python app's search matcher (telescope/fuzzy.py) makes of text with case folds, ligatures, marks and typos, for
rust/crates/core/fixtures/fuzzy.json (the tests in fuzzy.rs read it).

    python3 rust/parity/record_fuzzy.py            # rewrites the fixture
"""

import json
import sys
import unicodedata
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
OUT = ROOT / "rust" / "crates" / "core" / "fixtures" / "fuzzy.json"

sys.path.insert(0, str(ROOT / "desktop"))
from telescope import fuzzy  # noqa: E402

TEXTS = [
    "", "Größe", "GROSSE", "Straße", "ẞ", "Fuß", "ﬁsh", "ﬃ", "Ｆｕｌｌ ｗｉｄｔｈ", "İstanbul", "ıi", "ǅ", "Å", "Ω", "K", "ŉ", "ǰ", "ẖ",
    "e⃝x", "aัb", "aःb", "cáfé", "eͅx", "ᾈ", "ΐ", "ς Σ", "日本語 abc", "a‍b", "résumé", "naïve", "Ⅻ", "①",
    "x²", "½", "tab\tnew\nline", "ſ", "Cherokee ᏸ", "mixed Case-and_punctuation!",
]

# (name, keywords, the typed query, partial)
SEARCHES = [
    ("Größe", "size scale", q, False) for q in ["grosse", "gross", "größe", "GROSSE", "grose", "gr", "size", "xyz", "grösse", "große"]
] + [
    ("Straße einstellen", "", q, False) for q in ["strasse", "strasse einst", "str ein", "STRASSE", "straße"]
] + [
    ("Hardware acceleration", "hwaccel gpu", q, False) for q in ["ha", "hwaccel", "hardwre", "accel", "zzz"]
] + [
    ("Café Mode", "caffeine", q, False) for q in ["cafe", "café", "café", "CAFÉ", "caffe"]
] + [
    ("Fuß fit", "foot", q, True) for q in ["fuss", "fuss fit", "fuss nothing", "FUSS"]
] + [
    ("Ｗｉｄｅ view", "ﬁsh ﬂow", q, False) for q in ["wide", "fish", "flow", "ﬁsh"]
]
WEIGHTS = {"name": 1.0, "keywords": 0.9}


def main():
    out = {
        "python": sys.version.split()[0],
        "unicode": unicodedata.unidata_version,
        "weights": WEIGHTS,
        "normalize": [[t, fuzzy.normalize(t)] for t in TEXTS],
        "scores": [
            {"name": name, "keywords": keywords, "query": query, "partial": partial,
             "score": fuzzy.Doc(name=name, keywords=keywords).score(query, WEIGHTS, partial=partial)}
            for name, keywords, query, partial in SEARCHES
        ],
    }
    OUT.parent.mkdir(parents=True, exist_ok=True)
    lines = []
    for key, value in out.items():
        if isinstance(value, list):
            lines.append('  "%s": [\n%s\n  ]' % (key, ",\n".join("    " + json.dumps(v) for v in value)))
        else:
            lines.append('  "%s": %s' % (key, json.dumps(value)))
    OUT.write_text("{\n" + ",\n".join(lines) + "\n}\n")
    print(f"{len(out['normalize'])} normalized and {len(out['scores'])} scored, written to {OUT}")


if __name__ == "__main__":
    main()
