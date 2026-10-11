import json
import logging
import queue
import threading
import time
import urllib.error
import urllib.request
from typing import Optional

from telescope.pinned_https import PhoneAuth
from telescope.session_client import read_capped

logger = logging.getLogger(__name__)

# A request lost on a stalling link is sent again, a little later each time, for this long. Every action sets a value,
# so one that did arrive and only lost its reply does no harm arriving twice.
_RETRY_FOR_S = 15.0
_RETRY_FIRST_WAIT_S = 0.25
_RETRY_MAX_WAIT_S = 2.0


def drifted_settings(settings: dict, state: dict) -> list:
    """The sent settings (action: params, in the order sent) that the phone's /v1/state doesn't show."""
    if "cameras" not in state:
        return []  # not a whole state
    order = list(settings)
    out = []

    def after(a: str, b: str) -> bool:
        return b not in settings or order.index(a) > order.index(b)

    def check(action: str, matches) -> None:
        params = settings.get(action)
        if params is None:
            return
        try:
            ok = matches(params.get("value"))
        except (TypeError, ValueError):
            ok = True  # a state this can't read says nothing either way
        if not ok:
            out.append(params)

    iso, shutter = settings.get("iso"), settings.get("shutter")
    if iso and shutter and after("iso", "auto") and after("shutter", "auto"):
        try:
            kept = (not state.get("auto", True) and int(state.get("iso") or 0) == int(iso.get("value"))
                    and int(state.get("shutter_ns") or 0) == int(shutter.get("value")))
        except (TypeError, ValueError):
            kept = True
        if not kept:
            out += [iso, shutter]
    elif "auto" in settings and not state.get("auto", True):
        out.append(settings["auto"])
    if "wb_gains" in settings and after("wb_gains", "wb_auto"):
        check("wb_gains", lambda _v: bool(state.get("wb_manual")))
    else:
        check("wb_auto", lambda _v: not state.get("wb_manual"))
    focus = state.get("focus_mode")
    if focus in ("continuous", "manual"):  # "point" is picked on the preview, not sent as a mode
        check("focus_mode", lambda v: v == focus)
        if focus == "manual":
            check("focus_distance", lambda v: abs(float(state.get("focus_distance", 0.0)) - float(v)) < 0.01)
    if state.get("auto", True):  # exposure compensation only applies to auto exposure
        check("ae_comp", lambda v: int(state.get("ae_comp", 0)) == int(v))
    check("fps_target", lambda v: int(state.get("phone_fps", 0)) == int(v))
    check("jpeg_quality", lambda v: int(state.get("jpeg_quality", 0)) == int(v))
    check("ois", lambda v: bool(state.get("ois", True)) == (str(v) == "1"))
    check("black_level_lock", lambda v: bool(state.get("black_level_lock", False)) == (str(v) == "1"))
    check("nr_mode", lambda v: int(state.get("nr_mode", 1)) == int(v))
    check("edge_mode", lambda v: int(state.get("edge_mode", 1)) == int(v))
    if state.get("low_light_toggle"):  # an older phone doesn't know it: sending it again wouldn't change that
        check("low_light", lambda v: bool(state.get("low_light", False)) == (str(v) == "1"))
    return out


