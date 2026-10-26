import json
import threading
from pathlib import Path
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from types import SimpleNamespace

import pytest

from telescope.pinned_https import PhoneAuth
from telescope.stills import StillCaps, Stills, free_path, reason_text, safe_name

FULL = {"formats": ["jpeg", "heic", "ultrahdr", "raw"], "width": 4080, "height": 3072, "maxResWidth": 8160,
        "maxResHeight": 6144, "maxResRaw": True, "extensions": ["night", "hdr"], "exposureMaxNs": 30_000_000_000,
        "bracketing": True, "focusBracketing": True, "noiseReductionHq": True, "sharpeningHq": True,
        "hotPixelHq": True, "distortionCorrection": True, "burstMax": 60}


def _state(stills=FULL, version=1, current=True):
    cams = [{"id": "0", "current": not current}, {"id": "2", "current": current, "stills": stills}]
    return {"cameras": cams, **({"stills": version} if version is not None else {})}


# ── What the lens takes ──────────────────────────────────────────────────────

def test_the_current_lens_says_what_it_takes():
    caps = StillCaps.from_state(_state())
    assert caps.formats == ("jpeg", "heic", "ultrahdr", "raw")
    assert caps.size == (4080, 3072) and caps.max_res == (8160, 6144) and caps.max_res_raw
    assert caps.extensions == ("night", "hdr") and caps.exposure_max_ns == 30_000_000_000
    assert caps.bracketing and caps.focus_bracketing and caps.distortion and caps.burst_max == 60


def test_an_older_phone_or_a_lens_without_photos_takes_none():
    assert StillCaps.from_state(_state(version=None)) is None  # a phone without /v1/still
    assert StillCaps.from_state(_state(version=0)) is None
    assert StillCaps.from_state(_state(stills=None)) is None  # this lens hasn't been read yet
    assert StillCaps.from_state(_state(current=False)) is None  # the lens that has them isn't the current one
    assert StillCaps.from_state({}) is None and StillCaps.from_state(None) is None


def test_odd_values_fall_back_to_the_least_it_can_do():
    caps = StillCaps.from_state(_state({"formats": "raw", "width": "x", "burstMax": -3, "maxResRaw": "yes"}))
    assert caps.formats == ("jpeg",) and caps.size is None and caps.burst_max == 1 and not caps.max_res_raw
    assert caps.max_res is None and caps.extensions == () and caps.exposure_max_ns == 0


# ── Files ────────────────────────────────────────────────────────────────────

def test_a_name_already_taken_gets_a_number(tmp_path):
    assert free_path(tmp_path, "IMG_1.jpg") == tmp_path / "IMG_1.jpg"
    (tmp_path / "IMG_1.jpg").touch()
    (tmp_path / "IMG_1 (2).jpg.part").touch()  # one being written counts as taken
    assert free_path(tmp_path, "IMG_1.jpg") == tmp_path / "IMG_1 (3).jpg"


def test_names_from_the_phone_stay_in_the_folder():
    assert safe_name("../../etc/passwd", 0) == "passwd"
    assert safe_name("IMG_20261026_142501_01.dng", 0) == "IMG_20261026_142501_01.dng"
    assert safe_name("a b<c>.jpg", 0) == "a_b_c_.jpg"
    assert safe_name("", 2) == "photo_3.jpg" and safe_name("...", 0) == "photo_1.jpg"


def test_reasons_are_worded_here_and_unknown_ones_keep_the_phones_english():
    assert "already" in reason_text("busy")
    assert reason_text("some_new_code", "The phone said something new") == "The phone said something new"
    assert reason_text("some_new_code") == reason_text("failed")


# ── Taking one, against a stand-in phone ─────────────────────────────────────

