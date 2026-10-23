"""The desktop app's translations: tr() and trn() look a text up in the chosen language's .po file.

English is written in the code and is the fallback for anything not translated yet. The catalogs live in
telescope/locale/<lang>.po and scripts/i18n.py keeps them in step with the code (docs/translating.md).

Rules for the code:
- tr("Plain text") or tr("Paired with {name}", name=name): one literal string, so the extractor finds it.
  Fill in values with {name} fields, never by gluing strings together or with an f-string
- trn("{n} phone", "{n} phones", count) for anything with a number in it
- tr(..., ctx="...") when the same English needs different words in different places
- Never at import time (module level, class bodies, default arguments): the language isn't loaded yet.
  Mark such texts with N_("...") and translate them with tr(value) where they're shown
- A "# Translators: ..." comment on the line(s) right above the call is passed on to the translators
"""

import ast
import gettext
import logging
import os
import re
from pathlib import Path
from typing import Callable, Dict, Iterable, List, Optional, Tuple

logger = logging.getLogger(__name__)

LOCALE_DIR = Path(__file__).resolve().parent / "locale"
SOURCE_LANGUAGE = "en"
# Overrides the system language, e.g. TELESCOPE_LANG=sv to try a translation
ENV_LANGUAGE = "TELESCOPE_LANG"

# (context, english) -> translated forms; one entry for tr(), one per plural form for trn()
_catalog: Dict[Tuple[str, str], List[str]] = {}
_english: Dict[str, str] = {}  # translated -> English, for english()
_plural: Callable[[int], int] = lambda n: int(n != 1)
_language = SOURCE_LANGUAGE


def N_(text: str) -> str:
    """Marks a text for translation without translating it yet, for constants built at import time."""
    return text


def _fill(text: str, fallback: str, fields: dict) -> str:
    if not fields:
        return text
    try:
        return text.format(**fields)
    except (KeyError, IndexError, ValueError):
        # A translation with a broken {field} shows the English rather than crash the window
        logger.warning("Translation of %r has bad fields, using English", fallback)
    try:
        return fallback.format(**fields)
    except (KeyError, IndexError, ValueError):
        logger.exception("Text %r doesn't take the fields %s", fallback, sorted(fields))  # an exception in a Qt slot quits
        return fallback


class Shown(str):
    """A translated text that remembers its English, so a key saved from it doesn't change with the language."""

    def __new__(cls, text: str, source: str):
        shown = super().__new__(cls, text)
        shown.english = source
        return shown


def tr(text: str, /, ctx: str = "", **fields) -> str:
    """The text in the current language, with {fields} filled in (any name but ctx)."""
    forms = _catalog.get((ctx, text))
    return Shown(_fill(forms[0] if forms and forms[0] else text, text, fields), text)


def trn(singular: str, plural: str, n: int, /, ctx: str = "", **fields) -> str:
    """The form of the text that fits n, with {n} and the other {fields} filled in."""
    fields = {"n": n, **fields}
    english = singular if n == 1 else plural
    forms = _catalog.get((ctx, singular))
    if forms:
        index = _plural(n)
        if 0 <= index < len(forms) and forms[index]:
            return _fill(forms[index], english, fields)
    return _fill(english, english, fields)


def english(text: str) -> str:
    """The English a shown text was translated from (itself if it wasn't), for keys that are saved, like which cards
    are folded: they mustn't change with the language. A text from tr() knows; any other is looked up, which can
    pick the wrong English when two texts are translated alike."""
    return getattr(text, "english", None) or _english.get(text, text)


def sentence(text: str) -> str:
    """The text with a capital first letter, for one that can start with a filled-in field like "the phone"."""
    return text[:1].upper() + text[1:]


def and_list(words: list) -> str:
    """["A", "B", "C"] as "A, B and C" in the current language."""
    words = [str(w) for w in words]
    if len(words) < 2:
        return "".join(words)
    # Translators: what goes between the items of a list but the last two, as in "Camera, Microphone and Preview"
    between = tr(", ")
    # Translators: the end of a list, as in "Camera, Microphone and Preview"
    return tr("{items} and {last}", items=between.join(words[:-1]), last=words[-1])