class PhoneControlClient:
    """Sends authenticated camera-control requests via queued background worker; coalesces requests by action."""

    _NON_COALESCING = frozenset({"camera"})
    # What a phone that restarted its stream has forgotten, and gets again from resend_settings()
    _SETTINGS = frozenset({
        "auto", "iso", "shutter", "wb_auto", "wb_gains", "ois", "focus_mode", "focus_distance", "ae_comp", "nr_mode",
        "edge_mode", "black_level_lock", "jpeg_quality", "bitrate", "fps_target", "zoom", "low_light",
    })

    def __init__(self, stream_url: str, auth: PhoneAuth):
        self.retarget(stream_url)
        self.auth = auth
        self._queue: "queue.Queue" = queue.Queue()
        self._pending: dict = {}  # action: (place in the queue, params), the latest of each
        self._seq = 0
        self._settings: dict = {}  # the last of each setting sent, in the order they were last sent
        self._resent: dict = {}  # action: params sent again because the phone's state didn't show them, once each
        self._lock = threading.Lock()
        self._closed = False
        self._wake = threading.Event()  # set by close(), to end a wait between tries
        self._thread = threading.Thread(target=self._worker, daemon=True)
        self._thread.start()

    def _auth_headers(self) -> dict:
        return self.auth.headers()

    def get_state(self) -> Optional[dict]:
        try:
            req = urllib.request.Request(f"{self.base}/state", headers=self._auth_headers())
            with self.auth.open(req, timeout=4) as r:
                state = json.loads(read_capped(r).decode())
        except Exception:
            return None
        if not isinstance(state, dict):
            return None  # every caller reads it with .get
        self._resync(state)
        return state

    def _resync(self, state: dict):
        """Sends a setting again when the phone's state doesn't show it; once per value, as the phone may clamp it."""
        with self._lock:
            settings = dict(self._settings)
            stale = [p for p in drifted_settings(settings, state)
                     if p["action"] not in self._pending and self._resent.get(p["action"]) != p]
        for params in stale:
            logger.info("The phone doesn't show the %s sent, sending it again", params["action"])
            self.send(**params)
            with self._lock:
                self._resent[params["action"]] = params

    def send(self, **params):
        if self._closed:
            return
        action = params.get("action")
        with self._lock:
            self._resent.pop(action, None)
            if action in self._SETTINGS:
                self._settings.pop(action, None)
                self._settings[action] = params
            if action in self._NON_COALESCING:
                self._queue.put(params)
            else:
                # Goes out at the latest send's place, after whatever was sent between (Manual, Auto, Manual: Manual)
                self._seq += 1
                self._pending[action] = (self._seq, params)
                self._queue.put((action, self._seq))

    def retarget(self, stream_url: str):
        """The same stream on a new route (cable pulled: Wi-Fi): what's queued, and what resend_settings() sends,
        goes there."""
        self.base = stream_url.rsplit("/video", 1)[0]

    def resend_settings(self):
        """Send the phone every setting again, for a stream that came back while the panels showed another."""
        with self._lock:
            settings = list(self._settings.values())
        for params in settings:
            self.send(**params)

    def close(self):
        """Stop accepting requests and cancel queued ones (device switch cleanup)."""
        if self._closed:
            return
        self._closed = True
        self._wake.set()
        with self._lock:
            self._pending.clear()
        try:
            while True:
                self._queue.get_nowait()
        except queue.Empty:
            pass
        self._queue.put(None)

    def _worker(self):
        while True:
            item = self._queue.get()
            if item is None:
                return
            if isinstance(item, dict):
                params = item
            else:
                action, seq = item
                with self._lock:
                    queued = self._pending.get(action)
                    if queued is None or queued[0] != seq:
                        continue  # sent already, or a newer value waits further back
                    params = self._pending.pop(action)[1]
            self._deliver(params)

    def _deliver(self, params: dict):
        """Sends until it gets through, the phone refuses it, a newer value for the same action is waiting, or time's up."""
        action = params.get("action")
        deadline = time.monotonic() + _RETRY_FOR_S
        wait = _RETRY_FIRST_WAIT_S
        while not self._send_now(params):
            with self._lock:
                superseded = action in self._pending
            if self._closed or superseded:
                return
            if time.monotonic() + wait > deadline:
                logger.warning("Gave up sending %s to the phone", action)
                return
            if self._wake.wait(wait):
                return  # closed: this phone, or this session with it, is done
            wait = min(wait * 2, _RETRY_MAX_WAIT_S)

    def _send_now(self, params: dict) -> bool:
        """False when it may not have got there, so it's worth sending again."""
        body = json.dumps(params).encode("utf-8")
        headers = {**self._auth_headers(), "Content-Type": "application/json"}
        req = urllib.request.Request(f"{self.base}/control", data=body, method="POST", headers=headers)
        try:
            with self.auth.open(req, timeout=3) as r:
                r.read()
        except urllib.error.HTTPError as exc:
            logger.debug("The phone refused a control request: %s", exc)  # it got there: again won't change that
        except Exception as exc:
            logger.debug("Control request failed: %s", exc)
            return False
        return True
