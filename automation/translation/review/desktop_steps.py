# Steps run inside render_desktop.py: win, app, shoot, safe, PENDING are in scope


def full(name, title, area, note=""):
    inner = area.widget()
    inner.resize(area.viewport().width(), inner.sizeHint().height())
    shoot(name, title, inner, note=note)


shoot("main", "Main window, first start (no phone yet)", win)
for i, col in enumerate(win._columns):
    full(f"main-column-{i}", f"Main window, column {i + 1} scrolled out in full", col)

conn = win._plugin("connection")
safe("add phone", lambda: (conn.open_add_phone(), shoot("add-phone", "Add phone dialog", conn._add_dlg)))
safe("phones", lambda: (conn.open_phones(), shoot("phones", "Your phones dialog", conn._phones_dlg)))
for d in ("_add_dlg", "_phones_dlg"):
    if getattr(conn, d, None):
        getattr(conn, d).hide()

presets = win._plugin("presets")
safe("presets menu", lambda: (PENDING.update(name="presets-menu", title="Presets menu"), presets.build_menu(win).exec()))

win.open_settings()
app.processEvents()
dlg = win._settings_win
dlg.resize(900, 700)
for row in range(dlg._nav.count()):
    dlg._nav.setCurrentRow(row)
    app.processEvents()
    page = dlg._stack.currentWidget()
    tab = dlg._nav.item(row).text()
    shoot(f"settings-{row}", f"Settings > tab {row + 1} ({tab})", dlg)
    if isinstance(page, QScrollArea) and page.widget().sizeHint().height() > page.viewport().height():
        full(f"settings-{row}-full", f"Settings > tab {row + 1} ({tab}), whole page", page)

setup = win._plugin("setup")
safe("vaapi", lambda: (PENDING.update(name="vaapi", title="Set up VA-API dialog"), setup._open_vaapi()))
sc = win._plugin("shortcuts")
safe("binding", lambda: (PENDING.update(name="binding-editor", title="Shortcut editor (new shortcut)"), sc.add_binding(parent=dlg)))
dlg.hide()

# ── A paired phone ──
import copy
import json as _json
cfg = conn.get_config()
cfg["phones"] = [{"id": "p1", "name": "Pixel 8", "token": "t" * 32, "ips": ["192.168.1.20"], "cert_sha256": "ab" * 32},
                 {"id": "p2", "name": "Galaxy S24", "token": "u" * 32, "ips": ["192.168.1.21"], "cert_sha256": "cd" * 32}]
cfg["selected_phone"] = "p1"
safe("paired", lambda: conn.set_config(cfg))
for _ in range(5):
    app.processEvents()
shoot("paired", "Main window, two phones paired, not streaming", win)
safe("phones2", lambda: (conn.open_phones(), shoot("phones-paired", "Your phones dialog, with phones", conn._phones_dlg), conn._phones_dlg.hide()))
combo = conn._combo if hasattr(conn, "_combo") else None
if combo is not None:
    safe("combo", lambda: (combo.showPopup(), app.processEvents(), shoot("phone-list", "Phone list at the top left, opened", combo.view().window(), scope=combo), combo.hidePopup()))

# ── Streaming, with a healthy phone ──
STATE = _json.loads((REPO / "docs/state.json").read_text())
healthy = copy.deepcopy(STATE)
for k in [k for k in healthy if k.startswith(("codec_error", "camera_error", "heat_"))] + ["codec_unsupported", "dynamic_fps"]:
    healthy.pop(k, None)
healthy.update(camera_off=False, camera_taken=False, torch=False, auto=True, wb_manual=False, focus_mode="auto")


class Ctrl:
    base = "http://127.0.0.1:9"
    auth = None

    def __getattr__(self, name):
        return lambda *a, **k: None


ctrl = Ctrl()
for p in list(win._plugins):
    safe(f"{p.name} starting", p.on_stream_starting)
    safe(f"{p.name} start", lambda p=p: p.on_stream_start("http://127.0.0.1:9/stream", ctrl))
    safe(f"{p.name} state", lambda p=p: p.on_phone_state(healthy))
win._set_start_button(True)
win._set_status(tr("Streaming {size} at {fps} fps to {camera}", size="1920 × 1080", fps=30, camera="Telescope"), "ok")
win._fps_lbl.setText("30 fps")
for _ in range(5):
    app.processEvents()
shoot("streaming", "Main window while streaming (phone camera controls filled in)", win)
for i, col in enumerate(win._columns):
    full(f"streaming-column-{i}", f"Streaming, column {i + 1} scrolled out in full", col)

# ── The same, with the phone reporting trouble ──
for p in list(win._plugins):
    safe(f"{p.name} bad state", lambda p=p: p.on_phone_state(STATE))
for _ in range(5):
    app.processEvents()
shoot("streaming-trouble", "Streaming, phone reports problems (camera taken, H.264 unavailable, hot)", win)
for i, col in enumerate(win._columns):
    full(f"trouble-column-{i}", f"Phone reports problems, column {i + 1} in full", col)

# ── Narrow window ──
win.resize(520, 860)
for _ in range(5):
    app.processEvents()
win._set_start_button(True)
shoot("narrow", "Main window made narrow (short button texts)", win)
win.resize(1280, 860)

exec(compile(Path(__file__).with_name("desktop_harvest.py").read_text(), "desktop_harvest.py", "exec"))
