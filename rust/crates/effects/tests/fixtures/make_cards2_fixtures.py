"""Records what the Python app does for On Air, QR codes and Photos, for tests/cards2_parity.rs in the app crate to compare the Rust
plugins with.

    cd desktop && QT_QPA_PLATFORM=offscreen python ../rust/crates/effects/tests/fixtures/make_cards2_fixtures.py

Writes next to this file: cards2_configs.json (what get_config answers after set_config), on_air.json (the webhook each url, method
and payload makes, and which urls are usable), photos.json (what each lens offers and the options each pick sends), qr.json (Wi-Fi
codes, which quads look like codes, and what the reader finds in each of the frames) with the frames as qr_NN.gray.z (one byte a pixel, zlib).
"""
import json
import sys
import zlib
from pathlib import Path

import cv2
import numpy as np

HERE = Path(__file__).resolve().parent
DESKTOP = HERE.parents[4] / "desktop"
sys.path.insert(0, str(DESKTOP))

from telescope import on_air  # noqa: E402
from telescope.plugins import photos, qr_reader  # noqa: E402
from telescope.plugins.on_air import OnAirPlugin  # noqa: E402
from telescope.stills import StillCaps  # noqa: E402


class _Host:
    def __init__(self):
        self.saves = 0

    def schedule_save(self):
        self.saves += 1

    def plugin_config(self, name):
        return {}

    def add_preview_layer(self, owner, layer):
        pass

    def remove_preview_layer(self, owner):
        pass

    def refresh_preview(self):
        pass

    def still_caps(self, source_id=None):
        return None

    def still_busy(self, source_id=None):
        return False

    def is_streaming(self):
        return False


_APP = []


def _plugin(factory):
    from PyQt6.QtWidgets import QApplication
    from telescope.plugin import EventBus
    if not _APP:
        _APP.append(QApplication.instance() or QApplication([]))
    plugin = factory()
    plugin.setup(_Host(), EventBus())
    return plugin


def configs() -> list:
    """What each plugin saves after loading odd settings (None when set_config refuses them)."""
    out = []

    def on_air_case(cfg):
        plugin = _plugin(lambda: OnAirPlugin(server_cls=object, watch_cls=object, sender_cls=lambda cb: None))
        plugin.set_config(cfg)
        got = plugin.get_config()
        got["token"] = "<token>" if got["token"] != cfg.get("token") else got["token"]  # a made one is random
        out.append({"plugin": "on_air", "input": cfg, "output": got})

    token = "abcdefghijklmnop_-ABCDEF"
    for cfg in [
        {}, {"trigger": "mic", "page": False, "token": token, "address": "192.168.1.5", "webhook_url": "http://hub/x",
             "webhook_method": "GET", "webhook_on": "on", "webhook_off": "off"},
        {"trigger": "moon", "token": "x", "webhook_method": "PUT", "webhook_url": 5},
        {"trigger": ["camera"], "page": 0, "token": "a" * 16}, {"page": None}, {"page": "no"}, {"token": "a" * 65},
        {"token": "a" * 64}, {"token": "has space......"}, {"token": 5}, {"address": 5, "webhook_on": 1, "webhook_off": None},
        {"trigger": "camera_mic", "webhook_url": " http://hub/x "}, {"trigger": "streaming", "unknown": 1},
    ]:
        on_air_case(cfg)

    def qr_case(cfg):
        from PyQt6.QtWidgets import QApplication
        QApplication.instance() or QApplication([])
        plugin = _plugin(qr_reader.QrReaderPlugin)
        plugin.create_panel()
        plugin.set_config(cfg)
        out.append({"plugin": "qr_reader", "input": cfg, "output": plugin.get_config()})

    for cfg in [{}, {"on": True}, {"on": False, "notify": False}, {"on": 1, "notify": 0}, {"on": "yes", "notify": None},
                {"notify": "no"}, {"on": True, "notify": True, "other": 3}]:
        qr_case(cfg)

    def photos_case(cfg):
        from PyQt6.QtWidgets import QApplication
        QApplication.instance() or QApplication([])
        plugin = _plugin(photos.PhotosPlugin)
        plugin.create_panel()
        plugin.set_config(cfg)
        got = plugin.get_config()
        got["folder"] = "<default>" if got["folder"] == photos.default_folder() else got["folder"]
        out.append({"plugin": "photos", "input": cfg, "output": got})

    for cfg in [
        {}, {"format": "heic", "timer": 3, "best": False, "folder": "/x"}, {"timer": 7, "best": "yes", "burst": True, "folder": 5},
        {"quality": 80.5, "burst": 7.9, "focus_steps": "8", "exposure_s": 8}, {"quality": "high", "exposure_s": None},
        {"format": 5, "mode": None, "shots": ["x"], "bracket": 3, "distortion": {}}, {"folder": ""}, {"timer": 10, "best": 0},
        {"quality": 1000, "burst": -3}, {"quality": True, "burst": False}, {"unknown": 1, "folder": "/photos"},
        {"format": "raw+jpeg", "mode": "night", "shots": "focus", "bracket": "7x1", "distortion": "off", "exposure_s": 0.5},
    ]:
        photos_case(cfg)
    return out


