"""Paired phones and how to reach them. Qt-free: adb, the session client and LAN discovery are injected.

Routing rule (the one users feel): every resolve starts from scratch and prefers USB whenever *this*
phone answers over a cable. "Answers" is verified, not assumed: a plugged-in device only counts once
its /v1/hello reports the paired phone's id. Whenever the result isn't USB, the resolution says why,
so the UI can show it next to the route instead of leaving the user to guess.
"""

import concurrent.futures
import threading
from dataclasses import dataclass, field, replace
from typing import Callable, Optional

from telescope import ip_utils
from telescope.session_client import PING_PORT, PhoneSessionClient

STREAM_PORT = 8080  # the phone's MJPEG server; fixed on the phone, so not a setting here

ROUTE_AUTO, ROUTE_USB, ROUTE_WIFI = "auto", "usb", "wifi"

# Why USB wasn't used (or failed, when it was forced).
USB_NO_ADB = "no_adb"                # adb isn't installed
USB_NO_CABLE = "no_cable"            # adb sees no device at all
USB_UNAUTHORIZED = "unauthorized"    # a device is attached but hasn't accepted USB debugging
USB_OTHER_PHONE = "other_phone"      # a device answered, but it's a different phone
USB_APP_CLOSED = "app_closed"        # a device is attached but Telescope isn't answering on it

# Overall state of a phone.
READY = "ready"
UNREACHABLE = "unreachable"            # the app isn't open, or the phone isn't on this network
NOT_PAIRED = "not_paired"              # the phone answered but no longer accepts this computer
LOCAL_ONLY = "local_only"              # found over Wi-Fi, but the phone only allows USB
USB_NEEDS_ATTENTION = "usb_attention"  # USB forced and not usable; usb_note says why


@dataclass
class Phone:
    id: str
    name: str
    token: str
    ips: list = field(default_factory=list)
    active_ip: Optional[str] = None

    def to_dict(self) -> dict:
        return {"id": self.id, "name": self.name, "token": self.token,
                "ips": list(self.ips), "active_ip": self.active_ip}

    @classmethod
    def from_dict(cls, raw) -> "Phone":
        if not isinstance(raw, dict):
            raise ValueError("phone entry is not an object")
        pid, name, token = raw.get("id"), raw.get("name"), raw.get("token")
        if not all(isinstance(v, str) and v for v in (pid, name, token)):
            raise ValueError("phone entry needs id, name and token")
        ips = raw.get("ips", [])
        if not isinstance(ips, list) or not all(isinstance(ip, str) for ip in ips):
            raise ValueError("phone entry 'ips' must be a list of strings")
        active = raw.get("active_ip")
        return cls(pid, name, token, ips, active if isinstance(active, str) else None)


@dataclass(frozen=True)
class Route:
    kind: str                     # ROUTE_USB or ROUTE_WIFI
    host: str                     # device IP for Wi-Fi; unused for USB (reached through adb forwards)
    serial: Optional[str] = None  # adb serial for USB


@dataclass(frozen=True)
class Resolution:
    status: str
    route: Optional[Route] = None
    usb_note: Optional[str] = None  # set whenever the route isn't USB (or USB failed)
    streaming: bool = False
    busy: bool = False


class UsbTunnels:
    """Refcounted `adb forward tcp:0 tcp:<port>` per (serial, port).

    The status probe, a phone wake and a remote stop can overlap; each one closing "its" forward used
    to tear down the tunnel another was still using. tcp:0 lets adb pick a free local port, so probing
    two plugged-in devices doesn't make them fight over one local port either.
    """

    def __init__(self, forward: Callable, unforward: Callable):
        self._forward = forward      # (serial, remote_port) -> local_port or None
        self._unforward = unforward  # (serial, local_port) -> None
        self._lock = threading.Lock()
        self._held: dict = {}        # (serial, remote) -> [local_port, refs]

    def acquire(self, serial: str, remote_port: int) -> Optional[int]:
        with self._lock:
            held = self._held.get((serial, remote_port))
            if held:
                held[1] += 1
                return held[0]
            local = self._forward(serial, remote_port)
            if local is None:
                return None
            self._held[(serial, remote_port)] = [local, 1]
            return local

    def release(self, serial: str, remote_port: int):
        with self._lock:
            held = self._held.get((serial, remote_port))
            if not held:
                return
            held[1] -= 1
            if held[1] > 0:
                return
            del self._held[(serial, remote_port)]
            self._unforward(serial, held[0])


