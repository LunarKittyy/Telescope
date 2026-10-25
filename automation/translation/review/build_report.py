#!/usr/bin/env python3
# Builds the reviewer's pack: index.html with every screenshot and the texts it shows, then the texts no picture shows
# python build_report.py LANG LANG_NAME DESKTOP_DIR PHONE_DIR OUT
import html
import json
import re
import shutil
import sys
import xml.etree.ElementTree as ET
from pathlib import Path

REPO = Path(__file__).resolve().parents[3]
LANG, NAME, DDIR, PDIR, OUT = sys.argv[1], sys.argv[2], Path(sys.argv[3]), Path(sys.argv[4]), Path(sys.argv[5])
ANDROID_LANG = {"pt_BR": "pt-rBR", "id": "in"}.get(LANG, LANG)
shutil.rmtree(OUT, ignore_errors=True)
(OUT / "img").mkdir(parents=True)
e = html.escape

# Plain-words names for the code files, so "where" means something to a translator
AREAS = {
    "app.py": "Main window", "plugins/connection.py": "Connection card and phone pairing",
    "plugins/setup.py": "Getting started card and virtual camera setup", "plugins/shortcuts.py": "Keyboard shortcuts",
    "shortcuts.py": "Keyboard shortcuts (actions)", "plugins/camera_control.py": "Camera card (exposure, focus, white balance)",
    "widgets/plugins_panel.py": "Settings > Plugins", "plugins/microphone.py": "Microphone card",
    "plugins/stream_output.py": "Output card (resolution, frame rate, Light/Heavy)", "plugins/updates.py": "Updates",
    "updates.py": "Updates (download and install)", "plugins/browser_camera.py": "Browser camera",
    "plugins/monitoring.py": "Phone card (battery and heat warnings)", "plugins/transforms.py": "Transform card (flip, rotate, zoom)",
    "plugins/framing.py": "Auto framing card", "plugins/startup.py": "Starting with the computer",
    "widgets/vaapi_setup.py": "Set up VA-API dialog", "plugin_loader.py": "Loading plugins people add",
    "plugins/onboarding.py": "First-start tips", "plugins/preview.py": "Preview of the stream",
    "sources.py": "Phone connection messages", "platform/linux.py": "Linux virtual camera and system messages",
    "plugins/presets.py": "Presets", "platform/adb_download.py": "Downloading the USB tools (ADB)",
    "plugins/background.py": "Background card (blur, picture)", "platform/windows.py": "Windows virtual camera messages",
    "plugins/wait_screen.py": "Picture shown while the stream waits", "platform/virtual_mic.py": "Virtual microphone",
    "pyav_vaapi.py": "Hardware video decoding", "single_instance.py": "Telescope already running",
    "audio.py": "Microphone sound", "platform/hotkeys_portal.py": "Global shortcuts on Linux",
    "plugin.py": "Settings tab names", "stream.py": "Stream status", "phones.py": "Phone names",
    "widgets/lens_panel.py": "Lens picker", "stream_flow.py": "Starting and stopping streams", "widgets/adb.py": "USB tools download",
    "widgets/common.py": "Shared controls", "widgets/settings_window.py": "Settings window",
    "platform/hotkeys_windows.py": "Global shortcuts on Windows", "widgets/vision_status.py": "Person detection status",
    "i18n.py": "Lists and numbers", "platform/__init__.py": "System messages", "vision_pack.py": "Person detection download",
    "widgets/tiles.py": "Stream tiles", "browser_server.py": "Browser camera page", "platform/autostart.py": "Start with the computer",
    "splash.py": "Splash screen while starting", "widgets/collapse.py": "Folding cards", "widgets/controls.py": "Shared controls",
    "vcam.py": "Virtual camera", "widgets/banner.py": "Message bars", "widgets/level_meter.py": "Sound level meter",
    "widgets/settings_search.py": "Settings search",
}


def area(loc: str) -> str:
    path = loc.split(":")[0].removeprefix("telescope/")
    return AREAS.get(path, path)


def copy_img(src: Path) -> str:
    shutil.copy(src, OUT / "img" / src.name)
    return f"img/{src.name}"


# ── Desktop ──
d = json.loads((DDIR / "desktop.json").read_text())
cat = {(c["ctx"], c["msgid"]): c for c in d["catalog"]}


def desktop_row(c, tip=False):
    forms = c["msgstr"] if c["msgstr"] and any(c["msgstr"]) else ["(not translated yet)"]
    shown = "<br>".join(f"<span class=form>{i}:</span> {e(f)}" if len(forms) > 1 else e(f) for i, f in enumerate(forms))
    eng = e(c["msgid"]) + (f"<br><span class=dim>plural: {e(c['plural'])}</span>" if c["plural"] else "")
    notes = [e(n) for n in c["comments"]]
    if c["ctx"]:
        notes.append(f"context: {e(c['ctx'])}")
    if tip:
        notes.insert(0, "<b>tooltip</b> (shown when the mouse rests on it)")
    if c["fuzzy"]:
        notes.append("<b>marked fuzzy</b> (needs a check)")
    return f"<tr><td>{eng}</td><td lang={LANG}>{shown}</td><td class=note>{'<br>'.join(notes)}</td></tr>"