class _Phone:
    """/v1/still as the phone serves it: a photo that takes a couple of polls and leaves the files given."""

    def __init__(self, files=None, polls=2, start=None, fail=None, missing=False):
        self.files = files if files is not None else {"IMG_1.jpg": b"jpeg" * 100, "IMG_2.dng": b"dng!" * 5000}
        self.polls, self.start, self.fail, self.missing = polls, start, fail, missing
        self.posts, self.forgotten = [], []
        phone = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_a):
                pass

            def _json(self, body, code=200):
                data = json.dumps(body).encode()
                self.send_response(code)
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)

            def do_POST(self):
                body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                assert self.headers["Authorization"] == "Bearer token"
                if phone.missing:
                    return self._json({"error": "Not Found"}, 404)
                if body["action"] == "forget":
                    phone.forgotten.append(body["id"])
                    return self._json({"ok": True})
                phone.posts.append(body)
                self._json(phone.start or {"ok": True, "id": "abc"})

            def do_GET(self):
                if self.path.startswith("/v1/still/file"):
                    n = int(self.path.rsplit("n=", 1)[1])
                    data = list(phone.files.values())[n]
                    self.send_response(200)
                    self.send_header("Content-Length", str(len(data)))
                    self.end_headers()
                    self.wfile.write(data)
                    return
                phone.polls -= 1
                done = phone.polls <= 0
                files = [{"name": k, "size": len(v), "type": "image/jpeg"} for k, v in phone.files.items()] if done else []
                state = ("failed" if phone.fail else "done") if done else "capturing"
                self._json({"id": "abc", "state": state, "shots": 2, "taken": 2 if done else 1, "files": files,
                            **({"code": phone.fail, "error": "English"} if phone.fail and done else {})})

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.ctrl = SimpleNamespace(base=f"http://127.0.0.1:{self.server.server_port}/v1", auth=PhoneAuth("token"))

    def close(self):
        self.server.shutdown()


@pytest.fixture
def phone():
    made = []

    def make(**kw):
        made.append(_Phone(**kw))
        return made[-1]
    yield make
    for p in made:
        p.close()


def _take(stills, phone, folder, options=None, source="phone-1"):
    done, progress, result = threading.Event(), [], {}

    def on_done(r):
        result["r"] = r
        done.set()
    assert stills.take(source, phone.ctrl, options or {"format": "jpeg", "raw": 1}, folder, on_done, progress.append)
    assert done.wait(10)
    return result["r"], progress


def test_a_photo_is_saved_file_by_file_and_the_phone_forgets_it(tmp_path, phone):
    p = phone()
    result, progress = _take(Stills(), p, tmp_path / "Photos")
    assert result.ok and [Path(f).name for f in result.files] == ["IMG_1.jpg", "IMG_2.dng"]
    assert (tmp_path / "Photos" / "IMG_2.dng").read_bytes() == b"dng!" * 5000
    assert p.posts == [{"format": "jpeg", "raw": 1, "action": "take"}]
    assert p.forgotten == ["abc"]
    assert progress[0].state == "capturing" and progress[-1].state == "done" and progress[-1].saved == result.files
    assert not list((tmp_path / "Photos").glob("*.part"))


def test_files_with_the_same_name_dont_overwrite_earlier_photos(tmp_path, phone):
    (tmp_path / "IMG_1.jpg").write_bytes(b"older")
    result, _ = _take(Stills(), phone(), tmp_path)
    assert result.files[0].endswith("IMG_1 (2).jpg") and (tmp_path / "IMG_1.jpg").read_bytes() == b"older"


def test_a_refusal_is_worded_from_its_code(tmp_path, phone):
    result, _ = _take(Stills(), phone(start={"ok": False, "code": "unsupported", "error": "This lens can't save HEIC"}),
                      tmp_path)
    assert not result.ok and result.code == "unsupported" and "can't take that photo" in result.error


def test_a_photo_that_failed_on_the_phone_keeps_what_was_saved(tmp_path, phone):
    p = phone(fail="interrupted", files={"IMG_1.jpg": b"x"})
    result, _ = _take(Stills(), p, tmp_path)
    assert not result.ok and result.code == "interrupted" and len(result.files) == 1
    assert p.forgotten == ["abc"]


def test_an_older_phone_without_photos(tmp_path, phone):
    result, _ = _take(Stills(), phone(missing=True), tmp_path)
    assert result.code == "no_photos"


def test_one_photo_at_a_time_per_phone(tmp_path, phone):
    stills, p = Stills(), phone(polls=4)
    gate = threading.Event()
    assert stills.take("phone-1", p.ctrl, {"format": "jpeg"}, tmp_path, lambda r: gate.set())
    assert stills.busy("phone-1") and not stills.busy("phone-2")
    assert not stills.take("phone-1", p.ctrl, {"format": "jpeg"}, tmp_path, lambda r: None)
    assert gate.wait(20)
    assert not stills.busy("phone-1")


def test_a_folder_that_cant_be_made_fails_cleanly(tmp_path, phone):
    blocker = tmp_path / "file"
    blocker.write_text("not a folder")
    result, _ = _take(Stills(), phone(), blocker / "Photos")
    assert not result.ok and result.code == "save"
