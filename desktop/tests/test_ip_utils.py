"""Address classification and the pairing candidate list."""

from pathlib import Path
from types import SimpleNamespace

import pytest

import telescope.ip_utils as ip_utils_module
from telescope.ip_utils import rank_ip, valid_ipv4


@pytest.mark.parametrize("ip,expected_rank", [
    ("100.64.0.5", 0),   # Tailscale CGNAT
    ("100.127.255.255", 0),
    ("10.0.0.1", 1),
    ("192.168.1.1", 1),
    ("172.16.0.1", 1),   # RFC 1918 lower bound of 172.16.0.0/12
    ("172.31.255.255", 1),  # RFC 1918 upper bound
    ("172.15.0.1", 2),   # just below the RFC 1918 172.x range - not private
    ("172.32.0.1", 2),   # just above the RFC 1918 172.x range - not private
    ("8.8.8.8", 2),
])
def testrank_ip(ip, expected_rank):
    assert rank_ip(ip) == expected_rank


@pytest.mark.parametrize(
    "ip,valid",
    [
        ("0.0.0.0", True),
        ("255.255.255.255", True),
        ("1.2.3.4", True),
        ("256.2.3.4", False),
        ("01.2.3.4", False),
        ("1.2.3", False),
        ("a.b.c.d", False),
    ],
)
def testvalid_ipv4(ip, valid):
    assert valid_ipv4(ip) is valid


def _fake_adapters(monkeypatch, adapters):
    # Mock ifaddr.get_adapters() with adapter name and IP list pairs.
    fake = [
        SimpleNamespace(
            name=name,
            nice_name=name,
            ips=[SimpleNamespace(ip=ip, is_IPv4=is_v4) for ip, is_v4 in ips],
        )
        for name, ips in adapters
    ]
    monkeypatch.setattr(ip_utils_module.ifaddr, "get_adapters", lambda: fake)


def test_pairing_addresses_order_lan_then_tailscale_then_other(monkeypatch):
    _fake_adapters(monkeypatch, [
        ("tailscale0", [("100.90.12.34", True)]),
        ("eth9", [("203.0.113.7", True)]),
        ("wlan0", [("192.168.1.42", True)]),
        ("eth0", [("10.1.2.3", True)]),
    ])

    addresses = ip_utils_module.get_pairing_addresses()

    assert [(a.ip, a.interface, a.kind) for a in addresses] == [
        # Physical LAN first, in enumeration order within the kind.
        ("192.168.1.42", "wlan0", "lan"),
        ("10.1.2.3", "eth0", "lan"),
        ("100.90.12.34", "tailscale0", "tailscale"),
        ("203.0.113.7", "eth9", "other"),
    ]


def test_pairing_addresses_put_a_vpn_tunnel_behind_the_real_lan(monkeypatch):
    # A desktop VPN handing out an RFC 1918 address classifies as LAN like
    # any other, but the phone can only reach the physical one - so it must
    # not end up ahead of it in the QR code.
    # Only tunnels whose adapter name gives them away get demoted; anything
    # unrecognised keeps its enumeration position, which costs the phone a
    # timeout at worst since it works through every candidate anyway.
    _fake_adapters(monkeypatch, [
        ("tun0", [("10.8.0.6", True)]),
        ("wg0", [("10.2.0.2", True)]),
        ("wlan0", [("192.168.1.42", True)]),
    ])

    addresses = ip_utils_module.get_pairing_addresses()

    assert [a.ip for a in addresses] == ["192.168.1.42", "10.8.0.6", "10.2.0.2"]
    # Still advertised, though: a tunnel is occasionally the only shared path.
    assert all(a.kind == "lan" for a in addresses)


@pytest.mark.parametrize("name,is_vpn", [
    ("tun0", True),
    ("wg0", True),
    ("utun3", True),
    ("NordLynx", True),
    ("ProtonVPN", True),
    ("Tailscale", True),
    ("wlan0", False),
    ("eth0", False),
    ("Wi-Fi", False),
    ("Ethernet 2", False),
])
def test_looks_like_vpn_interface(name, is_vpn):
    assert ip_utils_module.looks_like_vpn_interface(name) is is_vpn


def test_pairing_addresses_cover_every_private_range(monkeypatch):
    _fake_adapters(monkeypatch, [
        ("a", [("192.168.0.1", True)]),
        ("b", [("10.255.255.254", True)]),
        ("c", [("172.16.0.1", True)]),
        ("d", [("172.31.255.254", True)]),
        ("e", [("100.64.0.1", True)]),
        ("f", [("100.127.255.254", True)]),
    ])

    kinds = {a.ip: a.kind for a in ip_utils_module.get_pairing_addresses()}

    assert kinds == {
        "192.168.0.1": "lan",
        "10.255.255.254": "lan",
        "172.16.0.1": "lan",
        "172.31.255.254": "lan",
        "100.64.0.1": "tailscale",
        "100.127.255.254": "tailscale",
    }


