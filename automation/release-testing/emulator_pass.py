#!/usr/bin/env python3
"""Go through the parts of the release checklist that an Android emulator and the isolated Linux profile can do, and
print pass, fail or skipped for each.

Usage: emulator_pass.py downloads/n932 [--previous downloads/n838] [--only NAME ...] [--list]

Needs an emulator that's already running (adb sees exactly one device) and whose camera can stream, the Linux bundle
from the same folder installed with linux/install.sh, v4l2loopback loaded, and ffmpeg and v4l2-ctl for the frame
checks. The phone app gets uninstalled and the profile's settings start fresh. See README.md for which emulator images
behave.
"""

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
import zipfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent
LINUX = HERE / "linux"
PROFILE = Path(os.environ.get("RT_PROFILE", LINUX / ".profile"))
DRIVE = PROFILE / "drv"
CONFIG_DIR = PROFILE / "home" / ".config" / "telescope"
LOG = Path(tempfile.gettempdir()) / f"telescope-{os.environ.get('USER', '')}" / "telescope.log"
PKG = "com.telescope"
FORWARD_PORT = 48766

CHECKS = []


class Fail(Exception):
    pass


class Skip(Exception):
    pass


def check(fn):
    CHECKS.append(fn)
    return fn


def need(cond, msg):
    if not cond:
        raise Fail(msg)


def wait_for(fn, timeout, step=1.0):
    """fn's first truthy result within timeout seconds, or None."""
    end = time.monotonic() + timeout
    while True:
        got = fn()
        if got or time.monotonic() > end:
            return got
        time.sleep(step)


# ── Phone ─────────────────────────────────────────────────────────────────────

def adb(*args, check=False) -> str:
    return subprocess.run(["adb", *args], capture_output=True, text=True, check=check).stdout


def shell(*args) -> str:
    return adb("shell", *args)


def sdk() -> int:
    return int(shell("getprop", "ro.build.version.sdk").strip() or 0)


def launch(settle=3.0):
    """Bring the phone app to the front, scrolled to the top."""
    def in_front():
        return f"{PKG}/" in shell("dumpsys", "activity", "activities").split("topResumedActivity", 1)[-1][:200]

    for _ in range(4):  # just after an install the new app can get killed under us
        shell("monkey", "-p", PKG, "-c", "android.intent.category.LAUNCHER", "1")
        if wait_for(in_front, 8, 1):
            break
    time.sleep(settle)
    if settle and in_front():
        for _ in range(2):
            shell("input", "swipe", "540", "700", "540", "2000", "200")
        time.sleep(0.5)
    shell("cmd", "statusbar", "collapse")  # a swipe that missed the app opens the shade


def nodes() -> list:
    for _ in range(4):
        shell("uiautomator", "dump", "/sdcard/ui.xml")
        xml = shell("cat", "/sdcard/ui.xml")
        if "<node" in xml:
            break
        time.sleep(1)
    out = []
    for m in re.finditer(r"<node [^>]*>", xml):
        attr = dict(re.findall(r'([\w-]+)="([^"]*)"', m.group(0)))
        b = list(map(int, re.findall(r"\d+", attr.get("bounds", ""))))
        out.append(dict(text=attr.get("text", ""), desc=attr.get("content-desc", ""),
                        id=attr.get("resource-id", "").split("/")[-1], checked=attr.get("checked") == "true", b=b))
    return out


def find(text, ns=None) -> list:
    """Nodes whose text or description is text, or failing that contains it."""
    ns = ns if ns is not None else nodes()
    t = text.lower()
    exact = [n for n in ns if t in (n["text"].lower(), n["desc"].lower())]
    return exact or [n for n in ns if t in (n["text"] + " " + n["desc"]).lower()]