URLS = [
    "http://hub.local/hook", "https://1.2.3.4:8123/api", "HTTP://Hub.Local/x", "http://[::1]:8123/x", "http://[", "http://[::1", "http://[nope]/x",
    "", "hub.local/x", "ftp://hub/x", "http://", "http://hub/a b", "http:///x", "http://hub/a\u200bb", "http://user:pw@hub:80/x?y=1#z",
    "http://hub", "http://hub?x=1", "http://hub/path?x=1&y=2#frag", "https://hub/a?b=c d", "  http://hub/x", "http://hub/x\n", "http://hub/ x",
    "http:/hub/x", "http://:80/x", "http://@/x", "mailto:a@b", "http://hub/%20x", "http://hub:abc/x", "http://[fe80::1%eth0]/x",
    "http://hub/\u00e9", "HTTPS://HUB/CaSe?Q=1", "http://hub//x//y", "http://hub/x?", "http://hub/x#", "http://hub/x?#", "javascript:alert(1)",
    "http://hub/\u00a0x", "http://hub/x\u2028y", "http://hub/\ue000",
]
PAYLOADS = ["", "turn=on", "?a=b&c=d", "&x=1", "state=on air&note=\"hi\"", "light=100%25", "busy", '{"a": 1}', "  {\"a\":1}  ", "NaN", "[1, 2]",
            "name=caf\u00e9", "a+b=c d", "x=1;y=2", "{a: 1}", "true", "12", "\"s\"", "Infinity", "-Infinity"]


def webhooks() -> dict:
    problems = [{"url": u, "problem": bool(on_air.webhook_problem(u))} for u in URLS]
    requests = []
    for url in URLS:
        if on_air.webhook_problem(url):
            continue
        for method in ("POST", "GET", "PUT"):
            for payload in PAYLOADS[::3]:
                for state in (True, False):
                    req = on_air.webhook_request(url, method, payload, state)
                    requests.append({"url": url, "method": method, "payload": payload, "on": state, "full_url": req.full_url,
                                     "http_method": req.get_method(), "data": req.data.decode() if req.data else None,
                                     "content_type": req.get_header("Content-type")})
    return {"problems": problems, "requests": requests}


FULL = StillCaps(formats=("jpeg", "heic", "ultrahdr", "raw"), size=(4080, 3072), max_res=(8160, 6144), max_res_raw=True,
                 extensions=("night", "hdr", "bokeh", "auto"), exposure_max_ns=30_000_000_000, bracketing=True, focus_bracketing=True,
                 noise_reduction_hq=True, sharpening_hq=True, hot_pixel_hq=True, distortion=True, burst_max=60,
                 extension_sizes=(("night", (3840, 2160)),))
LENSES = {
    "plain": StillCaps(size=(1920, 1080)),
    "full": FULL,
    "no_raw_max_res": StillCaps(formats=("jpeg", "raw"), max_res=(8000, 6000), max_res_raw=False, size=(4000, 3000)),
    "bursts": StillCaps(burst_max=4, focus_bracketing=True, bracketing=False, size=(1000, 1000)),
    "long_exposure_only": StillCaps(exposure_max_ns=1_000_000_000, size=(2000, 1500), distortion=True),
    "extensions": StillCaps(extensions=("hdr", "auto"), size=(3000, 2000), noise_reduction_hq=True),
    "heic_only": StillCaps(formats=("heic",), size=(3000, 2000)),
}


def caps_json(caps: StillCaps) -> dict:
    return {"formats": list(caps.formats), "size": list(caps.size) if caps.size else None,
            "max_res": list(caps.max_res) if caps.max_res else None, "max_res_raw": caps.max_res_raw, "extensions": list(caps.extensions),
            "exposure_max_ns": caps.exposure_max_ns, "bracketing": caps.bracketing, "focus_bracketing": caps.focus_bracketing,
            "noise_reduction_hq": caps.noise_reduction_hq, "sharpening_hq": caps.sharpening_hq, "hot_pixel_hq": caps.hot_pixel_hq,
            "distortion": caps.distortion, "burst_max": caps.burst_max,
            "extension_sizes": [[m, list(s)] for m, s in caps.extension_sizes]}