class RouteResolver:
    """Decides how to reach a paired phone right now. Blocking: call off the GUI thread."""

    def __init__(self, *, adb_available: Callable[[], bool], adb_device_states: Callable[[], list],
                 tunnels: UsbTunnels, discover: Callable[[str], list],
                 client_factory: Callable = PhoneSessionClient, wifi_timeout: float = 1.5):
        self._adb_available = adb_available
        self._adb_device_states = adb_device_states
        self._tunnels = tunnels
        self._discover = discover          # phone id -> current LAN addresses from mDNS
        self._client = client_factory
        self._wifi_timeout = wifi_timeout

    def resolve(self, phone: Phone, preference: str = ROUTE_AUTO) -> Resolution:
        usb_note = None
        if preference in (ROUTE_AUTO, ROUTE_USB):
            usb, usb_note = self._try_usb(phone)
            if usb is not None:
                return usb
            if preference == ROUTE_USB:
                return Resolution(USB_NEEDS_ATTENTION, usb_note=usb_note)
        wifi = self._try_wifi(phone)
        return replace(wifi, usb_note=usb_note if preference == ROUTE_AUTO else None)

    # ── USB ──────────────────────────────────────────────────────────────

    def _try_usb(self, phone: Phone):
        if not self._adb_available():
            return None, USB_NO_ADB
        states = self._adb_device_states()
        if not states:
            return None, USB_NO_CABLE
        note = USB_UNAUTHORIZED if any(state == "unauthorized" for _s, state in states) else None
        saw_other = False
        for serial, state in states:
            if state != "device":
                continue
            local = self._tunnels.acquire(serial, PING_PORT)
            if local is None:
                continue
            try:
                client = self._client(f"http://127.0.0.1:{local}", phone.token)
                hello = client.hello()
                if hello is None:
                    continue
                if hello[0] != phone.id:
                    saw_other = True
                    continue
                ping = client.ping()
                if ping.status == "not_paired":
                    return Resolution(NOT_PAIRED), None
                if ping.status == "paired":
                    return Resolution(READY, Route("usb", "127.0.0.1", serial),
                                      streaming=bool(ping.streaming), busy=bool(ping.busy)), None
            finally:
                self._tunnels.release(serial, PING_PORT)
        if note:
            return None, note
        return None, USB_OTHER_PHONE if saw_other else USB_APP_CLOSED

    # ── Wi-Fi ────────────────────────────────────────────────────────────

    def _wifi_candidates(self, phone: Phone) -> list:
        ordered = list(self._discover(phone.id)) + ([phone.active_ip] if phone.active_ip else [])
        ordered += sorted(phone.ips, key=ip_utils.rank_ip)
        seen, out = set(), []
        for ip in ordered:
            if ip and ip not in seen and ip_utils.valid_ipv4(ip):
                seen.add(ip)
                out.append(ip)
        return out

    def _probe_wifi(self, phone: Phone, ip: str) -> Optional[Resolution]:
        client = self._client(f"http://{ip}:{PING_PORT}", phone.token)
        hello = client.hello(timeout=self._wifi_timeout)
        if hello is None or hello[0] != phone.id:
            return None
        ping = client.ping()
        if ping.status == "not_paired":
            return Resolution(NOT_PAIRED)
        if ping.status != "paired":
            return None
        if ping.local_only:
            return Resolution(LOCAL_ONLY)
        return Resolution(READY, Route("wifi", ip), streaming=bool(ping.streaming), busy=bool(ping.busy))

    def _try_wifi(self, phone: Phone) -> Resolution:
        candidates = self._wifi_candidates(phone)
        if not candidates:
            return Resolution(UNREACHABLE)
        results = {}
        with concurrent.futures.ThreadPoolExecutor(max_workers=min(4, len(candidates))) as pool:
            futures = {pool.submit(self._probe_wifi, phone, ip): ip for ip in candidates}
            for fut in concurrent.futures.as_completed(futures):
                try:
                    results[futures[fut]] = fut.result()
                except Exception:
                    results[futures[fut]] = None
        # Candidate order decides between several answers (discovered address first).
        answers = [results[ip] for ip in candidates if results.get(ip) is not None]
        for wanted in (READY, LOCAL_ONLY, NOT_PAIRED):
            for answer in answers:
                if answer.status == wanted:
                    return answer
        return Resolution(UNREACHABLE)


def usb_note_text(note: Optional[str]) -> str:
    """Why the connection isn't using USB, in the words the UI shows."""
    return {
        USB_NO_ADB: "adb isn't installed, so USB can't be used",
        USB_NO_CABLE: "no phone plugged in over USB",
        USB_UNAUTHORIZED: "phone plugged in, but USB debugging isn't allowed yet: accept the prompt on the phone",
        USB_OTHER_PHONE: "the phone plugged in over USB is a different one",
        USB_APP_CLOSED: "phone plugged in, but Telescope isn't open on it",
    }.get(note, "")
