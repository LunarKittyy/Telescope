//! This computer's addresses a phone could pair over, and the address helpers routes use (ip_utils.py).

use serde::Serialize;
use std::net::{IpAddr, Ipv4Addr};

/// One address the phone can try to reach this desktop on, with what the pairing dialog and the phone need to pick.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PairingAddress {
    pub ip: String,
    pub interface: String,
    /// "lan", "tailscale" or "other".
    pub kind: &'static str,
}

const VIRTUAL_PREFIXES: &[&str] = &["docker", "br-", "veth", "virbr", "vboxnet", "vmnet"];
const VIRTUAL_SUBSTRINGS: &[&str] = &["virtualbox host-only", "vmware network adapter"];
const VPN_PREFIXES: &[&str] = &["tun", "tap", "wg", "utun", "ppp", "ipsec", "nordlynx"];
const VPN_SUBSTRINGS: &[&str] = &["vpn", "wireguard", "tailscale"];
/// QR code size matters: keep the list short and the names trimmed.
pub const MAX_PAIRING_CANDIDATES: usize = 8;
const MAX_INTERFACE_NAME_LEN: usize = 32;

/// The kind of pairing candidate ip is, or None if no phone could usefully pair over it.
pub fn classify_ip(ip: &str) -> Option<&'static str> {
    let Ok(IpAddr::V4(addr)) = ip.parse::<IpAddr>() else { return None };
    classify(addr)
}

pub fn classify(ip: Ipv4Addr) -> Option<&'static str> {
    if ip.is_loopback() || ip.is_link_local() || ip.is_multicast() || ip.is_unspecified() {
        return None;
    }
    let o = ip.octets();
    if o[0] == 100 && (64..128).contains(&o[1]) {
        return Some("tailscale");
    }
    Some(if ip.is_private() { "lan" } else { "other" })
}

pub fn is_virtual_interface(name: &str) -> bool {
    let lowered = name.trim().to_lowercase();
    // vEthernet is a real interface (Hyper-V bridge), not a container adapter
    if lowered.starts_with("vethernet") {
        return false;
    }
    VIRTUAL_PREFIXES.iter().any(|p| lowered.starts_with(p)) || VIRTUAL_SUBSTRINGS.iter().any(|s| lowered.contains(s))
}

/// Tunnel adapters: kept as candidates, but tried last within their kind.
pub fn looks_like_vpn_interface(name: &str) -> bool {
    let lowered = name.trim().to_lowercase();
    VPN_PREFIXES.iter().any(|p| lowered.starts_with(p)) || VPN_SUBSTRINGS.iter().any(|s| lowered.contains(s))
}

/// The candidates from (adapter name, address) pairs in the order the system lists them, best first.
pub fn pick_addresses(adapters: &[(String, IpAddr)]) -> Vec<PairingAddress> {
    let mut out: Vec<PairingAddress> = Vec::new();
    for (name, ip) in adapters {
        if is_virtual_interface(name) {
            continue;
        }
        let IpAddr::V4(v4) = ip else { continue };
        let Some(kind) = classify(*v4) else { continue };
        let ip = v4.to_string();
        if out.iter().any(|c| c.ip == ip) {
            continue;
        }
        out.push(PairingAddress { ip, interface: name.trim().chars().take(MAX_INTERFACE_NAME_LEN).collect(), kind });
    }
    // Stable: within a kind the system's order is as good a guess at the primary adapter as any
    let order = |k: &str| match k { "lan" => 0, "tailscale" => 1, _ => 2 };
    out.sort_by_key(|c| (order(c.kind), looks_like_vpn_interface(&c.interface)));
    out.truncate(MAX_PAIRING_CANDIDATES);
    out
}

/// Every address a phone could plausibly reach this desktop on, from the real adapters rather than a route probe
/// (which reports the VPN's address instead of the LAN one the phone needs).
pub fn pairing_addresses() -> Vec<PairingAddress> {
    addresses_from(|| Ok(if_addrs::get_if_addrs()?.into_iter().map(|i| (i.name.clone(), i.ip())).collect()))
}

/// The candidates among what list finds, none when it can't look.
fn addresses_from(list: impl FnOnce() -> std::io::Result<Vec<(String, IpAddr)>>) -> Vec<PairingAddress> {
    list().map_or_else(|_| Vec::new(), |adapters| pick_addresses(&adapters))
}

