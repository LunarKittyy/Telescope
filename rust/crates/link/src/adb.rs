//! The adb commands the desktop needs (platform/__init__.py): devices, forwards, the reverse to the pairing server
//! and the pairing broadcast.

use std::process::Command;

pub const PAIR_BROADCAST_ACTION: &str = "com.telescope.action.PAIR";

fn adb(serial: Option<&str>, args: &[&str]) -> Option<String> {
    let mut cmd = Command::new(std::env::var("TELESCOPE_ADB").unwrap_or_else(|_| "adb".into()));
    if let Some(s) = serial {
        cmd.args(["-s", s]);
    }
    let out = cmd.args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Serials of devices that accepted USB debugging.
pub fn devices() -> Vec<String> {
    adb(None, &["devices"])
        .unwrap_or_default()
        .lines()
        .skip(1)
        .filter_map(|l| l.split_once('\t'))
        .filter(|(_, state)| state.trim() == "device")
        .map(|(serial, _)| serial.to_string())
        .collect()
}

/// A free local port (adb picks it) forwarded to the phone's remote port.
pub fn forward(serial: &str, remote: u16) -> Option<u16> {
    adb(Some(serial), &["forward", "tcp:0", &format!("tcp:{remote}")])?.trim().lines().last()?.parse().ok()
}

pub fn unforward(serial: &str, local: u16) {
    adb(Some(serial), &["forward", "--remove", &format!("tcp:{local}")]);
}

/// The phone's own localhost:port tunnelled to this machine's, so it reaches the pairing server over the cable.
pub fn reverse(serial: &str, port: u16) -> bool {
    adb(Some(serial), &["reverse", &format!("tcp:{port}"), &format!("tcp:{port}")]).is_some()
}

pub fn unreverse(serial: &str, port: u16) {
    adb(Some(serial), &["reverse", "--remove", &format!("tcp:{port}")]);
}

/// Pushes the USB pairing offer to the Telescope app (base64, so nothing needs shell escaping).
pub fn broadcast_pair(serial: &str, package: &str, payload_b64: &str) -> bool {
    adb(Some(serial), &["shell", "am", "broadcast", "-a", PAIR_BROADCAST_ACTION, "-p", package, "--es", "payload", payload_b64])
        .is_some()
}
