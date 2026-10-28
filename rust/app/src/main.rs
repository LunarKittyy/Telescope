//! telescope-rs: the Rust rewrite of the Telescope desktop app, wiring every crate together.
//!
//!   telescope-rs                          the window
//!   telescope-rs pair [--usb]             pair a phone from the terminal (QR code, or the USB offer over adb)
//!   telescope-rs stream                   stream without a window, printing stats each second
//!   telescope-rs control JSON             send one /v1/control body to a streaming phone and show the state it changed
//!   telescope-rs snapshot OUT             save the window as a PPM once it has drawn, for comparing with the Qt one
//!   telescope-rs config-snapshot FOLDER   what this app makes of a config fixture, in the shape of its expected.json
//!
//! --autostart starts streaming once the window is up and --quit-after SECS closes it, for measuring.

mod cli;

use std::path::PathBuf;
use std::sync::Arc;
use telescope_core::config::DEV_PROFILE_ENV;
use telescope_core::services::Services;

/// Until the rewrite replaces the Python app, it keeps its settings in its own profile unless asked not to.
fn use_own_profile() {
    if std::env::var_os(DEV_PROFILE_ENV).is_some() || std::env::var("TELESCOPE_RS_REAL_CONFIG").as_deref() == Ok("1") {
        return;
    }
    let folder = std::env::var_os("TELESCOPE_RS_PROFILE").map(PathBuf::from).unwrap_or_else(|| {
        let base = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()).map(PathBuf::from)
            .unwrap_or_else(|| std::env::home_dir().unwrap_or_default().join(".config"));
        base.join("telescope-rs")
    });
    // Only this thread runs yet, so changing the environment can't race anything
    std::env::set_var(DEV_PROFILE_ENV, folder);
}

fn services(device: Option<String>) -> Arc<Services> {
    Arc::new(Services {
        link: Arc::new(telescope_link::LinkService::new()),
        media: Arc::new(telescope_media::FfmpegMedia),
        outputs: Arc::new(telescope_outputs::VirtualCameras { device }),
        audio: Some(Arc::new(telescope_audio::VirtualMic)),
        notifier: Some(Arc::new(telescope_system::Desktop)),
        autostart: Some(Arc::new(telescope_system::Desktop)),
        updater: Some(Arc::new(telescope_system::Desktop)),
        plugins: Box::new(Vec::new),
    })
}

fn main() {
    let mut words: Vec<String> = std::env::args().skip(1).collect();
    let flag = |words: &mut Vec<String>, name: &str| -> bool {
        let found = words.iter().position(|w| w == name);
        found.map(|i| words.remove(i)).is_some()
    };
    let value = |words: &mut Vec<String>, name: &str| -> Option<String> {
        let i = words.iter().position(|w| w == name)?;
        words.remove(i);
        (i < words.len()).then(|| words.remove(i))
    };
    let args = cli::Args {
        usb: flag(&mut words, "--usb"),
        vaapi: flag(&mut words, "--vaapi"),
        package: value(&mut words, "--package").unwrap_or_else(|| "com.telescope.dev".into()),
        device: value(&mut words, "--device"),
        seconds: value(&mut words, "--seconds").and_then(|s| s.parse().ok()).unwrap_or(30),
        route: value(&mut words, "--route").unwrap_or_else(|| "auto".into()),
        width: value(&mut words, "--width").and_then(|s| s.parse().ok()).unwrap_or(1920),
        height: value(&mut words, "--height").and_then(|s| s.parse().ok()).unwrap_or(1080),
        fps: value(&mut words, "--fps").and_then(|s| s.parse().ok()).unwrap_or(30),
        autostart: flag(&mut words, "--autostart"),
        quit_after: value(&mut words, "--quit-after").and_then(|s| s.parse().ok()),
    };
    let command = words.first().map(String::as_str);
    if command != Some("config-snapshot") {
        use_own_profile();
    }
    let result = match command {
        Some("config-snapshot") => cli::config_snapshot(words.get(1).map(String::as_str)),
        Some("pair") => cli::pair(&args, &services(None)),
        Some("stream") => cli::stream(&args, &services(args.device.clone())),
        Some("control") => cli::control(&args, &services(None), words.get(1).map(String::as_str).unwrap_or("{}")),
        Some("snapshot") => {
            let out = words.get(1).cloned().unwrap_or_else(|| "snapshot.ppm".into());
            telescope_shell::run(services(args.device.clone()), args.window(Some(out)))
        }
        Some(other) => Err(format!("Unknown command {other}")),
        None => telescope_shell::run(services(args.device.clone()), args.window(None)),
    };
    if let Err(e) = result {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
