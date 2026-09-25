"""Finds paired phones on the LAN by the _telescope._tcp announcement their app makes (mDNS/DNS-SD).

The announcement's TXT "id" is the phone id; the address is only a hint, since every connection still
authenticates. Without the zeroconf package, or on a network that drops multicast, lookups return
nothing and the stored addresses are used as before.
"""

import logging
import socket
import threading

logger = logging.getLogger(__name__)

SERVICE_TYPE = "_telescope._tcp.local."


class LanDiscovery:
    def __init__(self):
        self._lock = threading.Lock()
        self._by_id: dict = {}      # phone id -> [ipv4, ...]
        self._by_name: dict = {}    # service name -> phone id, to forget a phone when it leaves
        self._zc = None
        self._browser = None
        self._unavailable = False  # tried and failed; don't retry (and re-log) on every status poll

    def start(self):
        if self._zc is not None or self._unavailable:
            return
        try:
            from zeroconf import ServiceBrowser, Zeroconf
        except ImportError:
            logger.info("zeroconf not installed; phones are reached on their stored addresses only")
            self._unavailable = True
            return
        try:
            self._zc = Zeroconf()
            self._browser = ServiceBrowser(self._zc, SERVICE_TYPE, handlers=[self._on_change])
        except Exception:
            logger.warning("LAN discovery unavailable", exc_info=True)
            self._zc = None
            self._unavailable = True

    def stop(self):
        zc, self._zc = self._zc, None
        if zc is not None:
            try:
                zc.close()
            except Exception:
                pass

    def lookup(self, phone_id: str) -> list:
        with self._lock:
            return list(self._by_id.get(phone_id, []))

    def _on_change(self, zeroconf, service_type, name, state_change):
        from zeroconf import ServiceStateChange
        if state_change is ServiceStateChange.Removed:
            with self._lock:
                pid = self._by_name.pop(name, None)
                if pid:
                    self._by_id.pop(pid, None)
            return
        info = zeroconf.get_service_info(service_type, name, timeout=2000)
        if info is None:
            return
        raw_id = (info.properties or {}).get(b"id")
        if not raw_id:
            return
        pid = raw_id.decode("utf-8", "replace")
        ips = [socket.inet_ntoa(a) for a in info.addresses if len(a) == 4]
        with self._lock:
            self._by_name[name] = pid
            self._by_id[pid] = ips