def language() -> str:
    """The language loaded, "en" when nothing is translated."""
    return _language


def available() -> List[str]:
    """Languages with a catalog, besides English."""
    try:
        return sorted(p.stem for p in LOCALE_DIR.glob("*.po"))
    except OSError:
        return []


def pick(wanted: Iterable[str], have: Iterable[str]) -> str:
    """The first wanted language there's a catalog for: "pt-BR" takes pt_BR, then pt. English if none."""
    have = set(have)
    for tag in wanted:
        tag = tag.replace("-", "_")
        if tag.split("_")[0] == SOURCE_LANGUAGE:
            return SOURCE_LANGUAGE
        for candidate in (tag, tag.split("_")[0]):
            if candidate in have:
                return candidate
    return SOURCE_LANGUAGE


def system_languages() -> List[str]:
    """The languages the desktop asks for, most wanted first."""
    try:
        from PyQt6.QtCore import QLocale
        return list(QLocale.system().uiLanguages())
    except ImportError:
        return []


def resolve(chosen: str = "") -> str:
    """The language to load for the one picked in Settings ("" is the system's). TELESCOPE_LANG wins over both."""
    override = os.environ.get(ENV_LANGUAGE, "").strip()
    return pick([override or chosen] if override or chosen else system_languages(), available())


def native_name(lang: str) -> str:
    """A language's name in itself, as a language picker lists it: "Svenska" for sv."""
    if lang == SOURCE_LANGUAGE:
        return "English"
    try:
        from PyQt6.QtCore import QLocale
        name = QLocale(lang).nativeLanguageName()
    except ImportError:
        name = ""
    return sentence(name) if name else lang


def load(lang: Optional[str] = None) -> str:
    """Loads a language (the system's when None) and returns the one loaded. Call before building any window."""
    global _catalog, _english, _plural, _language
    lang = pick([lang], available()) if lang else resolve()
    _catalog, _english, _plural, _language = {}, {}, (lambda n: int(n != 1)), SOURCE_LANGUAGE
    if lang == SOURCE_LANGUAGE:
        return _language
    try:
        entries, header = read_po(LOCALE_DIR / f"{lang}.po")
    except (OSError, ValueError):
        logger.exception("Couldn't read the %s translation, staying in English", lang)
        return _language
    _plural = plural_rule(header)
    _catalog = {(e.ctx, e.msgid): e.msgstr for e in entries if not e.fuzzy and any(e.msgstr)}
    for (ctx, msgid), forms in _catalog.items():
        if ctx != "search" and len(forms) == 1:
            _english.setdefault(forms[0], msgid)  # two texts translated alike: the first one's English
    _language = lang
    _load_qt(lang)
    logger.info("Language: %s (%d texts translated)", lang, len(_catalog))
    return _language


_qt_translators: list = []


def _load_qt(lang: str) -> None:
    """Qt's own texts (the buttons in standard dialogs, a text field's right-click menu) in the same language."""
    try:
        from PyQt6.QtCore import QCoreApplication, QLibraryInfo, QLocale, QTranslator
    except ImportError:
        return
    app = QCoreApplication.instance()
    if app is None:
        return
    translator = QTranslator(app)
    folder = QLibraryInfo.path(QLibraryInfo.LibraryPath.TranslationsPath)
    if translator.load(QLocale(lang), "qtbase", "_", folder):
        app.installTranslator(translator)
        _qt_translators.append(translator)


# ── .po files ────────────────────────────────────────────────────────────────

class Entry:
    """One message of a .po file."""

    def __init__(self, msgid: str = "", plural: str = "", ctx: str = ""):
        self.msgid = msgid
        self.plural = plural
        self.ctx = ctx
        self.msgstr: List[str] = []
        self.comments: List[str] = []     # "#." from the code's "# Translators:"
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


