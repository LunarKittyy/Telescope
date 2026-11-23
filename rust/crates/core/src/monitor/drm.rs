//! Graphics card use per client from DRM fdinfo (the kernel's usage stats, see Documentation/gpu/drm-usage-stats.rst), as pure parsing and arithmetic.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use super::GpuLoad;
use std::collections::HashMap;

/// One kind of engine's busy counter; the unit depends on the driver.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Busy {
    /// Nanoseconds busy so far (i915, amdgpu), over `capacity` identical engines.
    Ns { ns: u64, capacity: u64 },
    /// Cycles busy and cycles passed so far (xe).
    Cycles { busy: u64, total: u64 },
}

/// One open graphics device as fdinfo shows it.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct DrmClient {
    pub driver: String,
    pub pdev: Option<String>,
    /// Shared by every duplicated or inherited fd of one client; missing on old kernels.
    pub id: Option<u64>,
    pub engines: Vec<(String, Busy)>,
}

/// Who a client is: the same id on the same device is one client however many fds and processes hold it.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct ClientKey {
    pub driver: String,
    pub pdev: Option<String>,
    pub id: u64,
}

impl DrmClient {
    pub fn key(&self) -> Option<ClientKey> {
        Some(ClientKey { driver: self.driver.clone(), pdev: self.pdev.clone(), id: self.id? })
    }
}

/// "123 ns" as 123; None for any other unit.
fn parse_ns(value: &str) -> Option<u64> {
    let mut parts = value.split_whitespace();
    let n = parts.next()?.parse().ok()?;
    (parts.next() == Some("ns") && parts.next().is_none()).then_some(n)
}

/// /proc/<pid>/fdinfo/<fd>; None when the fd isn't a graphics device (no `drm-driver`).
pub(crate) fn parse_drm_fdinfo(text: &str) -> Option<DrmClient> {
    let mut client = DrmClient::default();
    let mut driver = false;
    let mut ns: Vec<(&str, u64)> = Vec::new();
    let mut capacity: Vec<(&str, u64)> = Vec::new();
    let mut cycles: Vec<(&str, u64)> = Vec::new();
    let mut total_cycles: Vec<(&str, u64)> = Vec::new();
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else { continue };
        let value = value.trim();
        match key {
            "drm-driver" => {
                client.driver = value.to_string();
                driver = true;
            }
            "drm-pdev" => client.pdev = Some(value.to_string()),
            "drm-client-id" => client.id = value.parse().ok(),
            _ => {
                if let Some(name) = key.strip_prefix("drm-engine-capacity-") {
                    capacity.extend(value.parse().ok().map(|n| (name, n)));
                } else if let Some(name) = key.strip_prefix("drm-engine-") {
                    ns.extend(parse_ns(value).map(|n| (name, n)));
                } else if let Some(name) = key.strip_prefix("drm-total-cycles-") {
                    total_cycles.extend(value.parse().ok().map(|n| (name, n)));
                } else if let Some(name) = key.strip_prefix("drm-cycles-") {
                    cycles.extend(value.parse().ok().map(|n| (name, n)));
                }
            }
        }
    }
    if !driver {
        return None;
    }
    for (name, n) in ns {
        let cap = capacity.iter().find(|(c, _)| *c == name).map_or(1, |(_, c)| (*c).max(1));
        client.engines.push((name.to_string(), Busy::Ns { ns: n, capacity: cap }));
    }
    for (name, busy) in cycles {
        if let Some((_, total)) = total_cycles.iter().find(|(t, _)| *t == name) {
            client.engines.push((name.to_string(), Busy::Cycles { busy, total: *total }));
        }
    }
    Some(client)
}

/// A client's share of the card between two looks `wall_ns` apart: its busiest engine, 0 to 100. None when no engine can be compared or a counter went backwards (the client was reopened).
pub(crate) fn client_share(before: &DrmClient, after: &DrmClient, wall_ns: u64) -> Option<f32> {
    let mut best: Option<f64> = None;
    for (name, now) in &after.engines {
        let Some((_, was)) = before.engines.iter().find(|(n, _)| n == name) else { continue };
        let share = match (was, now) {
            (Busy::Ns { ns: a, .. }, Busy::Ns { ns: b, capacity }) => {
                let delta = b.checked_sub(*a)?;
                if wall_ns == 0 {
                    continue;
                }
                delta as f64 / (wall_ns as f64 * *capacity as f64)
            }
            (Busy::Cycles { busy: a, total: ta }, Busy::Cycles { busy: b, total: tb }) => {
                let (delta, total) = (b.checked_sub(*a)?, tb.checked_sub(*ta)?);
                if total == 0 {
                    continue;
                }
                delta as f64 / total as f64
            }
            _ => continue,
        };
        best = Some(best.map_or(share, |b| b.max(share)));
    }
    best.map(|s| (s * 100.0).clamp(0.0, 100.0) as f32)
}

/// Each client's share by looks at the same clients now and before; clients that are new have no reading.
pub(crate) fn client_shares(before: &HashMap<ClientKey, DrmClient>, now: &HashMap<ClientKey, (u32, DrmClient)>, wall_ns: u64) -> Vec<(u32, f32)> {
    let mut shares: Vec<(u32, f32)> = now.iter().filter_map(|(key, (pid, client))| Some((*pid, client_share(before.get(key)?, client, wall_ns)?))).collect();
    shares.sort_by_key(|(pid, _)| *pid);
    shares
}

/// The card's load from client shares (one per client, each counted once): each process's clients added up, and overall the system's own total if it has one, else the clients' sum, both capped at 100.
pub(crate) fn gpu_load_from_clients(shares: &[(u32, f32)], system_total: Option<f32>) -> Option<GpuLoad> {
    let mut per_pid: Vec<(u32, f32)> = Vec::new();
    for &(pid, share) in shares {
        match per_pid.iter_mut().find(|(p, _)| *p == pid) {
            Some((_, sum)) => *sum += share,
            None => per_pid.push((pid, share)),
        }
    }
    let sum: f32 = per_pid.iter().map(|(_, v)| *v).sum();
    for (_, v) in &mut per_pid {
        *v = v.min(100.0);
    }
    if per_pid.is_empty() {
        return system_total.map(|total| GpuLoad { total, per_pid });
    }
    Some(GpuLoad { total: system_total.unwrap_or(sum).min(100.0), per_pid })
}
