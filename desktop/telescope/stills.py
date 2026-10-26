"""Full-quality photos from the phone (/v1/still in docs/protocol.md): what the lens can take, and taking them into a
folder. The window runs one Stills for every plugin (HostStills). No Qt here: callbacks come from its worker threads."""
import json
import logging
import os
import re
import threading
import time
import urllib.error
import urllib.request
from dataclasses import dataclass, field
from pathlib import Path
from typing import Callable, Optional

from telescope.i18n import N_, tr
from telescope.session_client import read_capped

logger = logging.getLogger(__name__)

STILLS_VERSION = 1  # the /v1/still this desktop speaks; a phone sends stills: 1 in /v1/state when it takes it
_POLL_S = 0.4
_REQUEST_TIMEOUT_S = 10
_FILE_TIMEOUT_S = 30  # between reads of one file, not for all of it
_LOST_AFTER_S = 30  # the phone not answering for this long ends the photo
_LONGEST_S = 15 * 60  # a photo nobody hears the end of is given up on after this
_STALL_S = 45  # a photo whose status doesn't move for this long (plus its exposures) is given up on
_CHUNK = 1 << 20

# The phone's codes for why a photo didn't happen, worded here in the desktop's language (its English goes along
# for codes this desktop doesn't know)
_REASONS = {
    "unsupported": N_("This phone's camera can't take that photo."),
    "bad_option": N_("The phone didn't accept those photo settings."),
    "not_ready": N_("The phone isn't ready for photos yet. Try again in a moment."),
    "busy": N_("The phone is already taking a photo."),
    "camera_off": N_("The phone's camera is off."),
    "interrupted": N_("The photo was interrupted by a change to the camera."),
    "timeout": N_("The phone took too long to take the photo."),
    "failed": N_("The phone couldn't take the photo."),
    "lost": N_("Lost the phone while it was taking the photo."),
    "dropped": N_("The phone gave up on the photo, maybe because its stream stopped."),
    "save": N_("Telescope couldn't save the photo."),
    "no_photos": N_("This phone can't take photos from Telescope. Update the app on the phone."),
}


def reason_text(code: str, english: str = "") -> str:
    """Why a photo didn't happen, in the desktop's language."""
    if code in _REASONS:
        return tr(_REASONS[code])
    return english or tr(_REASONS["failed"])


def _size(raw: dict, w: str, h: str) -> Optional[tuple]:
    try:
        width, height = int(raw.get(w) or 0), int(raw.get(h) or 0)
    except (TypeError, ValueError):
        return None
    return (width, height) if width > 0 and height > 0 else None


