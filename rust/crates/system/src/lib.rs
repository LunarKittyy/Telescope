//! The desktop around the window, behind core's system traits: a skeleton (tray, notifications, autostart, single instance, global shortcuts, updater and update guard to come).

pub mod shortcuts;
pub mod single_instance;

use telescope_core::system::{Autostart, Notifier, Release, Updater};

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
        false
    }

    fn set_enabled(&self, _on: bool, _minimized: bool) -> Result<(), String> {
        Err("Starting with the computer isn't in this build yet".into())
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