def table(rows):
    if not rows:
        return "<p class=dim>No catalog texts found in this picture (names, numbers and pictures only).</p>"
    return ("<table><thead><tr><th>English</th><th>" + e(NAME) + "</th><th>Notes</th></tr></thead><tbody>"
            + "".join(rows) + "</tbody></table>")


sections, toc = [], []
covered = set()
first_seen = {}
for i, s in enumerate(d["shots"]):
    keys = [tuple(k) for k in s["shown"]]
    tips = [tuple(k) for k in s["tips"]]
    new = [k for k in keys if k not in covered]
    newtips = [k for k in tips if k not in covered]
    for k in keys + tips:
        first_seen.setdefault(k, s["title"])
    covered |= set(keys) | set(tips)
    repeat = len(keys) + len(tips) - len(new) - len(newtips)
    rows = [desktop_row(cat[k]) for k in new if k in cat] + [desktop_row(cat[k], True) for k in newtips if k in cat]
    anchor = f"d{i}"
    toc.append(f"<li><a href='#{anchor}'>{e(s['title'])}</a></li>")
    more = f"<p class=dim>{repeat} more text(s) in this picture are listed under an earlier one.</p>" if repeat else ""
    note = f"<p class=dim>{e(s['note'])}</p>" if s.get("note") else ""
    sections.append(f"<section id={anchor}><h3>{e(s['title'])}</h3>{note}<a href='{copy_img(DDIR / s['file'])}'>"
                    f"<img loading=lazy src='img/{s['file']}'></a>{table(rows)}{more}</section>")
desktop_html = "".join(sections)
desktop_toc = "".join(toc)

rest = [c for k, c in cat.items() if k not in covered]
groups = {}
for c in rest:
    groups.setdefault(area(c["locations"][0]) if c["locations"] else "Other", []).append(c)
rest_html = "".join(f"<h3>{e(g)} <span class=dim>({len(cs)})</span></h3>{table([desktop_row(c) for c in cs])}"
                    for g, cs in sorted(groups.items(), key=lambda kv: -len(kv[1])))


# ── Phone ──
def android_strings(path: Path) -> dict:
    out = {}
    root = ET.parse(path).getroot()
    for el in root:
        name = el.get("name")
        if el.get("translatable") == "false":
            continue
        if el.tag == "string":
            out[name] = ["".join(el.itertext())]
        elif el.tag == "plurals":
            out[name] = [f"{it.get('quantity')}: " + "".join(it.itertext()) for it in el]
    return out


def unesc(s: str) -> str:
    return s.replace("\\'", "'").replace('\\"', '"').replace("\\n", "\n").replace("\\@", "@").replace("\\?", "?")


def comments(path: Path) -> dict:
    notes, last = {}, None
    for line in path.read_text(encoding="utf-8").splitlines():
        m = re.search(r"<!--\s*(.*?)\s*-->", line)
        if m and "<string" not in line and "<plurals" not in line:
            last = m.group(1)
            continue
        n = re.search(r'name="([^"]+)"', line)
        if n:
            if last:
                notes[n.group(1)] = last
            last = None
    return notes


res = REPO / "android/app/src/main/res"
en = android_strings(res / "values/strings.xml")
tr_ = android_strings(res / f"values-{ANDROID_LANG}/strings.xml")
notes = comments(res / "values/strings.xml")


def pattern(text: str):
    text = unesc(re.sub(r"^\w+: ", "", text))
    parts = re.split(r"%(?:\d+\$)?[sd]", text)
    return re.compile(".*?".join(re.escape(p) for p in parts), re.S)


pats = {n: [pattern(v) for v in vs] for n, vs in tr_.items()}


def phone_row(name):
    eng = "<br>".join(e(unesc(v)) for v in en.get(name, ["?"]))
    shown = "<br>".join(e(unesc(v)) for v in tr_.get(name, ["(not translated yet)"]))
    note = e(notes.get(name, ""))
    return f"<tr><td>{eng}</td><td lang={LANG}>{shown}</td><td class=note><code>{e(name)}</code><br>{note}</td></tr>"


