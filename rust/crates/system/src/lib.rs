//! The desktop around the window, behind core's system traits: a skeleton (tray, notifications, autostart, single instance, global shortcuts, updater and update guard to come).

pub mod autostart;
pub mod hotkeys;
#[cfg(target_os = "linux")]
pub mod hotkeys_portal;
pub mod keys;
pub mod shortcuts;
pub mod single_instance;
pub mod update_guard;
pub mod updates;
pub mod updates_plugin;
pub mod version;

use telescope_core::system::{Autostart, Notifier, Release, Updater};

/// The folder this program runs from (autostart.app_dir).
pub fn app_dir() -> std::path::PathBuf {
    std::env::current_exe().ok().and_then(|p| p.parent().map(std::path::PathBuf::from)).unwrap_or_default()
}

/// Opens a link in the default browser.
pub fn open_url(url: &str) {
    let spawned = if cfg!(windows) {
        std::process::Command::new("rundll32").args(["url.dll,FileProtocolHandler", url]).spawn()
    } else {
        std::process::Command::new("xdg-open").arg(url).spawn()
    };
    if let Err(e) = spawned {
        log::warn!("Couldn't open {url}: {e}");
    }
}

/// This computer's desktop.
#[derive(Default)]
pub struct Desktop;

impl Notifier for Desktop {
    fn notify(&self, title: &str, body: &str, _urgent: bool) {
        log::info!("Notification: {title}: {body}");
    }
}

impl Autostart for Desktop {
    fn enabled(&self) -> bool {
        autostart::is_enabled(&autostart::here())
    }

    fn set_enabled(&self, on: bool, minimized: bool) -> Result<(), String> {
        if on { autostart::enable(&autostart::launch_command(&app_dir(), minimized), &autostart::here()) } else { autostart::disable(&autostart::here()) }
    }
}

impl Updater for Desktop {
    fn check(&self, _channel: &str) -> Result<Option<Release>, String> {
        Ok(None)
    }

    fn install(&self, _release: &Release) -> Result<(), String> {
        Err("Updating isn't in this build yet".into())
    }

    fn on_start(&self) -> Result<(), String> {
        Ok(())
    }

    fn confirm_started(&self) {}
}