@dataclass(frozen=True)
class StillCaps:
    """What the lens a stream is on can take, from the phone's /v1/state. Anything not here isn't offered."""
    formats: tuple = ("jpeg",)
    size: Optional[tuple] = None  # the largest JPEG
    max_res: Optional[tuple] = None  # the maximum resolution sensor mode's JPEG, when it's bigger
    max_res_raw: bool = False
    extensions: tuple = ()  # "night", "hdr", "bokeh", "auto"
    exposure_max_ns: int = 0  # long exposures up to this; 0 = no manual exposure
    bracketing: bool = False
    focus_bracketing: bool = False
    noise_reduction_hq: bool = False
    sharpening_hq: bool = False
    hot_pixel_hq: bool = False
    distortion: bool = False
    burst_max: int = 1
    extension_sizes: tuple = ()  # (mode, (width, height)) pairs for the maker's modes that said their size

    def size_for(self, mode: str) -> Optional[tuple]:
        """The size a photo in this mode comes out at; a maker's mode that didn't say is taken at the plain size."""
        if mode == "max_res" and self.max_res:
            return self.max_res
        return dict(self.extension_sizes).get(mode) or self.size

    @classmethod
    def from_state(cls, state: Optional[dict]) -> Optional["StillCaps"]:
        """The current lens's, or None when the phone or this lens can't take photos (or hasn't said yet)."""
        if not isinstance(state, dict):
            return None
        try:
            version = int(state.get("stills") or 0)
        except (TypeError, ValueError):
            return None
        if version < STILLS_VERSION:
            return None
        current = next((c for c in state.get("cameras") or () if isinstance(c, dict) and c.get("current")), None)
        raw = current.get("stills") if current else None
        if not isinstance(raw, dict):
            return None

        def words(key: str) -> tuple:
            value = raw.get(key)
            return tuple(v for v in value if isinstance(v, str)) if isinstance(value, list) else ()

        def number(key: str) -> int:
            try:
                return max(0, int(raw.get(key) or 0))
            except (TypeError, ValueError):
                return 0

        formats = words("formats")
        sizes = raw.get("extensionSizes")
        sizes = sizes if isinstance(sizes, dict) else {}
        extension_sizes = tuple((m, _size(v, "width", "height")) for m, v in sizes.items()
                                if isinstance(m, str) and isinstance(v, dict) and _size(v, "width", "height"))
        return cls(
            formats=formats if "jpeg" in formats else ("jpeg",) + formats,
            size=_size(raw, "width", "height"), max_res=_size(raw, "maxResWidth", "maxResHeight"),
            max_res_raw=raw.get("maxResRaw") is True, extensions=words("extensions"),
            extension_sizes=extension_sizes, exposure_max_ns=number("exposureMaxNs"), bracketing=raw.get("bracketing") is True,
            focus_bracketing=raw.get("focusBracketing") is True, noise_reduction_hq=raw.get("noiseReductionHq") is True,
            sharpening_hq=raw.get("sharpeningHq") is True, hot_pixel_hq=raw.get("hotPixelHq") is True,
            distortion=raw.get("distortionCorrection") is True, burst_max=max(1, number("burstMax")),
        )


@dataclass
class StillProgress:
    """How a photo is getting on: the phone's state ("waiting", "capturing", "done", "failed"), and what's saved."""
    state: str = "waiting"
    shots: int = 1
    taken: int = 0
    countdown_ms: int = 0
    paused: bool = False  # the phone paused the video to take it
    saved: list = field(default_factory=list)


@dataclass
class StillResult:
    """How a photo ended. files: the paths it saved, in the order they were taken (some even when it failed later)."""
    ok: bool
    files: list = field(default_factory=list)
    code: str = ""
    error: str = ""  # in the desktop's language

    @classmethod
    def failed(cls, code: str, english: str = "", files=None) -> "StillResult":
        return cls(False, list(files or []), code, reason_text(code, english))


def free_path(folder: Path, name: str) -> Path:
    """folder/name, or name with (2), (3) and so on before its extension when that's taken (see reserve_path)."""
    stem, dot, ext = name.rpartition(".")
    if not dot:
        stem, ext = name, ""
    path, n = folder / name, 2
    while path.exists() or path.with_name(path.name + ".part").exists():
        path = folder / (f"{stem} ({n}).{ext}" if ext else f"{stem} ({n})")
        n += 1
    return path


def reserve_path(folder: Path, name: str) -> Path:
    """A free name for name in folder, taken by creating it empty so another photo saving at the same time can't."""
    for _ in range(1000):
        path = free_path(folder, name)
        try:
            with open(path, "xb"):
                return path
        except FileExistsError:
            continue  # someone else took it between looking and creating
    raise OSError(f"no free name for {name}")


_SAFE_NAME = re.compile(r"[^A-Za-z0-9_.()-]")


def safe_name(name: str, index: int) -> str:
    """A file name from the phone, kept to plain characters and no folders."""
    name = _SAFE_NAME.sub("_", os.path.basename(str(name)))[:120].lstrip(".")
    return name or f"photo_{index + 1}.jpg"


