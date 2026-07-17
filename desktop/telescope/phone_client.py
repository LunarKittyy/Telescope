import json
import logging
import queue
import threading
import urllib.parse
import urllib.request
from typing import Optional

logger = logging.getLogger(__name__)


class PhoneControlClient:
    """Sends camera-control requests to the phone.

    A single background worker thread sends queued requests in order, so a
    burst of slider drags can never have an older request's response arrive
    after a newer one (each call previously spawned its own daemon thread,
    with no guarantee they'd complete in the order they were sent). Requests
    that share the same ``action`` are coalesced to just the latest value
    while they're still waiting to be sent; actions in ``_NON_COALESCING``
    (currently just camera switches) are always sent individually and in
    order instead.
    """

    _NON_COALESCING = frozenset({"camera"})

    def __init__(self, stream_url: str):
        self.base = stream_url.rsplit("/video", 1)[0]
        self._queue: "queue.Queue" = queue.Queue()
        self._pending: dict = {}
        self._lock = threading.Lock()
        self._closed = False
        self._thread = threading.Thread(target=self._worker, daemon=True)
        self._thread.start()

    def get_state(self) -> Optional[dict]:
        try:
            with urllib.request.urlopen(f"{self.base}/cameras", timeout=4) as r:
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
        """Stop accepting new requests and let the worker thread drain."""
        if self._closed:
            return
        self._closed = True
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
        qs = urllib.parse.urlencode(params)
        url = f"{self.base}/control?{qs}"
        try:
            with urllib.request.urlopen(url, timeout=3) as r:
                r.read()
        except Exception as exc:
            logger.debug("Control request failed: %s", exc)
