#!/usr/bin/env python3
"""Install PyInstaller with a launcher (bootloader) that says to extract Telescope first when its lib folder is missing.

Opened straight from the zip, Windows extracts only TelescopeDesktop.exe into a temporary folder, without lib-<build>.
PyInstaller's own launcher then shows three Tcl/Tk errors from the splash and "Failed to load Python DLL". This one
checks for the folder before the splash or Python loads and shows telescope.splash.extract_first() instead, in the
first of Windows' display languages that Telescope has a translation for.

Usage (Windows, with MSVC, after pip install pyinstaller -c constraints.txt): python scripts/build_bootloader.py <work folder>
--patch-only patches the source in the work folder without building it, to check the patch still applies.
"""

import argparse
import os
import re
import subprocess
import sys
import tarfile
from pathlib import Path

DESKTOP = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(DESKTOP))

from telescope import i18n, splash  # noqa: E402
from update_guard import LIB_PREFIX  # noqa: E402

# The check goes before the PKG archive is read: Defender can still be scanning an exe Windows just extracted, and
# PyInstaller's launcher can't read its archive meanwhile
ANCHOR = """    /* Resolve main PKG archive - embedded or side-loaded. */
"""

CHECK = """#if defined(_WIN32)
    /* Telescope: opened from inside a zip, Windows extracts only this exe, without its lib folder */
    if (telescope_lib_folder_missing(pyi_ctx->executable_filename)) {
        telescope_say_extract_first();
        return -1;
    }
#endif

"""

FUNCTIONS = """
#if defined(_WIN32)
#include "telescope_extract_first.h"

/* No lib-<build> folder next to the exe (update_guard.LIB_PREFIX) */
static int
telescope_lib_folder_missing(const char *executable)
{
    wchar_t pattern[PYI_PATH_MAX];
    wchar_t *slash;
    WIN32_FIND_DATAW found;
    HANDLE search;
    int missing = 1;
    if (pyi_win32_utf8_to_wcs(executable, pattern, PYI_PATH_MAX) == NULL) {
        return 0;
    }
    slash = wcsrchr(pattern, L'\\\\');
    if (slash == NULL || (size_t)(slash - pattern) + 1 + wcslen(TELESCOPE_LIB_PATTERN) >= PYI_PATH_MAX) {
        return 0;
    }
    wcscpy(slash + 1, TELESCOPE_LIB_PATTERN);
    search = FindFirstFileExW(pattern, FindExInfoBasic, &found, FindExSearchLimitToDirectories, NULL, 0);
    if (search == INVALID_HANDLE_VALUE) {
        /* Only a search that found nothing: anything else (a path too long, no right to list) lets it start as before */
        return GetLastError() == ERROR_FILE_NOT_FOUND;
    }
    do {
        if (found.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY) {
            missing = 0;
        }
    } while (missing && FindNextFileW(search, &found));
    FindClose(search);
    return missing;
}

/* The first of Windows' display languages with a translation: "pt-BR" takes pt-BR, then pt */
static void
telescope_say_extract_first(void)
{
    wchar_t languages[512];
    ULONG count = 0, size = sizeof(languages) / sizeof(languages[0]);
    const struct telescope_text *pick = &TELESCOPE_TEXTS[0];
    if (GetUserPreferredUILanguages(MUI_LANGUAGE_NAME, &count, languages, &size)) {
        const wchar_t *tag;
        int found = 0;
        for (tag = languages; *tag && !found; tag += wcslen(tag) + 1) {
            size_t i, primary = wcscspn(tag, L"-");
            for (i = 0; i < TELESCOPE_TEXT_COUNT && !found; i++) {
                const wchar_t *lang = TELESCOPE_TEXTS[i].lang;
                if (_wcsicmp(tag, lang) == 0 || (wcslen(lang) == primary && _wcsnicmp(tag, lang, primary) == 0)) {
                    pick = &TELESCOPE_TEXTS[i];
                    found = 1;
                }
            }
        }
    }
    MessageBoxW(NULL, pick->text, pick->title, MB_OK | MB_ICONWARNING);
}
#endif
"""

