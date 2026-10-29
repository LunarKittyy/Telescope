//! telescope-rs: the Rust rewrite of the Telescope desktop app, wiring every crate together.
//!
//!   telescope-rs                          the window
//!   telescope-rs pair [--usb]             pair a phone from the terminal (QR code, or the USB offer over adb)
//!   telescope-rs stream                   stream without a window, printing stats each second
//!   telescope-rs control JSON             send one /v1/control body to a streaming phone and show the state it changed
//!   telescope-rs snapshot OUT             save the window as a PPM once it has drawn, for comparing with the Qt one
//!   telescope-rs config-snapshot FOLDER   what this app makes of a config fixture, in the shape of its expected.json
//!   telescope-rs scenes OUT [ids]         parity/render_scenes.py's scenes, drawn offscreen for compare_scenes.py
//!
//! --autostart starts streaming once the window is up and --quit-after SECS closes it, for measuring.
//!
//! main.py's own options: --action ID [NAME=VALUE ...] runs a shortcut action in the running copy, --list-actions
//! lists them, --minimized starts in the tray, --after-update and --restarted wait for the old copy to let go of
//! the lock, --moved-from FOLDER is taken and ignored until the updater is ported.

mod cli;
mod scenes;

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

fn services(device: Option<String>, remote: telescope_system::shortcuts::RemoteActions) -> Arc<Services> {
    let link = Arc::new(telescope_link::LinkService::new());
    // Added plugins can't take a built-in's name; "en" until the shell loads languages
    let added = telescope_plugin_host::load::factory(vec!["camera_control".into(), "onboarding".into(), "connection".into()], "en".into());
    let connection = link.clone();
    Arc::new(Services {
        link,
        media: Arc::new(telescope_media::FfmpegMedia),
        outputs: Arc::new(telescope_outputs::VirtualCameras { device }),
        audio: Some(Arc::new(telescope_audio::VirtualMic)),
        notifier: Some(Arc::new(telescope_system::Desktop)),
        autostart: Some(Arc::new(telescope_system::Desktop)),
        updater: Some(Arc::new(telescope_system::Desktop)),
        plugins: Box::new(move || {
            let mut plugins: Vec<Box<dyn telescope_core::plugin::Plugin>> = vec![
                Box::new(connection.connection_plugin()),
                Box::new(telescope_system::shortcuts::ShortcutsPlugin::new(remote.clone())),
            ];
            plugins.extend(added());
            plugins
        }),
    })
}

/// The preview's frames painted by media's GPU scaler on the window's own device, two textures taking turns so the one
/// Slint shows isn't drawn over.
struct GpuPreview {
    gpu: telescope_media::gpu::Gpu,
    pictures: [Option<telescope_shell::preview::wgpu::Texture>; 2],
    next: usize,
}

impl telescope_shell::preview::TexturePainter for GpuPreview {
    fn paint(&mut self, frame: &telescope_core::Frame, width: u32, height: u32) -> Option<telescope_shell::preview::wgpu::Texture> {
        let slot = &mut self.pictures[self.next];
        if slot.as_ref().is_none_or(|t| (t.width(), t.height()) != (width, height)) {
            *slot = Some(self.gpu.picture(width, height));
        }
        let target = slot.clone()?;
        self.next = 1 - self.next;
        match self.gpu.paint(frame, &target) {
            Ok(()) => Some(target),
            Err(e) => {
                log::warn!("Couldn't paint the preview: {e}");
                None
            }
        }
    }
}

/// --action / --list-actions: talk to the running copy, print what it says, and exit (run_action).
fn run_action(words: Option<Vec<String>>) -> i32 {
    use telescope_system::single_instance as si;
    let request = match words {
        Some(words) => match si::action_request(&words) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("telescope: {e}");
                return 2;
            }
        },
        None => serde_json::json!({"list": true}),
    };
    let key = si::control_key_path(&telescope_core::config::config_path());
    let (ok, text) = si::send_action(&si::Address::current(), &key, &request);
    if ok { println!("{text}") } else { eprintln!("{text}") }
    if ok { 0 } else { 1 }
}