def test_pairing_addresses_skip_loopback_link_local_ipv6_and_duplicates(monkeypatch):
    _fake_adapters(monkeypatch, [
        ("lo", [("127.0.0.1", True)]),
        ("wlan0", [
            ("169.254.10.11", True),          # link-local: no DHCP lease
            (("fe80::1", 0, 0), False),       # ifaddr reports IPv6 as a tuple
            ("192.168.1.42", True),
        ]),
        ("wlan0:1", [("192.168.1.42", True)]),  # same address, aliased adapter
    ])

    addresses = ip_utils_module.get_pairing_addresses()

    assert [(a.ip, a.interface) for a in addresses] == [("192.168.1.42", "wlan0")]


def test_pairing_addresses_skip_container_and_vm_only_adapters(monkeypatch):
    _fake_adapters(monkeypatch, [
        ("docker0", [("172.17.0.1", True)]),
        ("br-1a2b3c", [("172.18.0.1", True)]),
        ("veth3f9a", [("172.19.0.1", True)]),
        ("virbr0", [("192.168.122.1", True)]),
        ("vboxnet0", [("192.168.56.1", True)]),
        ("VMware Network Adapter VMnet8", [("192.168.75.1", True)]),
        ("VirtualBox Host-Only Network", [("192.168.99.1", True)]),
        ("Wi-Fi", [("192.168.1.42", True)]),
    ])

    assert [a.ip for a in ip_utils_module.get_pairing_addresses()] == ["192.168.1.42"]


def test_pairing_addresses_keeps_hyper_v_bridged_lan_adapter(monkeypatch):
    # Windows bridges a Hyper-V host's real LAN connection through an adapter
    # named "vEthernet (...)" - dropping those would strip the only address
    # the phone can reach on such a machine.
    _fake_adapters(monkeypatch, [("vEthernet (External)", [("192.168.1.42", True)])])

    assert [a.ip for a in ip_utils_module.get_pairing_addresses()] == ["192.168.1.42"]


def test_pairing_addresses_are_capped_and_keep_the_best_ones(monkeypatch):
    # Everything here goes into a QR code the phone has to read off a screen.
    _fake_adapters(monkeypatch, [
        *[(f"tailscale{i}", [(f"100.64.0.{i}", True)]) for i in range(8)],
        ("wlan0", [("192.168.1.42", True)]),
    ])

    addresses = ip_utils_module.get_pairing_addresses()

    assert len(addresses) == ip_utils_module.MAX_PAIRING_CANDIDATES == 8
    # The LAN address survives the cap even though it was enumerated last.
    assert addresses[0].ip == "192.168.1.42"


def test_pairing_addresses_trim_very_long_interface_names(monkeypatch):
    _fake_adapters(monkeypatch, [("Intel(R) Wi-Fi 6E AX211 160MHz Adapter #2", [("192.168.1.42", True)])])

    assert ip_utils_module.get_pairing_addresses()[0].interface == "Intel(R) Wi-Fi 6E AX211 160MHz A"


def test_pairing_addresses_tolerate_enumeration_failure(monkeypatch):
    monkeypatch.setattr(
        ip_utils_module.ifaddr,
        "get_adapters",
        lambda: (_ for _ in ()).throw(OSError()),
    )
    assert ip_utils_module.get_pairing_addresses() == []


@pytest.mark.parametrize("ip,kind", [
    ("192.168.1.1", "lan"),
    ("10.0.0.1", "lan"),
    ("172.16.0.1", "lan"),
    ("172.15.0.1", "other"),   # just below the RFC 1918 172.x range
    ("172.32.0.1", "other"),   # just above it
    ("100.64.0.1", "tailscale"),
    ("100.63.255.255", "other"),  # just below the CGNAT block
    ("100.128.0.0", "other"),     # just above it
    ("8.8.8.8", "other"),
    ("127.0.0.1", None),
    ("169.254.1.1", None),
    ("224.0.0.1", None),
    ("0.0.0.0", None),
    ("::1", None),
    ("not-an-ip", None),
])
def test_classify_ip(ip, kind):
    assert ip_utils_module.classify_ip(ip) == kind


def test_no_route_probe_towards_a_public_address_remains():
    # Route probe reports default route's interface (wrong under VPN); this design avoids it. UTF-8 encoding needed for box-drawing chars.
    sources = Path(ip_utils_module.__file__).resolve().parent.rglob("*.py")
    offenders = [p.name for p in sources if "8.8.8.8" in p.read_text(encoding="utf-8")]
    assert offenders == []
