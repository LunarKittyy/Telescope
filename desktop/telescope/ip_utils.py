"""Pure IP-address helpers used by device-profile/pairing logic, kept free
of Qt and socket-server code so they're isolated from the connection
plugin's panel/dialog and QR-pairing-server responsibilities.
"""

import socket
from typing import Optional


def get_local_ips() -> list[str]:
    ips: set[str] = set()
    try:
        for info in socket.getaddrinfo(socket.gethostname(), None, socket.AF_INET):
            ip = info[4][0]
            if not ip.startswith("127."):
                ips.add(ip)
    except Exception:
        pass
    try:
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as s:
            s.connect(("8.8.8.8", 80))
            ips.add(s.getsockname()[0])
    except Exception:
        pass
    return sorted(ips, key=rank_ip)


def rank_ip(ip: str) -> int:
    parts = ip.split(".")
    if len(parts) == 4:
        try:
            octets = [int(p) for p in parts]
        except ValueError:
            return 2
        a, b = octets[0], octets[1]
        if a == 100 and 64 <= b <= 127:
            return 0  # Tailscale CGNAT range
        # RFC 1918 private ranges - note 172.16.0.0/12 only, not all of 172.x.x.x
        if a == 10 or a == 192 and b == 168 or a == 172 and 16 <= b <= 31:
            return 1  # LAN
    return 2


def best_ip(ips: list[str]) -> Optional[str]:
    if not ips:
        return None
    return min(ips, key=rank_ip)


def extract_ip(s: str) -> str:
    """Strip protocol/port/path so 'http://1.2.3.4:8080/video' -> '1.2.3.4'."""
    s = s.strip()
    if "://" in s:
        s = s.split("://", 1)[1]
    s = s.split("/")[0]
    s = s.split(":")[0]
    return s.strip()


def valid_ipv4(ip: str) -> bool:
    parts = ip.split(".")
    if len(parts) != 4:
        return False
    try:
        return all(0 <= int(p) <= 255 and str(int(p)) == p for p in parts)
    except ValueError:
        return False