/// The single-instance lock for the window, or None after the running copy was asked to show itself (main.py's
/// acquire_single_instance, write_control_key and listen_for_raise). The lock's thread answers later launches.
fn take_lock(wait: bool, remote: &telescope_system::shortcuts::RemoteActions) -> Option<()> {
    use telescope_system::single_instance as si;
    let address = si::Address::current();
    let acquired = si::acquire(&address, std::time::Duration::from_secs(if wait { 15 } else { 0 }));
    let si::Acquired::Lock(lock) = acquired else {
        if let Some((title, text)) = si::notice(&acquired, &address) {
            eprintln!("{title}: {text}");
        }
        return None;
    };
    // Straight away: an --action from a sign-in script mustn't find the last run's key
    let key = si::write_control_key(&si::control_key_path(&telescope_core::config::config_path()));
    let remote = remote.clone();
    si::listen(lock, || log::info!("Another start asked this window to show itself"), move |request| remote.handle(request), key);
    Some(())
}

fn main() {
    let mut words: Vec<String> = std::env::args().skip(1).collect();
    // --action takes every word after it
    let action = words.iter().position(|w| w == "--action").map(|i| words.split_off(i).split_off(1));
    let list_actions = words.iter().any(|w| w == "--list-actions");
    if action.is_some() || list_actions {
        use_own_profile();
        std::process::exit(run_action(action));
    }
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
        width: value(&mut words, "--width").and_then(|s| s.parse().ok()).unwrap_or(0),
        height: value(&mut words, "--height").and_then(|s| s.parse().ok()).unwrap_or(0),
        fps: value(&mut words, "--fps").and_then(|s| s.parse().ok()).unwrap_or(30),
        autostart: flag(&mut words, "--autostart"),
        quit_after: value(&mut words, "--quit-after").and_then(|s| s.parse().ok()),
    };
    let minimized = flag(&mut words, "--minimized");
    let wait_for_lock = flag(&mut words, "--after-update") | flag(&mut words, "--restarted");
    let _moved_from = value(&mut words, "--moved-from");
    if minimized {
        log::info!("--minimized: starting in the tray isn't in this build yet, so the window opens");
    }
    let remote = telescope_system::shortcuts::RemoteActions::default();
    telescope_shell::preview::use_gpu_painter(Arc::new(|device, queue| {
        Box::new(GpuPreview { gpu: telescope_media::gpu::Gpu::on_device(device, queue), pictures: [None, None], next: 0 })
    }));
    let command = words.first().map(String::as_str);
    if command == Some("scenes") {
        if let Err(e) = scenes::run(&words[1..]) {
            eprintln!("{e}");
            std::process::exit(1);
        }
        return;
    }
    if command != Some("config-snapshot") {
        use_own_profile();
    }
    let result = match command {
        Some("config-snapshot") => cli::config_snapshot(words.get(1).map(String::as_str)),
        Some("pair") => cli::pair(&args, &services(None, remote)),
        Some("stream") => cli::stream(&args, &services(args.device.clone(), remote)),
        Some("control") => cli::control(&args, &services(None, remote), words.get(1).map(String::as_str).unwrap_or("{}")),
        Some("snapshot") => {
            let out = words.get(1).cloned().unwrap_or_else(|| "snapshot.ppm".into());
            telescope_shell::run(services(args.device.clone(), remote), args.window(Some(out)))
        }
        Some(other) => Err(format!("Unknown command {other}")),
        None => match take_lock(wait_for_lock, &remote) {
            Some(()) => telescope_shell::run(services(args.device.clone(), remote), args.window(None)),
            None => Ok(()),
        },
    };
    if let Err(e) = result {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
