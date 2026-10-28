//! This computer's IPv4 addresses a phone could pair over (ip_utils.get_pairing_addresses).

use serde::Serialize;
use std::net::Ipv4Addr;

#[derive(Clone, Debug, Serialize)]
pub struct PairingAddress {
    pub ip: String,
    pub interface: String,
    pub kind: &'static str,
}

const VIRTUAL_PREFIXES: &[&str] = &["docker", "br-", "veth", "virbr", "vboxnet", "vmnet"];
const VPN_PREFIXES: &[&str] = &["tun", "tap", "wg", "utun", "ppp", "ipsec", "nordlynx"];
const VPN_SUBSTRINGS: &[&str] = &["vpn", "wireguard", "tailscale"];
const MAX_CANDIDATES: usize = 8;

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

fn looks_like_vpn(name: &str) -> bool {
    let lower = name.to_lowercase();
    VPN_PREFIXES.iter().any(|p| lower.starts_with(p)) || VPN_SUBSTRINGS.iter().any(|s| lower.contains(s))
}

pub fn pairing_addresses() -> Vec<PairingAddress> {
    let mut out: Vec<PairingAddress> = Vec::new();
    let mut ifap: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs fills a linked list we walk read-only and free with freeifaddrs
    unsafe {
        if libc::getifaddrs(&mut ifap) != 0 {
            return out;
        }
        let mut cur = ifap;
        while !cur.is_null() {
            let ifa = &*cur;
            cur = ifa.ifa_next;
            if ifa.ifa_addr.is_null() || (*ifa.ifa_addr).sa_family as i32 != libc::AF_INET {
                continue;
            }
            let name = std::ffi::CStr::from_ptr(ifa.ifa_name).to_string_lossy().into_owned();
            if VIRTUAL_PREFIXES.iter().any(|p| name.to_lowercase().starts_with(p)) {
                continue;
            }
            let sin = &*(ifa.ifa_addr as *const libc::sockaddr_in);
            let ip = Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr));
            let Some(kind) = classify(ip) else { continue };
            if out.iter().any(|c| c.ip == ip.to_string()) {
                continue;
            }
            out.push(PairingAddress { ip: ip.to_string(), interface: name.chars().take(32).collect(), kind });
        }
        libc::freeifaddrs(ifap);
    }
    let order = |k: &str| match k { "lan" => 0, "tailscale" => 1, _ => 2 };
    out.sort_by_key(|c| (order(c.kind), looks_like_vpn(&c.interface)));
    out.truncate(MAX_CANDIDATES);
    out
}
