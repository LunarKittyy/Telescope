// test_main.py::test_the_window_comes_up_without_loading_what_only_a_stream_needs: only the vision pack is still loaded from outside
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use telescope_core::config::{ConfigFile, CONFIG_FILENAME};
use telescope_core::plugin::Plugin;
use telescope_core::services::Services;
use telescope_effects::background::BackgroundPlugin;
use telescope_effects::framing::FramingPlugin;
use telescope_shell::window;
use telescope_vision::{pack::PackInfo, service::LoadError, VisionHost};

#[test]
fn the_window_comes_up_without_loading_the_vision_pack() {
    i_slint_backend_testing::init_no_event_loop();
    let dir = telescope_core::testing::scratch_dir("window-start");
    let loads = Arc::new(AtomicU32::new(0));
    let counted = loads.clone();
    let host = VisionHost::with(Box::new(move || {
        counted.fetch_add(1, Ordering::SeqCst);
        Err(LoadError::Missing("not here".into()))
    }), None, PathBuf::from("/nowhere"), PackInfo::current(), Box::new(telescope_vision::pack::web));
    let services = Arc::new(Services {
        link: Arc::new(telescope_link::LinkService::new()), media: Arc::new(telescope_media::FfmpegMedia), outputs: Arc::new(telescope_outputs::VirtualCameras::default()), audio: None,
        notifier: None, autostart: None, updater: None, tray: None, plugins: Box::new(Vec::new), vision: Some(Arc::new(host)), plugin_problems: Default::default(), plugin_admin: None,
    });
    let plugins = |_: &_| vec![Box::new(BackgroundPlugin::new()) as Box<dyn Plugin>, Box::new(FramingPlugin::new())];
    let w = window::build(ConfigFile::at(dir.join(CONFIG_FILENAME)), plugins, Some(services), None).unwrap();
    w.process();
    assert_eq!(loads.load(Ordering::SeqCst), 0);
}

// The same for what the program itself builds: its services, every built-in plugin and the window, offscreen. Asking for the pack logs either way
// it goes, so nothing of the kind in the log means nothing asked
#[cfg(target_os = "linux")]
#[test]
fn the_programs_own_window_comes_up_without_loading_the_vision_pack() {
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures/configs/fresh-install");
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_telescope-rs")).arg("config-snapshot").arg(fixture).env("RUST_LOG", "info").output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let log = String::from_utf8_lossy(&out.stderr);
    assert!(!log.contains("Vision isn't there") && !log.contains("MediaPipe"), "{log}");
}
