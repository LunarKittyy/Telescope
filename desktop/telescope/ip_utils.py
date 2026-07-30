"""Pure IP-address helpers used by device-profile/pairing logic, kept free
of Qt and socket-server code so they're isolated from the connection
plugin's panel/dialog and QR-pairing-server responsibilities.
"""

import ipaddress
from dataclasses import dataclass
from typing import Literal, Optional

import ifaddr

# Tailscale hands out addresses from the CGNAT block.
_TAILSCALE_NET = ipaddress.ip_network("100.64.0.0/10")
_PRIVATE_NETS = (
    ipaddress.ip_network("192.168.0.0/16"),
    ipaddress.ip_network("10.0.0.0/8"),
    ipaddress.ip_network("172.16.0.0/12"),
)

AddressKind = Literal["lan", "tailscale", "other"]

# Ordering the phone should try candidates in: a physical LAN path is the
# fastest and the one most likely to survive a VPN that still permits
# local-network traffic; Tailscale works across networks but only when both
# ends are on the tailnet; anything else is a long shot we still advertise
# rather than silently drop.
_KIND_ORDER: dict[str, int] = {"lan": 0, "tailscale": 1, "other": 2}

# Adapters that exist only to talk to containers/VMs on this same machine.
# A phone can never reach the desktop through one of these, so advertising
# them just burns a connection timeout on the phone before it gets to a
# candidate that can work. Matched case-insensitively against the adapter
# name; deliberately narrow, because over-matching would drop a real LAN
# address (Windows bridges a Hyper-V host's actual LAN through an adapter
# named "vEthernet (...)", so that prefix is *not* in here).
_VIRTUAL_PREFIXES = (
    "docker",
    "br-",
    "veth",
    "virbr",
    "vboxnet",
    "vmnet",
)
_VIRTUAL_SUBSTRINGS = (
    "virtualbox host-only",
    "vmware network adapter",
)

# Tunnel interfaces. Unlike the virtual adapters above these are kept - a
# VPN's own address is occasionally the only path between two devices - but
# they're tried after physical adapters in the same class, because a desktop
# VPN handing out an RFC 1918 address (which classifies as "lan") otherwise
# pushes the real LAN address down the phone's list and costs it a
# connection timeout to find out.
_VPN_PREFIXES = ("tun", "tap", "wg", "utun", "ppp", "ipsec", "nordlynx")
_VPN_SUBSTRINGS = ("vpn", "wireguard", "tailscale")

# Everything here ends up inside a QR code the phone has to read with its
# camera, and every extra byte adds modules to the matrix. A machine with a
# dozen adapters would otherwise produce a code that's dense enough to be
# hard to scan and a candidate list that takes the phone a connection
# timeout each to work through - so keep the best few, with names trimmed to
# something still recognisable in the dialog.
MAX_PAIRING_CANDIDATES = 8
_MAX_INTERFACE_NAME_LEN = 32


@dataclass(frozen=True)
class PairingAddress:
    """One address the phone can try to reach this desktop on, with enough
    context to explain it in the pairing dialog and to let the phone decide
    which network to route the attempt over."""

    ip: str
    interface: str
    kind: AddressKind


def classify_ip(ip: str) -> Optional[AddressKind]:
    """The kind of pairing candidate [ip] is, or None if it's an address no
    phone could usefully pair over (loopback, link-local, multicast, IPv6,
    or malformed)."""
    try:
        addr = ipaddress.ip_address(ip)
    except ValueError:
        return None
    if not isinstance(addr, ipaddress.IPv4Address):
        return None
    if addr.is_loopback or addr.is_link_local or addr.is_multicast or addr.is_unspecified:
        return None
    if addr in _TAILSCALE_NET:
        return "tailscale"
    if any(addr in net for net in _PRIVATE_NETS):
        return "lan"
    return "other"


def is_virtual_interface(name: str) -> bool:
    """True for adapters that only reach containers/VMs on this machine."""
    lowered = name.strip().lower()
    # Windows names a Hyper-V/WSL virtual switch "vEthernet (...)", and the
    # host's own LAN connection is bridged through one when Hyper-V is
    # enabled - so despite sharing a prefix with Linux's container-side
    # "veth<hex>" pipes, these are kept.
    if lowered.startswith("vethernet"):
        return False
    return (
        lowered.startswith(_VIRTUAL_PREFIXES)
        or any(s in lowered for s in _VIRTUAL_SUBSTRINGS)
    )


def looks_like_vpn_interface(name: str) -> bool:
    """True for tunnel adapters - kept as candidates, but tried last within
    their class."""
    lowered = name.strip().lower()
    return (
        lowered.startswith(_VPN_PREFIXES)
        or any(s in lowered for s in _VPN_SUBSTRINGS)
    )


def get_pairing_addresses() -> list[PairingAddress]:
    """Every address the phone could plausibly reach this desktop on, best
    candidate first.

    Enumerates the machine's actual network adapters rather than asking the
    routing table where a public IP would go: a UDP "route probe" towards a
    well-known public DNS resolver reports whichever interface currently
    owns the default route, which under a VPN is the VPN's - so the one
    address the phone can really reach (the physical LAN one) would be
    missing from the QR code exactly when it's needed most. Hostname
    resolution has the mirror-image problem: it reports whatever /etc/hosts
    or DNS says, which may be nothing, or stale, on a network with no
    internet at all."""
    candidates: list[PairingAddress] = []
    seen: set[str] = set()
    try:
        adapters = ifaddr.get_adapters()
    except Exception:
        return []
    for adapter in adapters:
        name = adapter.nice_name or adapter.name
        if isinstance(name, bytes):
            name = name.decode(errors="replace")
        if is_virtual_interface(name):
            continue
        name = name.strip()[:_MAX_INTERFACE_NAME_LEN]
        for addr in adapter.ips:
            if not addr.is_IPv4:
                continue
            ip = str(addr.ip)
            kind = classify_ip(ip)
            if kind is None or ip in seen:
                continue
            seen.add(ip)
            candidates.append(PairingAddress(ip=ip, interface=name, kind=kind))
    # Stable sort: within a kind (and once tunnels are pushed behind
    # physical adapters), the order the OS reported the adapters in is as
    # good a guess at "the primary one" as anything we could invent.
    candidates.sort(
        key=lambda c: (_KIND_ORDER[c.kind], looks_like_vpn_interface(c.interface))
    )
    return candidates[:MAX_PAIRING_CANDIDATES]


def describe_address(addr: PairingAddress) -> str:
    """One line for the pairing dialog's "waiting for the phone on" list."""
    if addr.kind == "tailscale":
        return f"{addr.ip} · Tailscale"
    if addr.kind == "lan":
        return f"{addr.ip} · {addr.interface}/LAN"
    return f"{addr.ip} · {addr.interface}"


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
