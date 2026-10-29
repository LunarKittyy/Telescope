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

fn services(device: Option<String>) -> Arc<Services> {
    let link = Arc::new(telescope_link::LinkService::new());
    // Added plugins can't take a built-in's name; "en" until the shell loads languages
    let added = telescope_plugin_host::load::factory(vec!["camera_control".into(), "onboarding".into(), "connection".into(), "microphone".into(), "stream_output".into(), "webcam".into()], "en".into());
    let connection = link.clone();
    let mic_link: Arc<dyn telescope_core::link::Link> = link.clone();
    Arc::new(Services {
        link,
        media: Arc::new(telescope_media::FfmpegMedia),
        outputs: Arc::new(telescope_outputs::VirtualCameras { device }),
        audio: Some(Arc::new(telescope_audio::VirtualMic::default())),
        notifier: Some(Arc::new(telescope_system::Desktop)),
        autostart: Some(Arc::new(telescope_system::Desktop)),
        updater: Some(Arc::new(telescope_system::Desktop)),
        plugins: Box::new(move || {
            let mut plugins: Vec<Box<dyn telescope_core::plugin::Plugin>> = vec![
                Box::new(connection.connection_plugin()),
                Box::new(telescope_audio::plugin::MicrophonePlugin::with_link(mic_link.clone())),
                Box::new(telescope_outputs::stream_output::StreamOutputPlugin::new(true)),
                Box::new(telescope_outputs::webcam::WebcamPlugin::system()),
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
