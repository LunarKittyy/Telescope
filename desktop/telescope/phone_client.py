import json
import logging
import queue
import threading
import urllib.request
from typing import Optional

logger = logging.getLogger(__name__)


class PhoneControlClient:
    """Sends authenticated camera-control requests via queued background worker; coalesces requests by action."""

    _NON_COALESCING = frozenset({"camera"})

    def __init__(self, stream_url: str, token: str):
        self.base = stream_url.rsplit("/video", 1)[0]
        self.token = token
        self._queue: "queue.Queue" = queue.Queue()
        self._pending: dict = {}
        self._lock = threading.Lock()
        self._closed = False
        self._thread = threading.Thread(target=self._worker, daemon=True)
        self._thread.start()

    def _auth_headers(self) -> dict:
        return {"Authorization": f"Bearer {self.token}"}

    def get_state(self) -> Optional[dict]:
        try:
            req = urllib.request.Request(f"{self.base}/state", headers=self._auth_headers())
            with urllib.request.urlopen(req, timeout=4) as r:
                return json.loads(r.read().decode())
        except Exception:
            return None

    def send(self, **params):
        if self._closed:
            return
        action = params.get("action")
        with self._lock:
            if action in self._NON_COALESCING:
                self._queue.put(params)
            else:
                is_new = action not in self._pending
                self._pending[action] = params
                if is_new:
                    self._queue.put(action)

    def close(self):
        """Stop accepting requests and cancel queued ones (device switch cleanup)."""
        if self._closed:
            return
        self._closed = True
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
                with self._lock:
                    params = self._pending.pop(item, None)
                if params is None:
                    continue
            self._send_now(params)

    def _send_now(self, params: dict):
        body = json.dumps(params).encode("utf-8")
        headers = {**self._auth_headers(), "Content-Type": "application/json"}
        req = urllib.request.Request(f"{self.base}/control", data=body, method="POST", headers=headers)
        try:
            with urllib.request.urlopen(req, timeout=3) as r:
                r.read()
        except Exception as exc:
            logger.debug("Control request failed: %s", exc)
