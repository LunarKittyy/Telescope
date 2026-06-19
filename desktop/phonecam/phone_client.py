import json
import threading
import urllib.request
from typing import Optional


class PhoneControlClient:
    def __init__(self, stream_url: str):
        self.base = stream_url.rsplit("/video", 1)[0]

    def get_state(self) -> Optional[dict]:
        try:
            r = urllib.request.urlopen(f"{self.base}/cameras", timeout=4)
            return json.loads(r.read().decode())
        except Exception:
            return None

    def send(self, **params):
        qs  = "&".join(f"{k}={v}" for k, v in params.items())
        url = f"{self.base}/control?{qs}"
        threading.Thread(target=self._req, args=(url,), daemon=True).start()

    def _req(self, url):
        try: urllib.request.urlopen(url, timeout=3)
        except Exception: pass