def photo_options() -> dict:
    picks = []
    modes = ["normal", "max_res", "night", "hdr", "bokeh", "auto", "moon"]
    formats = ["jpeg", "heic", "ultrahdr", "raw", "raw+jpeg", "png"]
    shots = ["single", "burst", "bracket", "focus", "weird"]
    for mode in modes:
        for fmt in formats:
            for shot in shots:
                picks.append({"mode": mode, "format": fmt, "shots": shot})
    extras = [
        {"quality": 88}, {"quality": 1}, {"quality": 100}, {"quality": 40}, {"burst": 3}, {"burst": 100}, {"burst": 1}, {"bracket": "5x1"},
        {"bracket": "7x1"}, {"bracket": "3x2"}, {"bracket": "nope"}, {"focus_steps": 8}, {"focus_steps": 99}, {"exposure_s": 0.5},
        {"exposure_s": 8}, {"exposure_s": 120}, {"timer": 3}, {"timer": 10}, {"timer": 7}, {"best": False}, {"distortion": "on"},
        {"distortion": "off"}, {"distortion": "camera"},
    ]
    cases = []
    for lens, caps in LENSES.items():
        for n, pick in enumerate(picks[::7] + [dict(p, **e) for p in picks[::61] for e in extras]):
            choices = dict(photos.DEFAULTS, **pick)
            rotation = 90 if n % 5 == 0 else 0
            cases.append({"lens": lens, "choices": {k: v for k, v in choices.items() if photos.DEFAULTS.get(k) != v or k in pick},
                          "rotation": rotation, "offered": photos.offered(caps, choices), "options": photos.build_options(caps, choices, rotation)})
    return {"lenses": {name: caps_json(c) for name, c in LENSES.items()}, "cases": cases,
            "sizes": {name: {m: (list(c.size_for(m)) if c.size_for(m) else None) for m in ("normal", "max_res", "night", "hdr", "auto")}
                      for name, c in LENSES.items()},
            "diagnostics": {name: [photos.offered(c, {})["modes"]] for name, c in LENSES.items()}}


QUADS = [
    [[100, 100], [200, 100], [200, 200], [100, 200]], [[100, 100], [200, 110], [190, 190], [110, 200]], [[100, 100], [200, 100], [120, 120], [100, 200]],
    [[100, 100], [200, 200], [200, 100], [100, 200]], [[100, 100], [400, 100], [400, 112], [100, 112]], [[100, 100], [108, 100], [108, 108], [100, 108]],
    [[100, 100], [200, 100], [200, 200]], [[0, 0], [16, 0], [16, 16], [0, 16]], [[0, 0], [15, 0], [15, 15], [0, 15]],
    [[0, 0], [100, 0], [100, 30], [0, 30]], [[0, 0], [100, 0], [100, 29], [0, 29]], [[0, 0], [50, 0], [50, 50], [0, 50]],
    [[0, 0], [100, 0], [100, 100], [0, 100]], [[0, 0], [100, 0], [100, 100], [0, 100], [5, 5]], [[0, 0], [100, 0], [50, 0], [0, 100]],
    [[10, 10], [110, 12], [112, 111], [8, 109]], [[0, 0], [1e9, 0], [1e9, 1e9], [0, 1e9]], [[float("nan"), 0], [10, 0], [10, 10], [0, 10]],
    [[0, 0], [100, 0], [100, 100], [0, 100]][::-1], [[0, 0], [100, 0], [30, 30], [0, 100]], [[0, 0], [100, 0], [100, 20], [0, 20]],
    [[0, 0], [100, 0], [100, 31], [0, 31]],
]

WIFI = [
    "WIFI:T:WPA;S:Home;P:hunter2;;", r"WIFI:S:My\;Net\:5G;T:WPA;P:pa\\ss\,word;H:true;;", "wifi:T:nopass;S:Cafe;;", "WIFI:T:WPA;P:nameless;;",
    "https://example.com", "WIFI:", "WIFI:S:x", "WIFI:S:x;;", "WIFI:S:a;S:b;P:1;P:2;;", "WIFI:s:lower;p:pw;t:WEP;h:TRUE;;", "WIFI:S:end\\", "WIFI:S:\\\"quoted\\\";;",
    "WIFI:S:colon:inside;P:a:b;;", "WIFI:S:semi\\;colon;P:\\;;;", "WIFI:T:WPA;S:;P:x;;", "WIFI:S:n;H:false;;", "WIFI:S:n;H:True;;", "WIFI:S:n;H:yes;;",
    "WIFI:S:\u00e9\u00e8;P:\u4e2d\u6587;;", "WIFI:S:x;P:;;", "WIFI:T:;S:x;;", "wiFi:S:mixed;;", "WIFI:SS:x;;", "WIFI:S:x;;extra", "WIFI:S:x;P:y", "WIFI:;S:x;;",
    "WIFI::S:x;;", "WIFI:S:x:y:z;;",
]
LINKS = ["http://a", "HTTPS://B", "https://", "ftp://x", "httpx://x", " http://x", "http:/x", "", "http", "text"]


