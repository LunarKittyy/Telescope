//! The desktop around the window, behind core's system traits: a skeleton (tray, notifications, autostart, single instance, global shortcuts, updater and update guard to come).

pub mod autostart;
pub mod hotkeys;
#[cfg(target_os = "linux")]
pub mod hotkeys_portal;
pub mod hotkeys_windows;
pub mod keys;
pub mod shortcuts;
pub mod single_instance;
pub mod startup;
#[cfg(target_os = "linux")]
pub mod tray;
#[cfg(all(test, target_os = "linux"))]
mod test_bus;
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

/// The notify-send call for a notification (send_notification): urgent ones stay on screen until dismissed.
pub fn notify_command(title: &str, body: &str, urgent: bool) -> Vec<String> {
    ["notify-send", "-a", "Telescope", "-u", if urgent { "critical" } else { "normal" }, "--", title, body]
        .into_iter().map(String::from).collect()
}

fn on_path(program: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|path| std::env::split_paths(&path).any(|d| d.join(program).is_file()))
}

impl Notifier for Desktop {
    fn notify(&self, title: &str, body: &str, urgent: bool) {
        if cfg!(target_os = "linux") && on_path("notify-send") {
            let cmd = notify_command(title, body, urgent);
            let spawned = std::process::Command::new(&cmd[0]).args(&cmd[1..])
                .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn();
            match spawned {
                // Reaped on a thread of its own, so it doesn't linger as a zombie
                Ok(mut child) => drop(std::thread::Builder::new().name("notify-send".into()).spawn(move || child.wait())),
                Err(e) => log::warn!("Couldn't show a notification: {e}"),
            }
            return;
        }
        // Windows shows it as a tray balloon, which needs the tray
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

#[cfg(test)]
mod tests {
    #[test]
    fn notifications_go_through_notify_send() {
        assert_eq!(super::notify_command("Telescope", "-Torch: on", false),
                   ["notify-send", "-a", "Telescope", "-u", "normal", "--", "Telescope", "-Torch: on"]);
        assert_eq!(super::notify_command("Phone lost", "x", true)[4], "critical");
    }
}
