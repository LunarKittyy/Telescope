//! A private session bus for tests that talk D-Bus, gone when dropped.

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};

pub struct Bus(Child, pub String);

impl Bus {
    /// None where dbus-daemon isn't installed.
    pub fn start() -> Option<Bus> {
        let mut child = Command::new("dbus-daemon").args(["--session", "--nofork", "--print-address"])
            .stdout(Stdio::piped()).spawn().ok()?;
        let mut address = String::new();
        BufReader::new(child.stdout.take()?).read_line(&mut address).ok()?;
        Some(Bus(child, address.trim().to_string()))
    }

    pub fn connect(&self) -> zbus::Result<zbus::blocking::Connection> {
        zbus::blocking::connection::Builder::address(self.1.as_str())?.build()
    }
}

impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