def _qr_frame(text, scale, offset=(400, 10), size=(720, 1280), border=0, blur=1.0, angle=0.0, shade=90):
    code = cv2.QRCodeEncoder.create().encode(text)
    code = cv2.resize(code, (code.shape[1] * scale, code.shape[0] * scale), interpolation=cv2.INTER_NEAREST)
    if border:
        code = cv2.copyMakeBorder(code, border, border, border, border, cv2.BORDER_CONSTANT, value=255)
    grey = np.full(size, shade, np.uint8)
    h, w = code.shape
    grey[offset[1]:offset[1] + h, offset[0]:offset[0] + w] = code
    if angle:
        centre = (offset[0] + w / 2, offset[1] + h / 2)
        grey = cv2.warpAffine(grey, cv2.getRotationMatrix2D(centre, angle, 1.0), (size[1], size[0]), borderValue=shade)
    return cv2.GaussianBlur(grey, (0, 0), blur) if blur else grey


def qr_frames() -> list:
    """(name, text, grey frame): codes from arm's length to right up close, turned, with a border, and none."""
    frames = []
    for scale in (3, 4, 6, 8, 14):
        frames.append((f"url_{scale}x", "https://example.com", _qr_frame("https://example.com", scale)))
    frames.append(("wifi", "WIFI:T:WPA;S:Telescope;P:hunter22;;", _qr_frame("WIFI:T:WPA;S:Telescope;P:hunter22;;", 6, border=20, size=(360, 640),
                                                                         offset=(300, 40), blur=0)))
    frames.append(("long", "https://example.com/a/rather/long/path?with=a&query=string&and=more#fragment-" + "x" * 40,
                   _qr_frame("https://example.com/a/rather/long/path?with=a&query=string&and=more#fragment-" + "x" * 40, 5, border=16)))
    frames.append(("turned_20", "hello", _qr_frame("hello", 8, angle=20.0, border=16)))
    frames.append(("turned_45", "turned code", _qr_frame("turned code", 8, angle=45.0, border=16)))
    frames.append(("unicode", "caf\u00e9 \u4e2d\u6587", _qr_frame("caf\u00e9 \u4e2d\u6587", 8, border=16)))
    frames.append(("digits", "0123456789", _qr_frame("0123456789", 8, border=16)))
    frames.append(("bright", "bright", _qr_frame("bright", 8, shade=200)))
    frames.append(("small_frame", "small", _qr_frame("small", 4, offset=(30, 20), size=(180, 320), blur=0, border=8)))
    frames.append(("nothing", None, np.full((90, 160), 90, np.uint8)))
    noise = np.random.default_rng(7).integers(0, 255, (120, 160), dtype=np.uint8)
    frames.append(("noise", None, noise))
    return frames


def qr() -> dict:
    detectors = (cv2.QRCodeDetectorAruco(), cv2.QRCodeDetector())
    frames = []
    for i, (name, text, grey) in enumerate(qr_frames()):
        (HERE / f"qr_{i:02d}.gray.z").write_bytes(zlib.compress(grey.tobytes(), 9))
        found = qr_reader.read_code(grey, detectors)
        frames.append({"name": name, "text": text, "file": f"qr_{i:02d}.gray.z", "width": grey.shape[1], "height": grey.shape[0],
                       "found": None if found is None else {"text": found[0], "corners": found[1].tolist()}})
    return {"wifi": [{"text": t, "parsed": (lambda w: None if w is None else
                                            {"ssid": w.ssid, "password": w.password, "security": w.security, "hidden": w.hidden})(qr_reader.parse_wifi(t)),
                      "copy": list(qr_reader.QrReaderPlugin.copy(t))} for t in WIFI],
            "links": [{"text": t, "link": qr_reader.is_link(t)} for t in LINKS],
            "quads": [{"points": q, "plausible": bool(qr_reader.plausible(np.array(q, np.float64)) if len(q) == 4 else False)} for q in QUADS[:6]]
                     + [{"points": [[0.0 if v != v else v for v in p] for p in q], "plausible": bool(qr_reader.plausible(np.array(q, np.float64)))}
                        for q in QUADS[6:] if len(q) == 4 and all(len(p) == 2 for p in q)],
            "frames": frames}


def main():
    (HERE / "cards2_configs.json").write_text(json.dumps(configs(), indent=1) + "\n")
    (HERE / "on_air.json").write_text(json.dumps(webhooks(), separators=(",", ":")) + "\n")
    (HERE / "photos.json").write_text(json.dumps(photo_options(), separators=(",", ":")) + "\n")
    (HERE / "qr.json").write_text(json.dumps(qr(), indent=1) + "\n")


if __name__ == "__main__":
    main()