MAIN_DECL = "int\npyi_main(struct PYI_CONTEXT *pyi_ctx)\n"


def c_string(text: str) -> str:
    """A wide C string literal; everything past ASCII as \\x escapes, closed off so no hex digit runs on."""
    out = []
    for ch in text:
        if ch == "\n":
            out.append("\\n")
        elif ch in '"\\':
            out.append("\\" + ch)
        elif 32 <= ord(ch) < 127:
            out.append(ch)
        else:
            out.extend(f'\\x{unit:04x}" L"' for unit in _utf16(ch))
    return 'L"' + "".join(out) + '"'


def _utf16(ch: str) -> list:
    data = ch.encode("utf-16-le")
    return [int.from_bytes(data[i:i + 2], "little") for i in range(0, len(data), 2)]


def texts_header() -> str:
    """telescope_extract_first.h: English first (the fallback), then each translation that has the texts."""
    i18n.load(i18n.SOURCE_LANGUAGE, qt=False)
    english = splash.extract_first()
    rows = [(i18n.SOURCE_LANGUAGE, english)]
    for lang in i18n.available():
        if i18n.load(lang, qt=False) == lang and splash.extract_first() != english:
            rows.append((lang, splash.extract_first()))
    i18n.load(i18n.SOURCE_LANGUAGE, qt=False)
    rows = [f"    {{{c_string(lang.replace('_', '-'))}, {c_string(title)}, {c_string(text)}}}," for lang, (title, text) in rows]
    return ("/* Written by Telescope's scripts/build_bootloader.py */\n"
            f"#define TELESCOPE_LIB_PATTERN {c_string(LIB_PREFIX + '*')}\n"
            "struct telescope_text { const wchar_t *lang, *title, *text; };\n"
            "static const struct telescope_text TELESCOPE_TEXTS[] = {\n" + "\n".join(rows) + "\n};\n"
            "#define TELESCOPE_TEXT_COUNT (sizeof(TELESCOPE_TEXTS) / sizeof(TELESCOPE_TEXTS[0]))\n")


def pinned_version() -> str:
    found = re.search(r"^pyinstaller==(\S+)$", (DESKTOP / "constraints.txt").read_text(), re.M)
    if not found:
        raise SystemExit("constraints.txt doesn't pin pyinstaller")
    return found.group(1)


def fetch_source(work: Path, version: str) -> Path:
    source = work / f"pyinstaller-{version}"
    if not source.is_dir():
        subprocess.run([sys.executable, "-m", "pip", "download", "--no-deps", "--no-binary", ":all:",
                        "--dest", str(work), f"pyinstaller=={version}"], check=True)
        with tarfile.open(work / f"pyinstaller-{version}.tar.gz") as archive:
            archive.extractall(work, filter="data")
    return source


def patch(source: Path):
    src = source / "bootloader" / "src"
    main = src / "pyi_main.c"
    code = main.read_text(encoding="utf-8")
    if "telescope_lib_folder_missing" not in code:
        if code.count(ANCHOR) != 1 or code.count(MAIN_DECL) != 1:
            raise SystemExit(f"{main} changed: the extract-first check needs a new place")
        code = code.replace(ANCHOR, CHECK + ANCHOR).replace(MAIN_DECL, FUNCTIONS + "\n" + MAIN_DECL)
        main.write_text(code, encoding="utf-8")
    (src / "telescope_extract_first.h").write_text(texts_header(), encoding="utf-8")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("work", type=Path)
    parser.add_argument("--patch-only", action="store_true")
    args = parser.parse_args()
    args.work.mkdir(parents=True, exist_ok=True)
    source = fetch_source(args.work, pinned_version())
    patch(source)
    if args.patch_only:
        print(f"Patched {source}")
        return
    # Over the constraints.txt install, which brought the pinned dependencies; the env var builds the launcher from source
    env = dict(os.environ, PYINSTALLER_COMPILE_BOOTLOADER="1")
    subprocess.run([sys.executable, "-m", "pip", "install", "--no-deps", "--force-reinstall", str(source)],
                   check=True, env=env)


if __name__ == "__main__":
    main()