def exposure_seconds(options: dict) -> float:
    """How long the sensor is open in all for a photo with these /v1/still options (0 for the video's exposure)."""
    try:
        exposure = float(options.get("exposure_ns") or 0) / 1e9
        count = max(1, int(options.get("count") or 1))
        bracket = len([s for s in str(options.get("bracket_ev") or "").split(",") if s.strip()]) or 1
        steps = max(1, int(options.get("focus_steps") or 1))
    except (TypeError, ValueError):
        return 0.0
    return max(0.0, exposure) * count * bracket * steps


class Stills:
    """Takes photos on a stream's phone and saves them, one at a time per phone, each on a thread of its own."""

    def __init__(self):
        self._busy: dict = {}  # source id: the stop event of the photo being taken there
        self._lock = threading.Lock()

    def busy(self, source_id: Optional[str]) -> bool:
        with self._lock:
            return source_id in self._busy

    def take(self, source_id: Optional[str], ctrl, options: dict, folder, on_done: Callable[[StillResult], None],
             on_progress: Optional[Callable[[StillProgress], None]] = None,
             alive: Optional[Callable[[], bool]] = None) -> bool:
        """Start a photo with ctrl (the stream's PhoneControlClient: base and auth); False while one is being taken.
        alive() turning False (its stream stopped or restarted) ends the photo as interrupted."""
        stop = threading.Event()
        with self._lock:
            if source_id in self._busy:
                return False
            self._busy[source_id] = stop

        def work():
            try:
                result = self._run(ctrl, options, Path(folder), stop, on_progress, alive)
            except Exception as exc:  # a bug must still end the photo, or the card waits forever
                logger.exception("Taking a photo failed")
                result = StillResult.failed("failed", str(exc))
            finally:
                with self._lock:
                    self._busy.pop(source_id, None)
            on_done(result)

        threading.Thread(target=work, daemon=True, name="still").start()
        return True

    def shutdown(self):
        """Stop every photo; each tells its phone to forget it on the way out."""
        with self._lock:
            for stop in self._busy.values():
                stop.set()

    # ── On the photo's thread ─────────────────────────────────────────────

    def _request(self, ctrl, path: str, body: Optional[dict] = None) -> dict:
        data = json.dumps(body).encode() if body is not None else None
        headers = dict(ctrl.auth.headers(), **({"Content-Type": "application/json"} if data else {}))
        req = urllib.request.Request(f"{ctrl.base}{path}", data=data, headers=headers, method="POST" if data else "GET")
        with ctrl.auth.open(req, timeout=_REQUEST_TIMEOUT_S) as r:
            reply = json.loads(read_capped(r).decode())
        if not isinstance(reply, dict):
            raise ValueError("not a JSON object")
        return reply

    def _run(self, ctrl, options: dict, folder: Path, stop: threading.Event, on_progress, alive=None) -> StillResult:
        try:
            started = self._request(ctrl, "/still", dict(options, action="take"))
        except urllib.error.HTTPError as exc:
            if exc.code == 404:
                return StillResult.failed("no_photos")  # an older phone has no /v1/still
            return StillResult.failed("failed", str(exc))
        except Exception as exc:
            return StillResult.failed("lost", str(exc))
        if not started.get("ok") or not isinstance(started.get("id"), str):
            return StillResult.failed(str(started.get("code") or "failed"), str(started.get("error") or ""))
        sid = started["id"]
        saved: list = []
        fetched = 0
        heard = begun = moved = time.monotonic()
        last = None  # what the phone said last, to tell a photo that's getting on from one that's stuck
        stall_after = _STALL_S + exposure_seconds(options)
        try:
            folder.mkdir(parents=True, exist_ok=True)
        except OSError as exc:
            logger.warning("Can't make the photo folder %s: %s", folder, exc)
            return StillResult.failed("save")
        while not stop.is_set():
            if alive is not None and not alive():
                # Its stream stopped or restarted: the phone drops the photo with it, if it can still say so at all
                return StillResult.failed("interrupted", "", saved)
            try:
                status = self._request(ctrl, f"/still?id={sid}")
                heard = time.monotonic()
            except urllib.error.HTTPError as exc:
                if alive is not None and not alive():
                    continue
                if exc.code == 404:
                    return StillResult.failed("dropped", "", saved)  # the phone let go of it: its stream stopped, say
                if time.monotonic() - heard > _LOST_AFTER_S:
                    self._forget(ctrl, sid)
                    return StillResult.failed("lost", str(exc), saved)
                stop.wait(_POLL_S)
                continue
            except Exception as exc:
                if alive is not None and not alive():
                    continue
                if time.monotonic() - heard > _LOST_AFTER_S:
                    self._forget(ctrl, sid)  # in case it's only this computer's link that's gone
                    return StillResult.failed("lost", str(exc), saved)
                stop.wait(_POLL_S)
                continue
            files = [f for f in status.get("files") or () if isinstance(f, dict)]
            state = str(status.get("state") or "")
            now = (state, status.get("taken"), len(files), status.get("countdown_ms") if state == "waiting" else None)
            if now != last:
                last, moved = now, time.monotonic()

            def report():
                if on_progress is not None:
                    on_progress(StillProgress(
                        state=state, shots=int(status.get("shots") or 1), taken=int(status.get("taken") or 0),
                        countdown_ms=int(status.get("countdown_ms") or 0), paused=bool(status.get("paused")),
                        saved=list(saved)))
            report()  # before the downloads, which can take a while for a batch of RAW files
            while fetched < len(files) and not stop.is_set():
                path = self._fetch(ctrl, sid, fetched, files[fetched], folder)
                if path is None:
                    self._forget(ctrl, sid)
                    return StillResult.failed("save", "", saved)
                saved.append(str(path))
                fetched += 1
                report()
            if state in ("done", "failed") and fetched >= len(files):
                self._forget(ctrl, sid)
                if state == "failed":
                    return StillResult.failed(str(status.get("code") or "failed"), str(status.get("error") or ""), saved)
                return StillResult(True, saved)
            if time.monotonic() - begun > _LONGEST_S or time.monotonic() - moved > stall_after:
                logger.warning("Photo %s got nowhere on the phone (%s), giving up on it", sid, state)
                self._forget(ctrl, sid)
                return StillResult.failed("timeout", "", saved)
            stop.wait(_POLL_S)
        self._forget(ctrl, sid)  # stopped: Telescope is quitting
        return StillResult.failed("failed", "", saved)

    def _fetch(self, ctrl, sid: str, n: int, info: dict, folder: Path) -> Optional[Path]:
        """File n of the photo into folder under the phone's name (made free), or None when it couldn't be saved."""
        name = safe_name(str(info.get("name") or ""), n)
        req = urllib.request.Request(f"{ctrl.base}/still/file?id={sid}&n={n}", headers=ctrl.auth.headers())
        try:
            path = reserve_path(folder, name)
        except OSError as exc:
            logger.warning("Couldn't save photo %s: %s", name, exc)
            return None
        part = path.with_name(path.name + ".part")
        try:
            with ctrl.auth.open(req, timeout=_FILE_TIMEOUT_S) as r, open(part, "wb") as out:
                while chunk := r.read(_CHUNK):
                    out.write(chunk)
            size = info.get("size")
            if isinstance(size, int) and part.stat().st_size != size:
                raise OSError(f"got {part.stat().st_size} of {size} bytes")
            os.replace(part, path)
            return path
        except Exception as exc:
            logger.warning("Couldn't save photo %s: %s", name, exc)
            for leftover in (part, path):
                try:
                    leftover.unlink()
                except OSError:
                    pass
            return None

    def _forget(self, ctrl, sid: str):
        try:
            self._request(ctrl, "/still", {"action": "forget", "id": sid})
        except Exception as exc:
            logger.debug("The phone didn't forget photo %s: %s", sid, exc)
