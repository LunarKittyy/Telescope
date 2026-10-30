//! The desktop around the window, behind core's system traits: a skeleton (tray, notifications, autostart, single instance, global shortcuts, updater and update guard to come).

pub mod autostart;
pub mod hotkeys;
#[cfg(target_os = "linux")]
pub mod hotkeys_portal;
pub mod hotkeys_windows;
pub mod keys;
pub mod memory;
pub mod registry;
pub mod shortcuts;
pub mod setup_move;
pub mod single_instance;
pub mod splash;
// The tray's window declares some of the same functions with its own handle types
#[cfg(windows)]
#[allow(clashing_extern_declarations)]
pub mod splash_windows;
pub mod startup;
#[cfg(target_os = "linux")]
pub mod tray;
#[allow(clashing_extern_declarations)]
pub mod tray_windows;
#[cfg(all(test, target_os = "linux"))]
mod test_bus;
pub mod update_guard;
pub mod updates;
pub mod updates_plugin;
pub mod version;
pub mod winapp;

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
/// This desktop's tray icon: StatusNotifierItem on Linux, the notification area on Windows; None where neither shows.
pub fn system_tray() -> Option<Box<dyn telescope_core::system::Tray>> {
    #[cfg(target_os = "linux")]
    return tray::SniTray::new().map(|t| {
        TRAY_SHOWN.store(true, std::sync::atomic::Ordering::SeqCst);
        Box::new(t) as Box<dyn telescope_core::system::Tray>
    });
    #[cfg(windows)]
    return tray_windows::WindowsTray::new().map(|t| Box::new(t) as Box<dyn telescope_core::system::Tray>);
    #[cfg(not(any(target_os = "linux", windows)))]
    None
}

#[derive(Default)]
pub struct Desktop;

/// The notify-send call for a notification (send_notification): urgent ones stay on screen until dismissed.
pub fn notify_command(title: &str, body: &str, urgent: bool) -> Vec<String> {
    ["notify-send", "-a", "Telescope", "-u", if urgent { "critical" } else { "normal" }, "--", title, body]
        .into_iter().map(String::from).collect()
}

/// Whether the tray icon is showing, which Linux notifications fall back on without notify-send (send_notification's `elif self._tray`).
static TRAY_SHOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// How a notification gets out.
#[derive(Debug, PartialEq, Eq)]
pub enum NotifyRoute {
    NotifySend,
    /// Through the tray icon, which on Linux is the desktop's notification service.
    Tray,
    Nowhere,
}

/// send_notification's choice: notify-send on Linux when it is installed, else the tray icon where there is one.
pub fn notify_route(linux: bool, has_notify_send: bool, tray_shown: bool) -> NotifyRoute {
    if linux && has_notify_send {
        NotifyRoute::NotifySend
    } else if tray_shown {
        NotifyRoute::Tray
    } else {
        NotifyRoute::Nowhere
    }
}

fn on_path(program: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|path| std::env::split_paths(&path).any(|d| d.join(program).is_file()))
}

impl Notifier for Desktop {
    fn notify(&self, title: &str, body: &str, urgent: bool) {
        let route = notify_route(cfg!(target_os = "linux"), cfg!(target_os = "linux") && on_path("notify-send"),
                                 TRAY_SHOWN.load(std::sync::atomic::Ordering::SeqCst));
        if route == NotifyRoute::NotifySend {
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
        // Windows shows it as the tray icon's balloon, which needs the tray
        #[cfg(windows)]
        if tray_windows::balloon(title, body, urgent) {
            return;
        }
        #[cfg(target_os = "linux")]
        if route == NotifyRoute::Tray {
            if let Err(e) = tray::desktop_message(title, body, urgent) {
                log::warn!("Couldn't show a notification: {e}");
            }
            return;
        }
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
    fn without_notify_send_the_tray_shows_a_notification() {
        use super::{notify_route, NotifyRoute::*};
        assert_eq!(notify_route(true, true, true), NotifySend);
        assert_eq!(notify_route(true, true, false), NotifySend);
        assert_eq!(notify_route(true, false, true), Tray);
        assert_eq!(notify_route(true, false, false), Nowhere);
        // Windows has no notify-send: its tray balloon is the only way
        assert_eq!(notify_route(false, false, true), Tray);
        assert_eq!(notify_route(false, false, false), Nowhere);
    }

    #[test]
    fn notifications_go_through_notify_send() {
        assert_eq!(super::notify_command("Telescope", "-Torch: on", false),
                   ["notify-send", "-a", "Telescope", "-u", "normal", "--", "Telescope", "-Torch: on"]);
        assert_eq!(super::notify_command("Phone lost", "x", true)[4], "critical");
    }
}