def tap(text, n=0, ns=None, scroll=True):
    hits = find(text, ns)
    for _ in range(3 if scroll and ns is None and len(hits) <= n else 0):
        scroll_down()
        hits = find(text)
        if len(hits) > n:
            break
    need(len(hits) > n, f"nothing on the phone's screen says {text!r}")
    b = hits[n]["b"]
    shell("input", "tap", str((b[0] + b[2]) // 2), str((b[1] + b[3]) // 2))
    time.sleep(1.5)


def node_text(res_id) -> str:
    return next((n["text"] for n in nodes() if n["id"] == res_id), "")


def scroll_down():
    shell("input", "swipe", "540", "1800", "540", "500", "300")
    time.sleep(1)


def status_detail() -> str:
    detail = node_text("tvStatusDetail")
    if not detail:
        scroll_down()
        detail = node_text("tvStatusDetail")
    return detail.replace("&#10;", "\n")


def computer_rows() -> int:
    return len([n for n in nodes() if n["desc"].startswith("Remove ") and n["desc"].endswith("?")])


def stream_notification() -> dict:
    dump = shell("dumpsys", "notification", "--noredact")
    for rec in dump.split("NotificationRecord(")[1:]:
        if f"pkg={PKG} " in rec and "channel=telescope_stream" in rec:
            flags = re.search(r"flags=(\S+)", rec)
            title = re.search(r"android.title=String \((.*?)\)", rec)
            text = re.search(r"android.text=String \((.*?)\)", rec)
            actions = re.search(r"actions=(\d+)", rec)
            return dict(flags=flags.group(1) if flags else "", title=title.group(1) if title else "",
                        text=text.group(1) if text else "", actions=int(actions.group(1)) if actions else 0)
    return {}


def toasts() -> list:
    return re.findall(r"TextToastRecord\{.*?text=(.*?) duration", shell("dumpsys", "notification"))


def ping(token) -> int:
    adb("forward", f"tcp:{FORWARD_PORT}", "tcp:8766")
    r = subprocess.run(["curl", "-sk", "-o", "/dev/null", "-w", "%{http_code}", "-H", f"Authorization: Bearer {token}",
                        f"https://localhost:{FORWARD_PORT}/v1/ping"], capture_output=True, text=True)
    return int(r.stdout or 0)


def apk_version() -> str:
    m = re.search(r"versionName=(\S+)", shell("dumpsys", "package", PKG))
    return m.group(1) if m else ""


# ── Desktop ───────────────────────────────────────────────────────────────────

PRELUDE = r'''
from PyQt6.QtWidgets import QLabel, QAbstractButton, QComboBox, QListWidget, QLineEdit, QApplication, QMessageBox
import gc

def dialogs(kind=None):
    return [w for w in wins() if w is not main_win() and (kind is None or type(w).__name__ == kind)]

def answer(text):
    for m in dialogs("QMessageBox"):
        for b in buttons(m):
            if b.text().replace("&", "") == text:
                b.click()
                return m.text()
    return None

def labels(root=None):
    """Visible label texts in root's own window (Settings is a child of the main window but a window of its own)."""
    root = root or main_win()
    return [l.text() for l in root.findChildren(QLabel) if l.isVisible() and l.window() is root.window()]

def settings_tab(name):
    main_win().open_settings()
    d = main_win()._settings_win
    for l in d.findChildren(QListWidget):
        for i in range(l.count()):
            if l.item(i).text().startswith(name):
                l.setCurrentRow(i)
    return d

def plugin_switch(title):
    d = main_win()._settings_win
    l = [l for l in d.findChildren(QLabel) if l.text() == title or l.text().startswith(title + " ")][0]
    row = l.parentWidget()
    while row and not [b for b in row.findChildren(QAbstractButton) if b.isCheckable()]:
        row = row.parentWidget()
    return [b for b in row.findChildren(QAbstractButton) if b.isCheckable()][0]

def combo(name):
    return [c for c in main_win().findChildren(QComboBox) if c.accessibleName() == name][0]
'''


def drv(code: str, wait=10.0) -> str:
    out = DRIVE / "out.txt"
    out.unlink(missing_ok=True)
    tmp = DRIVE / "in.tmp"
    tmp.write_text(code)
    tmp.rename(DRIVE / "in.py")
    got = wait_for(lambda: out.exists(), wait, 0.1)
    return out.read_text() if got else "(no answer)"


def app_running() -> bool:
    return "yes" in drv("print('yes' if main_win().isVisible() else 'no')", 2)


def app_start(app_dir=None):
    env = dict(os.environ, RT_APP=str(app_dir)) if app_dir else None
    subprocess.run([str(LINUX / "run_app.sh")], check=True, env=env)
    need(wait_for(app_running, 60, 2), "the desktop app didn't start (see linux/.profile/app.log)")
    drv(PRELUDE)


def app_stop():
    subprocess.run([str(LINUX / "stop_app.sh")])
    time.sleep(3)


def log_mark() -> int:
    return len(LOG.read_text(errors="replace").splitlines()) if LOG.exists() else 0


def log_since(mark) -> list:
    return LOG.read_text(errors="replace").splitlines()[mark:] if LOG.exists() else []


def wait_log(mark, pattern, timeout) -> str:
    rx = re.compile(pattern)
    return wait_for(lambda: next((line for line in log_since(mark) if rx.search(line)), None), timeout) or ""


def click(text, root="main_win()"):
    out = drv(f"print(click({text!r}, {root}))")
    need("clicked" in out, f"couldn't click {text!r} on the desktop: {out.strip()[-200:]}")


def start_stream(timeout=40) -> str:
    mark = log_mark()
    if "Stop streaming" in drv("print([b.text() for b in buttons(main_win(), 'streaming')])"):
        stop_stream()
        mark = log_mark()
    click("Start streaming")
    line = wait_log(mark, r"Status: Streaming", timeout)
    need(line, "the stream didn't start: " + " | ".join(log_since(mark)[-3:]))
    return line


def stop_stream():
    drv("b = [b for b in buttons(main_win(), 'streaming')]\nif b and b[0].text() == 'Stop streaming': b[0].click()")
    time.sleep(3)


def token() -> str:
    phones = json.loads((CONFIG_DIR / "telescope_config.json").read_text())["plugin_configs"]["connection"]["phones"]
    return phones[0]["token"] if phones else ""


def pair_over_usb():
    """Add phone over USB, answering yes to "Same phone?"; returns what the dialog said."""
    drv("for w in dialogs(): w.close()")
    launch()
    click("Add phone")

    def paired():
        drv("answer('Yes')")
        return drv("print(next((l for d in dialogs('AddPhoneDialog') for l in labels(d) if 'Paired with' in l), ''))").strip()
    said = wait_for(paired, 30, 2)
    drv("for d in dialogs('AddPhoneDialog'): click('Done', d)")
    need(said, "Add phone didn't pair over USB")
    return said


def vcam_device() -> str:
    names = re.findall(r"Streaming .* to (.+)$", "\n".join(log_since(0)), re.M)
    listing = subprocess.run(["v4l2-ctl", "--list-devices"], capture_output=True, text=True).stdout
    for block in listing.split("\n\n"):
        if names and block.startswith(names[-1].strip() + " ("):
            return re.search(r"(/dev/video\d+)", block).group(1)
    raise Skip("couldn't find the virtual camera's device")


def grab(dev):
    import cv2  # noqa: PLC0415 (only the frame checks need it)
    path = Path(tempfile.mkdtemp()) / "f.png"
    subprocess.run(["ffmpeg", "-loglevel", "error", "-y", "-f", "v4l2", "-i", dev, "-frames:v", "1", str(path)],
                   timeout=20)
    frame = cv2.imread(str(path))
    need(frame is not None, f"couldn't read a frame from {dev}")
    return frame.astype(int)


def colour_spread(frame) -> int:
    return int((frame.max(2) - frame.min(2)).max())


# ── Checks ────────────────────────────────────────────────────────────────────
# Each takes the run's arguments, raises Fail or Skip, or returns a short note. They run in order and later ones lean
# on what earlier ones set up (a fresh phone app, a pairing, a running desktop).

@check
def phone_fresh_install(a):
    adb("uninstall", PKG)
    adb("install", str(a.build / "Telescope.apk"), check=True)
    launch()
    ns = wait_for(lambda: (lambda ns: ns if find("Get set up", ns) or find("Allow Telescope", ns) else None)(nodes()), 40, 2)
    need(ns, "the phone app's main screen didn't come up")
    need(not find("Allow Telescope", ns), "the phone app asked for a permission on launch")
    rows = [n["text"] for n in ns if n["text"] in ("Camera", "Local network", "Notifications", "Battery")]
    want = ["Camera", "Local network", "Notifications", "Battery"] if sdk() >= 37 else ["Camera", "Notifications", "Battery"]
    need(rows == want, f"Get set up lists {rows}, expected {want}")
    return f"{apk_version()}, rows {', '.join(rows)}"


@check
def phone_deny_camera_twice(a):
    for _ in range(2):
        tap("Allow", scroll=False)
        tap("Don’t allow", scroll=False)
    need(find("Open settings"), "Camera's button didn't turn into Open settings")


def granted(permission) -> bool:
    return re.search(rf"{permission}: granted=true", shell("dumpsys", "package", PKG)) is not None


SYSTEM_ALLOW = ("permission_allow_button", "permission_allow_foreground_only_button", "button1")


@check
def phone_allow_everything(a):
    shell("pm", "grant", PKG, "android.permission.CAMERA")
    shell("am", "force-stop", PKG)
    launch()
    for _ in range(12):
        ns = nodes()
        system = [n for n in ns if n["id"] in SYSTEM_ALLOW]
        if system:  # a permission or battery dialog over the app
            b = system[0]["b"]
            shell("input", "tap", str((b[0] + b[2]) // 2), str((b[1] + b[3]) // 2))
            time.sleep(1.5)
            continue
        if not find("Get set up", ns):
            break
        need([n for n in ns if n["text"] == "Allow"], "Get set up is still there with nothing left to allow")
        tap("Allow", ns=ns)
    wanted = ["android.permission.CAMERA"]
    wanted += ["android.permission.POST_NOTIFICATIONS"] if sdk() >= 33 else []
    wanted += ["android.permission.ACCESS_LOCAL_NETWORK"] if sdk() >= 37 else []
    missing = [w.rsplit(".", 1)[-1] for w in wanted if not granted(w)]
    need(not missing, f"still not granted after Get set up: {', '.join(missing)}")
    need(PKG in shell("dumpsys", "deviceidle", "whitelist"), "the battery exemption wasn't granted")
    launch()
    need(not find("Get set up"), "Get set up didn't go away after allowing everything")


@check
def desktop_fresh_start(a):
    app_stop()
    shutil.rmtree(CONFIG_DIR, ignore_errors=True)
    app_start()
    need("Get set up" in drv("print(labels())"), "the first-run checklist isn't on the main window")


@check
def usb_pairing(a):
    return pair_over_usb()


@check
def stream_and_status(a):
    line = start_stream()
    detail = status_detail()
    need("over USB" in detail and "fps" in detail, f"the phone's Status card says {detail!r}")
    note = line.split("Status: ")[-1]
    if sdk() >= 36:
        n = stream_notification()
        need("PROMOTED_ONGOING" in n.get("flags", ""), f"the stream notification isn't a Live Update: {n}")
        need(n.get("title") == "Streaming" and "over USB" in n.get("text", "") and n.get("actions", 0) >= 1,
             f"the Live Update reads {n}")
        note += ", Live Update"
    return note


@check
def transforms_apply_live(a):
    import numpy as np  # noqa: PLC0415
    dev = vcam_device()
    before = grab(dev)
    click("Mirror")
    time.sleep(2)
    mirrored = grab(dev)
    click("Mirror")
    click("180°")
    time.sleep(2)
    turned = grab(dev)
    click("0°")
    diff = lambda x, y: abs(x - y).mean()  # noqa: E731
    need(diff(np.flip(before, 1), mirrored) < diff(before, mirrored) / 2, "Mirror didn't flip the output")
    need(diff(np.flip(np.flip(before, 0), 1), turned) < diff(before, turned) / 2, "180° didn't turn the output")


@check
def wait_screen(a):
    dev = vcam_device()
    stop_stream()
    frame = grab(dev)
    need(frame.std() > 10, "the virtual camera shows a flat picture with nothing streaming")


@check
def remote_start_toast_once(a):
    launch()
    if not next((n["checked"] for n in nodes() if n["id"] == "checkWaitForComputer"), False):
        tap("Wait for my computer")
    shell("input", "keyevent", "KEYCODE_HOME")
    time.sleep(2)
    start_stream()
    shown = []
    for _ in range(3):
        launch(settle=0)  # a short toast is gone from the queue a couple of seconds later
        shown.append(1 if wait_for(lambda: any("started from your computer" in t for t in toasts()), 3, 0.3) else 0)
        time.sleep(2)
        shell("input", "keyevent", "KEYCODE_HOME")
        time.sleep(3)
    stop_stream()
    launch()
    tap("Wait for my computer")
    need(shown[0] >= 1 and not any(shown[1:]), f"toast per opening: {shown}")


@check
def repair_replaces_token(a):
    old = token()
    launch()
    rows = computer_rows()
    pair_over_usb()
    new = token()
    need(new and new != old, "re-pairing kept the same token")
    need(ping(old) == 401 and ping(new) == 200, f"old token {ping(old)}, new token {ping(new)}")
    launch()
    need(computer_rows() == rows, f"the phone lists {computer_rows()} computers after re-pairing, had {rows}")


@check
def remove_on_phone(a):
    start_stream()
    old = token()
    launch()
    mark = log_mark()
    host = os.uname().nodename
    tap(f"Remove {host}?")
    tap("Remove")
    need(wait_log(mark, r"Stream dropped|Not streaming", 5), "the stream didn't stop after removing the computer")
    need(ping(old) == 401, "the removed computer's token still works")
    need(wait_log(mark, r"doesn't recognise this computer", 10), "the desktop didn't say the phone forgot it")
    pair_over_usb()


@check
def remove_on_desktop(a):
    launch()
    before = computer_rows()
    drv("[b for b in buttons(main_win()) if b.accessibleName() == 'Your phones'][0].click()", 2)
    time.sleep(1)
    drv("d = dialogs('PhonesDialog')[0]\n[b for b in buttons(d) if b.accessibleName().startswith('Remove')][0].click()", 2)
    time.sleep(1)
    drv("answer('Yes')")
    time.sleep(4)
    drv("for w in dialogs(): w.close()")
    launch()
    need(computer_rows() == before - 1, f"the phone still lists {computer_rows()} computers (had {before})")
    pair_over_usb()


@check
def app_closed_banner(a):
    shell("am", "force-stop", PKG)
    mark = log_mark()
    click("Start streaming")
    need(wait_log(mark, r"Banner: Can't connect", 30), "no banner when the phone app is closed")
    launch()
    click("Try again")
    need(wait_log(mark, r"Status: Streaming", 40), "Try again didn't start the stream")
    stop_stream()


@check
def heat_step(a):
    sizes = "c = combo('Resolution')\nprint([c.itemText(i) for i in range(c.count())])"
    start_stream()  # the sizes come from the phone's camera once it's streaming
    items = wait_for(lambda: (lambda got: got if "1920" in got else None)(drv(sizes)), 15) or ""
    stop_stream()
    if "1920" not in items:
        raise Skip("this emulator's camera has no 1080p, so the heat step can't drop the size")
    drv("c = combo('Resolution')\nc.setCurrentIndex([i for i in range(c.count()) if c.itemText(i).startswith('1920')][0])")
    start_stream()
    mark = log_mark()
    shell("cmd", "thermalservice", "override-status", "4")
    try:
        need(wait_log(mark, r"The phone is very hot.*1280 × 720 and 15 fps", 40), "the desktop didn't say the phone is very hot")
        launch()
        need("720p 15 fps" in status_detail(), f"the phone's Status card says {status_detail()!r} while hot")
    finally:
        shell("cmd", "thermalservice", "reset")
    cooled = wait_for(lambda: "1080p 30 fps" in status_detail(), 70, 5)
    need(cooled, "the phone didn't go back to 1080p 30 fps after cooling down")
    need(not wait_for(lambda: "very hot" in drv("print(labels())"), 1), "the hot note stayed after cooling down")
    stop_stream()


def _drop_zip(path):
    drv(f"""
from PyQt6.QtCore import QMimeData, QUrl, QPointF
from PyQt6.QtGui import QDropEvent
d = settings_tab('Plugins')
p = [w for w in d.findChildren(QWidget) if type(w).__name__ == 'PluginsPanel'][0]
m = QMimeData(); m.setUrls([QUrl.fromLocalFile({str(path)!r})])
p.dropEvent(QDropEvent(QPointF(10, 10), Qt.DropAction.CopyAction, m, Qt.MouseButton.NoButton, Qt.KeyboardModifier.NoModifier))
""", 3)
    time.sleep(1)
    drv("answer('OK')")


def _switch(title, on, confirm):
    drv(f"settings_tab('Plugins')\ns = plugin_switch({title!r})\nif s.isChecked() != {on}: s.click()", 3)
    time.sleep(1)
    drv(f"answer({confirm!r})")
    state = drv(f"print(plugin_switch({title!r}).isChecked())")
    need(str(on) in state, f"{title} didn't switch {'on' if on else 'off'}")


def _zip(folder: Path, tmp: Path) -> Path:
    out = tmp / f"{folder.name}.zip"
    with zipfile.ZipFile(out, "w") as z:
        for f in folder.rglob("*"):
            if "__pycache__" not in f.parts and f.is_file():
                z.write(f, f.relative_to(folder.parent))
    return out


def _restart():
    app_stop()
    app_start()


@check
def plugins(a):
    try:
        return _plugins(a)
    finally:  # whatever happened, leave no test plugins and every built-in on for the checks after this one
        app_stop()
        shutil.rmtree(CONFIG_DIR / "plugins", ignore_errors=True)
        cfg = CONFIG_DIR / "telescope_config.json"
        config = json.loads(cfg.read_text())
        config["plugins"] = {"off": [], "noticed": [], "added": {}}
        cfg.write_text(json.dumps(config, indent=2))
        app_start()


def _plugins(a):
    tmp = Path(tempfile.mkdtemp())
    _drop_zip(_zip(REPO / "desktop" / "examples" / "plugins" / "black_and_white", tmp))
    need("Black and white" in drv("print(labels(settings_tab('Plugins')))"), "the dropped plugin isn't under Added")
    _switch("Black and white", True, "Switch on")
    _restart()
    need("Black and white" in drv("print(labels())"), "the plugin's card isn't there after a restart")
    start_stream()
    drv("[c for c in main_win().findChildren(QAbstractButton) if c.text() == 'Black and white picture'][0].click()")
    time.sleep(2)
    need(colour_spread(grab(vcam_device())) < 10, "the output still has colour with Black and white ticked")
    stop_stream()

    plugins_dir = CONFIG_DIR / "plugins"
    with open(plugins_dir / "black_and_white" / "__init__.py", "a") as f:
        f.write("# changed\n")
    boom = tmp / "boom"
    boom.mkdir()
    (boom / "plugin.json").write_text(json.dumps({"name": "boom", "title": "Boom", "version": "1.0", "author": "test",
                                                  "description": "Raises in setup.", "api": 1, "class": "BoomPlugin"}))
    (boom / "__init__.py").write_text("from telescope.plugin import TelescopePlugin\n\n\nclass BoomPlugin(TelescopePlugin):\n"
                                      "    name = 'boom'\n\n    def setup(self, host, bus):\n        raise RuntimeError('on purpose')\n")
    _drop_zip(_zip(boom, tmp))
    _switch("Boom", True, "Switch on")
    _switch("Transforms", False, "Switch off")
    mark = log_mark()
    _restart()
    need(wait_log(mark, r"Banner: .*Boom didn't start|Banner: .*and Boom didn't start", 5), "no banner naming the plugin that crashed")
    texts = drv("print([l.text() for l in settings_tab('Plugins').findChildren(QLabel)])")
    need("files changed" in texts, "the changed plugin isn't switched off with the reason")
    gone = wait_for(lambda: "Transforms" not in drv("print([l for l in labels() if l == 'Transforms'])"), 10)
    need(gone, "Transforms' card is still there")
    start_stream()
    stop_stream()
    _switch("Transforms", True, "Switch on")
    for name in ("Boom", "Black and white"):
        drv(f"[b for b in buttons(settings_tab('Plugins')) if b.accessibleName() == 'Remove {name}'][0].click()", 2)
        time.sleep(1)
        drv("answer('Remove') or answer('Yes')")
    _restart()
    labels_now = drv("print(labels())")
    need("Transforms" in labels_now and "Black and white" not in labels_now, "after removing: " + labels_now[:200])
    need(not any(plugins_dir.iterdir()) if plugins_dir.exists() else True, "plugin folders were left behind")


@check
def corrupt_config(a):
    app_stop()
    cfg = CONFIG_DIR / "telescope_config.json"
    kept = cfg.read_bytes()
    cfg.write_text('{"x":\n  garbage')
    mark = log_mark()
    try:
        app_start()
        need(wait_log(mark, r"Banner: Your settings couldn't be read", 5), "no banner about the unreadable settings")
        need(list(CONFIG_DIR.glob("telescope_config.json.invalid-*")), "no .invalid- copy of the bad file")
    finally:
        app_stop()
        cfg.write_bytes(kept)
        app_start()


@check
def single_instance(a):
    env_sh = f"source {LINUX / 'env.sh'}; cd \"$RT_PROFILE/apps/Telescope\"; timeout 20 \"$XDG_DATA_HOME/Telescope/venv/bin/python3\" main.py"
    r = subprocess.run(["bash", "-c", env_sh], capture_output=True)
    need(r.returncode == 0, f"a second launch didn't hand over to the first (exit {r.returncode})")
    need(app_running(), "the first instance went away")


@check
def menu_entry_and_autostart(a):
    entry = PROFILE / "home" / ".local" / "share" / "applications" / "telescope.desktop"
    app_dir = PROFILE / "apps" / "Telescope"
    moved = PROFILE / "apps" / "Moved"
    app_stop()
    app_dir.rename(moved)
    try:
        app_start(moved)
        need(f"{moved}/start.sh" in entry.read_text(), "the menu entry didn't follow the moved folder")
    finally:
        app_stop()
        moved.rename(app_dir)
        app_start()
    autostart = PROFILE / "home" / ".config" / "autostart" / "telescope.desktop"
    flip = ("d = settings_tab('General')\nl = [l for l in d.findChildren(QLabel) if l.text() == 'Open Telescope when I sign in'][0]\n"
            "[b for b in l.parentWidget().findChildren(QAbstractButton) if b.isCheckable()][0].click()")
    drv(flip)
    time.sleep(1)
    need(autostart.exists() and "--minimized" in autostart.read_text(), "ticking sign-in didn't write the autostart entry")
    drv(flip)
    time.sleep(1)
    need(not autostart.exists(), "unticking sign-in left the autostart entry")


@check
def copy_diagnostics(a):
    version = json.loads((a.build / "manifest.json").read_text())["version"]
    out = drv("d = settings_tab('Updates')\nclick('Copy diagnostics', d)\nprint(QApplication.clipboard().text()[:80])")
    need(out.startswith(f"Telescope {version}"), f"Copy diagnostics starts {out[:60]!r}")
    launch()
    tap("Copy diagnostics")
    return "phone copied too"


@check
def upgrade_over_usb(a):
    if not a.previous:
        raise Skip("no --previous release to upgrade from")
    adb("uninstall", PKG)
    adb("install", str(a.previous / "Telescope.apk"), check=True)
    shell("pm", "grant", PKG, "android.permission.CAMERA")
    old = apk_version()
    pair_over_usb()
    drv("click('Install over USB', settings_tab('Virtual camera'))", 3)
    need(wait_for(lambda: apk_version() != old and apk_version(), 90, 3), "Install over USB didn't update the phone app")
    launch()
    start_stream()
    stop_stream()
    return f"{old} to {apk_version()}, still paired"


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("build", type=Path, nargs="?", help="folder from fetch_nightly.sh with the release to check")
    parser.add_argument("--previous", type=Path, help="an older release's folder, for the upgrade check")
    parser.add_argument("--only", nargs="+", metavar="NAME", help="run just these checks (they may lean on earlier ones)")
    parser.add_argument("--list", action="store_true", help="list the checks and stop")
    a = parser.parse_args()
    if a.list:
        print("\n".join(fn.__name__ for fn in CHECKS))
        return 0
    if not a.build:
        parser.error("the release folder is needed")
    devices = [line for line in adb("devices").splitlines()[1:] if line.strip().endswith("device")]
    if len(devices) != 1:
        parser.error(f"adb needs to see exactly one device, sees {len(devices)}")
    shell("svc", "wifi", "enable")
    shell("svc", "data", "enable")

    results = []
    for fn in CHECKS:
        if a.only and fn.__name__ not in a.only:
            continue
        try:
            note = fn(a)
            results.append(("PASS", fn.__name__, note or ""))
        except Skip as e:
            results.append(("SKIP", fn.__name__, str(e)))
        except Fail as e:
            results.append(("FAIL", fn.__name__, str(e)))
        except Exception as e:  # a broken check shouldn't stop the rest
            results.append(("FAIL", fn.__name__, f"{type(e).__name__}: {e}"))
        print(f"{results[-1][0]:<5} {fn.__name__:<26} {results[-1][2]}", flush=True)
    adb("forward", "--remove", f"tcp:{FORWARD_PORT}")
    app_stop()  # a copy left running holds the single-instance lock and the next run would talk to it
    failed = sum(r[0] == "FAIL" for r in results)
    print(f"\n{len(results) - failed} of {len(results)} passed or skipped, {failed} failed")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