p = json.loads((PDIR / "phone.json").read_text())
psections, ptoc, pcovered = [], [], set()
for i, s in enumerate(p["shots"]):
    found = []
    for name, ps in pats.items():
        if name not in en:
            continue
        for t in s["texts"]:
            if any((pt.fullmatch(t) or (len(pt.pattern) > 12 and pt.search(t))) for pt in ps):
                found.append(name)
                break
    new = [n for n in found if n not in pcovered]
    pcovered |= set(found)
    anchor = f"p{i}"
    ptoc.append(f"<li><a href='#{anchor}'>{e(s['title'])}</a></li>")
    more = f"<p class=dim>{len(found) - len(new)} more text(s) in this picture are listed under an earlier one.</p>" if len(found) > len(new) else ""
    tbl = table([phone_row(n) for n in new]).replace("No catalog texts", "No app texts")
    psections.append(f"<section id={anchor}><h3>{e(s['title'])}</h3><a href='{copy_img(PDIR / s['file'])}'>"
                     f"<img class=phone loading=lazy src='img/{s['file']}'></a>{tbl}{more}</section>")
prest = [n for n in en if n not in pcovered]
prest_html = table([phone_row(n) for n in prest])

total_d, total_p = len(cat), len(en)
page = f"""<!doctype html><html lang=en><head><meta charset=utf-8><meta name=viewport content="width=device-width,initial-scale=1">
<title>Telescope {e(NAME)} review</title><style>
:root{{--bg:#fff;--fg:#1b1d22;--dim:#6b7080;--line:#e2e4ea;--head:#f4f5f8;--accent:#6b5bd6}}
@media (prefers-color-scheme:dark){{:root{{--bg:#14161b;--fg:#e6e7eb;--dim:#9096a6;--line:#2a2e37;--head:#1c1f26;--accent:#a99cff}}}}
body{{background:var(--bg);color:var(--fg);font:15px/1.5 system-ui,sans-serif;margin:0 auto;max-width:1100px;padding:16px}}
h1{{margin:.2em 0}} h2{{margin-top:2.2em;border-bottom:2px solid var(--line);padding-bottom:.2em}} h3{{margin:1.6em 0 .5em}}
a{{color:var(--accent)}} img{{max-width:100%;border:1px solid var(--line);border-radius:6px;display:block}} img.phone{{max-width:360px}}
table{{border-collapse:collapse;width:100%;margin:.8em 0;font-size:14px}} th,td{{border:1px solid var(--line);padding:6px 8px;vertical-align:top;text-align:left}}
th{{background:var(--head)}} td:nth-child(1),td:nth-child(2){{width:38%}} .note{{color:var(--dim);font-size:13px}} .dim{{color:var(--dim)}}
.form{{color:var(--dim);font-size:12px}} section{{margin-bottom:2.5em}} details summary{{cursor:pointer}} code{{font-size:12px}}
@media (max-width:700px){{td:nth-child(1),td:nth-child(2){{width:auto}}}}
</style></head><body>
<h1>Telescope: {e(NAME)} translation review</h1>
<p>Telescope streams a phone's camera to a computer, where it shows up as a webcam. There are two apps: the desktop app and the phone app.
This page shows each screen in {e(NAME)} with the texts on it, so you can see where every text appears.</p>
<p><b>How to use it:</b> look at a picture, then at the table under it. Each table lists the texts that picture shows the first time they come up
(English, the current {e(NAME)}, and notes from the developer). Click a picture to open it full size.
Texts that never show up in a picture (mostly error messages) are listed at the end of each part, grouped by where they appear.</p>
<p>Please send corrections in the files themselves (<code>{e(LANG)}.po</code> for the desktop, <code>strings.xml</code> for the phone) or as a list.
Words in <code>{{curly}}</code> or <code>%1$s</code> are filled in by the app: keep them, but move them anywhere in the sentence.
Names, numbers and phone models in the pictures are made-up samples.</p>
<p class=dim>Desktop: {len(covered & set(cat))} of {total_d} texts appear in a picture. Phone: {len(pcovered)} of {total_p}.</p>
<h2>Contents</h2><details open><summary>Desktop app</summary><ol>{desktop_toc}<li><a href='#drest'>Texts not shown in any picture</a></li></ol></details>
<details open><summary>Phone app</summary><ol>{''.join(ptoc)}<li><a href='#prest'>Texts not shown in any picture</a></li></ol></details>
<h2>Desktop app</h2>{desktop_html}
<h2 id=drest>Desktop texts not shown in any picture ({len(rest)})</h2>
<p>These come up only in particular situations: errors, a phone that disconnects, Windows or Linux only, and so on. The group name says roughly where.</p>{rest_html}
<h2>Phone app</h2><p class=dim>The phone around the app (notification shade, system buttons) is in English because the test phone is.</p>{''.join(psections)}
<h2 id=prest>Phone texts not shown in any picture ({len(prest)})</h2>{prest_html}
</body></html>"""
(OUT / "index.html").write_text(page, encoding="utf-8")
shutil.copy(REPO / f"desktop/telescope/locale/{LANG}.po", OUT / f"desktop-{LANG}.po")
shutil.copy(res / f"values-{ANDROID_LANG}/strings.xml", OUT / f"phone-strings-{LANG}.xml")
print(f"desktop {len(covered & set(cat))}/{total_d}, phone {len(pcovered)}/{total_p}, {len(list((OUT / 'img').iterdir()))} images")