def read_po(path: Path) -> Tuple[List[Entry], str]:
    """The entries of a .po file and its header (the msgstr of the empty msgid)."""
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
        obsolete = line.startswith("#~")
        if obsolete:
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
        entry.obsolete = entry.obsolete or obsolete
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
    return [e for e in entries if e.msgid and not e.obsolete], header


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


def plural_rule(header: str) -> Callable[[int], int]:
    m = re.search(r"plural\s*=\s*(.+?);?\s*$", header_field(header, "Plural-Forms"))
    if not m:
        return lambda n: int(n != 1)
    try:
        return gettext.c2py(m.group(1))
    except (ValueError, SyntaxError):
        logger.warning("Bad Plural-Forms %r, using English's", m.group(1))
        return lambda n: int(n != 1)


# ── Finding the texts in the code ────────────────────────────────────────────

FIELD = re.compile(r"(?<!\{)\{([A-Za-z_][A-Za-z0-9_]*)(?:[:!][^{}]*)?\}(?!\})")


def fields(text: str) -> set:
    """The {fields} a text fills in."""
    return set(FIELD.findall(text))


def find_texts(source: str, filename: str) -> Tuple[List[Entry], List[str]]:
    """Every tr(), trn() and N_() text in one Python file, and what's wrong with any that can't be found."""
    tree = ast.parse(source, filename)
    lines = source.splitlines()
    found: List[Entry] = []
    problems: List[str] = []

    def literal(node) -> Optional[str]:
        return node.value if isinstance(node, ast.Constant) and isinstance(node.value, str) else None

    def translator_comment(lineno: int) -> List[str]:
        notes = []
        i = lineno - 2
        while i >= 0 and lines[i].strip().startswith("#"):
            text = lines[i].strip().lstrip("#").strip()
            if text.lower().startswith("translators:"):
                notes.insert(0, text.split(":", 1)[1].strip())
                break
            notes.insert(0, text)
            i -= 1
        else:
            return []
        return [" ".join(notes)]

    calls = [n for n in ast.walk(tree) if isinstance(n, ast.Call) and isinstance(n.func, ast.Name)]
    for node in sorted(calls, key=lambda n: (n.lineno, n.col_offset)):
        name = node.func.id
        if name not in ("tr", "trn", "N_"):
            continue
        where = f"{filename}:{node.lineno}"
        need = 2 if name == "trn" else 1
        texts = [literal(a) for a in node.args[:need]]
        if len(node.args) < need or None in texts:
            if name != "tr" or (node.args and isinstance(node.args[0], ast.JoinedStr)):
                problems.append(f"{where}: {name}() needs plain string literals (no f-strings), "
                                f"so the text can be translated")
            continue  # tr(variable) translates a text marked with N_() elsewhere
        ctx = ""
        for kw in node.keywords:
            if kw.arg == "ctx":
                ctx = literal(kw.value) or ""
        if name != "N_" and not any(kw.arg is None for kw in node.keywords):  # **fields can't be checked
            given = {kw.arg for kw in node.keywords} - {"ctx"} | ({"n"} if name == "trn" else set())
            wanted = fields(texts[0]) | (fields(texts[1]) if name == "trn" else set())
            if wanted - given or given - wanted - ({"n"} if name == "trn" else set()):
                problems.append(f"{where}: {name}() fills in {sorted(given)} but the text has {sorted(wanted)}")
        entry = Entry(texts[0], texts[1] if name == "trn" else "", ctx)
        entry.locations.append(where)
        entry.comments = translator_comment(node.lineno)
        if fields(entry.msgid) or fields(entry.plural):
            entry.flags.append("python-brace-format")
        found.append(entry)
    return found, problems


def find_all(root: Path, package: str = "telescope") -> Tuple[List[Entry], List[str]]:
    """Every text in the package, merged by (context, English), in the order they first appear."""
    merged: Dict[Tuple[str, str], Entry] = {}
    problems: List[str] = []
    for path in sorted((root / package).rglob("*.py")):
        rel = path.relative_to(root).as_posix()
        entries, bad = find_texts(path.read_text(encoding="utf-8"), rel)
        problems += bad
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
    return list(merged.values()), problems