/// Preference among a phone's addresses: Tailscale 0, LAN 1, other 2.
pub fn rank_ip(ip: &str) -> u8 {
    let parts: Vec<&str> = ip.split('.').collect();
    if parts.len() == 4 {
        let Ok(o) = parts.iter().map(|p| p.parse::<i64>()).collect::<Result<Vec<_>, _>>() else { return 2 };
        let (a, b) = (o[0], o[1]);
        if a == 100 && (64..=127).contains(&b) {
            return 0;
        }
        if a == 10 || (a == 192 && b == 168) || (a == 172 && (16..=31).contains(&b)) {
            return 1;
        }
    }
    2
}

/// A dotted IPv4 literal with no leading zeros.
pub fn valid_ipv4(ip: &str) -> bool {
    let parts: Vec<&str> = ip.split('.').collect();
    parts.len() == 4
        && parts.iter().all(|p| {
            !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) && p.parse::<u32>().is_ok_and(|n| n <= 255 && n.to_string() == *p)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adapters(list: &[(&str, &str)]) -> Vec<PairingAddress> {
        let raw: Vec<(String, IpAddr)> = list.iter().map(|(n, ip)| (n.to_string(), ip.parse().unwrap())).collect();
        pick_addresses(&raw)
    }

    #[test]
    fn rank_ip_cases() {
        for (ip, rank) in [("100.64.0.5", 0), ("100.127.255.255", 0), ("10.0.0.1", 1), ("192.168.1.1", 1), ("172.16.0.1", 1),
                           ("172.31.255.255", 1), ("172.15.0.1", 2), ("172.32.0.1", 2), ("9.9.9.9", 2)] {
            assert_eq!(rank_ip(ip), rank, "{ip}");
        }
    }

    #[test]
    fn valid_ipv4_cases() {
        for (ip, ok) in [("0.0.0.0", true), ("255.255.255.255", true), ("1.2.3.4", true), ("256.2.3.4", false),
                         ("01.2.3.4", false), ("1.2.3", false), ("a.b.c.d", false), ("+1.2.3.4", false)] {
            assert_eq!(valid_ipv4(ip), ok, "{ip}");
        }
    }

    #[test]
    fn pairing_addresses_order_lan_then_tailscale_then_other() {
        let got = adapters(&[("tailscale0", "100.90.12.34"), ("eth9", "203.0.113.7"), ("wlan0", "192.168.1.42"), ("eth0", "10.1.2.3")]);
        let got: Vec<(&str, &str, &str)> = got.iter().map(|a| (a.ip.as_str(), a.interface.as_str(), a.kind)).collect();
        assert_eq!(got, [("192.168.1.42", "wlan0", "lan"), ("10.1.2.3", "eth0", "lan"),
                         ("100.90.12.34", "tailscale0", "tailscale"), ("203.0.113.7", "eth9", "other")]);
    }

    #[test]
    fn pairing_addresses_put_a_vpn_tunnel_behind_the_real_lan() {
        let got = adapters(&[("tun0", "10.8.0.6"), ("wg0", "10.2.0.2"), ("wlan0", "192.168.1.42")]);
        assert_eq!(got.iter().map(|a| a.ip.as_str()).collect::<Vec<_>>(), ["192.168.1.42", "10.8.0.6", "10.2.0.2"]);
        assert!(got.iter().all(|a| a.kind == "lan"));
    }

    #[test]
    fn looks_like_vpn_interface_cases() {
        for (name, vpn) in [("tun0", true), ("wg0", true), ("utun3", true), ("NordLynx", true), ("ProtonVPN", true),
                            ("Tailscale", true), ("wlan0", false), ("eth0", false), ("Wi-Fi", false), ("Ethernet 2", false)] {
            assert_eq!(looks_like_vpn_interface(name), vpn, "{name}");
        }
    }

    #[test]
    fn pairing_addresses_cover_every_private_range() {
        let got = adapters(&[("a", "192.168.0.1"), ("b", "10.255.255.254"), ("c", "172.16.0.1"), ("d", "172.31.255.254"),
                             ("e", "100.64.0.1"), ("f", "100.127.255.254")]);
        let kinds: Vec<(&str, &str)> = got.iter().map(|a| (a.ip.as_str(), a.kind)).collect();
        assert_eq!(kinds, [("192.168.0.1", "lan"), ("10.255.255.254", "lan"), ("172.16.0.1", "lan"), ("172.31.255.254", "lan"),
                           ("100.64.0.1", "tailscale"), ("100.127.255.254", "tailscale")]);
    }

    #[test]
    fn pairing_addresses_skip_loopback_link_local_ipv6_and_duplicates() {
        let got = adapters(&[("lo", "127.0.0.1"), ("wlan0", "169.254.10.11"), ("wlan0", "fe80::1"), ("wlan0", "192.168.1.42"),
                             ("wlan0:1", "192.168.1.42")]);
        assert_eq!(got.iter().map(|a| (a.ip.as_str(), a.interface.as_str())).collect::<Vec<_>>(), [("192.168.1.42", "wlan0")]);
    }

    #[test]
    fn pairing_addresses_skip_container_and_vm_only_adapters() {
        let got = adapters(&[("docker0", "172.17.0.1"), ("br-1a2b3c", "172.18.0.1"), ("veth3f9a", "172.19.0.1"),
                             ("virbr0", "192.168.122.1"), ("vboxnet0", "192.168.56.1"), ("VMware Network Adapter VMnet8", "192.168.75.1"),
                             ("VirtualBox Host-Only Network", "192.168.99.1"), ("Wi-Fi", "192.168.1.42")]);
        assert_eq!(got.iter().map(|a| a.ip.as_str()).collect::<Vec<_>>(), ["192.168.1.42"]);
    }

    #[test]
    fn pairing_addresses_keeps_hyper_v_bridged_lan_adapter() {
        assert_eq!(adapters(&[("vEthernet (External)", "192.168.1.42")]).len(), 1);
    }

    #[test]
    fn pairing_addresses_are_capped_and_keep_the_best_ones() {
        let mut list: Vec<(String, String)> = (0..8).map(|i| (format!("tailscale{i}"), format!("100.64.0.{i}"))).collect();
        list.push(("wlan0".into(), "192.168.1.42".into()));
        let raw: Vec<(String, IpAddr)> = list.iter().map(|(n, ip)| (n.clone(), ip.parse().unwrap())).collect();
        let got = pick_addresses(&raw);
        assert_eq!(got.len(), MAX_PAIRING_CANDIDATES);
        assert_eq!(MAX_PAIRING_CANDIDATES, 8);
        assert_eq!(got[0].ip, "192.168.1.42");
    }

    #[test]
    fn pairing_addresses_tolerate_enumeration_failure() {
        assert_eq!(addresses_from(|| Err(std::io::Error::other("no adapters for you"))), []);
        let found = addresses_from(|| Ok(vec![("wlan0".to_string(), "192.168.1.42".parse().unwrap())]));
        assert_eq!(found.len(), 1, "the same road works when the list comes");
    }

    #[test]
    fn pairing_addresses_trim_very_long_interface_names() {
        let got = adapters(&[("Intel(R) Wi-Fi 6E AX211 160MHz Adapter #2", "192.168.1.42")]);
        assert_eq!(got[0].interface, "Intel(R) Wi-Fi 6E AX211 160MHz A");
    }

    #[test]
    fn classify_ip_cases() {
        for (ip, kind) in [("192.168.1.1", Some("lan")), ("10.0.0.1", Some("lan")), ("172.16.0.1", Some("lan")),
                           ("172.15.0.1", Some("other")), ("172.32.0.1", Some("other")), ("100.64.0.1", Some("tailscale")),
                           ("100.63.255.255", Some("other")), ("100.128.0.0", Some("other")), ("9.9.9.9", Some("other")),
                           ("127.0.0.1", None), ("169.254.1.1", None), ("224.0.0.1", None), ("0.0.0.0", None), ("::1", None),
                           ("not-an-ip", None)] {
            assert_eq!(classify_ip(ip), kind, "{ip}");
        }
    }

    #[test]
    fn no_route_probe_towards_a_public_address_remains() {
        let probe = ["8", "8", "8", "8"].join(".");
        // The whole app, as Python's looks through the whole package
        let here = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut dirs = vec![here.join("../../crates"), here.join("../../app")];
        let mut read = 0;
        while let Some(dir) = dirs.pop() {
            for entry in std::fs::read_dir(dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    dirs.push(path);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    let text = std::fs::read_to_string(&path).unwrap();
                    read += 1;
                    assert!(!text.contains(&probe), "{path:?}");
                }
            }
        }
        assert!(read > 300, "only {read} files");
    }
}
