//! telescope-rs: the Rust rewrite of the Telescope desktop app, wiring every crate together.
//!
//!   telescope-rs                          the window
//!   telescope-rs pair [--usb]             pair a phone from the terminal (QR code, or the USB offer over adb)
//!   telescope-rs stream                   stream without a window, printing stats each second
//!   telescope-rs control JSON             send one /v1/control body to a streaming phone and show the state it changed
//!   telescope-rs snapshot OUT             save the window as a PPM once it has drawn, for comparing with the Qt one
//!   telescope-rs config-snapshot FOLDER   what this app makes of a config fixture, in the shape of its expected.json
//!   telescope-rs scenes OUT [ids]         parity/render_scenes.py's scenes, drawn offscreen for compare_scenes.py
//!   telescope-rs dev [--keep] [--reset] [--plugin FOLDER ...]   the dev desktop, in a profile of its own (dev.rs)
//!
//! --autostart starts streaming once the window is up and --quit-after SECS closes it, for measuring.
//!
//! main.py's own options: --action ID [NAME=VALUE ...] runs a shortcut action in the running copy, --list-actions
//! lists them, --minimized starts in the tray, --after-update and --restarted wait for the old copy to let go of
//! the lock, --moved-from FOLDER is the unzipped copy the installed one replaced, which it removes once it has started fine.

mod cli;
mod confirm;
mod dev;
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
    let folder = std::env::var_os("TELESCOPE_RS_PROFILE").map(PathBuf::from)
        .unwrap_or_else(|| telescope_core::config::PathEnv::current().own_profile_folder());
    // Only this thread runs yet, so changing the environment can't race anything
    std::env::set_var(DEV_PROFILE_ENV, folder);
}

/// Points the sign-in entry at this copy; a source build or a copy on a profile of its own mustn't repoint the real app's (main.py).
fn refresh_sign_in_entry(source_build: bool, command: &[String], place: &telescope_system::autostart::Place) -> bool {
    // The own profile counts as a dev profile, so it has to be chosen before asking
    use_own_profile();
    !source_build && telescope_core::config::dev_profile().is_none() && telescope_system::autostart::refresh(command, place)
}

/// The Updates plugin, with the adb server stopped before an installer replaces the folder it runs from.
fn updates_plugin() -> telescope_system::updates_plugin::UpdatesPlugin {
    use telescope_system::updates_plugin::{RealBackend, UpdatesPlugin};
    let mut backend = RealBackend::default();
    backend.before_setup = Some(Arc::new(telescope_link::adb::stop_adb_server));
    UpdatesPlugin::new(Arc::new(backend), telescope_system::version::channel())
}

/// Browser camera on this computer's addresses, with its certificate next to the settings file.
fn browser_camera(media: Arc<dyn telescope_core::media::Media>) -> telescope_link::browser::plugin::BrowserCameraPlugin {
    use telescope_core::web::Web;
    let web: Arc<dyn Web> = Arc::new(telescope_link::web::SystemWeb);
    let folder = telescope_core::config::config_path().parent().map(std::path::Path::to_path_buf).unwrap_or_default();
    let server = telescope_link::browser::plugin::system_server(web, folder, media);
    telescope_link::browser::plugin::BrowserCameraPlugin::new(server, || telescope_link::web::SystemWeb.addresses())
}

/// tray: the window's run, which shows the tray icon.
fn services(device: Option<String>, remote: telescope_system::shortcuts::RemoteActions, tray: bool) -> Arc<Services> {
    let link = Arc::new(telescope_link::LinkService::new());
    let problems = telescope_core::services::PluginProblems::default();
    // Added plugins can't take a built-in's name; "en" until the shell loads languages
    let added = telescope_plugin_host::load::factory(vec!["camera_control".into(), "onboarding".into(), "connection".into(), "browser_camera".into(), "microphone".into(), "stream_output".into(), "webcam".into(), "transforms".into(), "colour_grading".into(), "notes".into(), "name_tag".into(), "composite".into(), "desk".into(), "chroma_key".into(), "fun_pack".into(), "presets".into(), "wait_screen".into(), "startup".into(), "photos".into(), "qr_reader".into(), "on_air".into(), "cinema".into(), "background".into(), "privacy_blur".into(), "framing".into(), "face_exposure".into(), "setup".into(), "monitoring".into()], "en".into(), problems.clone());
    let media: Arc<dyn telescope_core::media::Media> = Arc::new(telescope_media::FfmpegMedia);
    let pictures = telescope_effects::shared::picture_reader(media.clone());
    let browser_media = media.clone();
    let connection = link.clone();
    let mic_link: Arc<dyn telescope_core::link::Link> = link.clone();
    let outputs: Arc<dyn telescope_core::output::Outputs> = Arc::new(telescope_outputs::VirtualCameras { device });
    let sign_outputs = outputs.clone();
    let setup_media = media.clone();
    // Until the app ships its own translations, a dev build lists the Python app's
    let locale = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../desktop/telescope/locale"));
    let languages = telescope_setup::setup::Language::found(&[locale]);
    Arc::new(Services {
        link,
        media,
        outputs: outputs.clone(),
        audio: Some(Arc::new(telescope_audio::VirtualMic::default())),
        notifier: Some(Arc::new(telescope_system::Desktop)),
        autostart: Some(Arc::new(telescope_system::Desktop)),
        updater: Some(Arc::new(telescope_system::Desktop)),
        tray: if tray { telescope_system::system_tray().map(std::sync::Mutex::new) } else { None },
        plugin_problems: problems,
        vision: Some(Arc::new(telescope_vision::VisionHost::system())),
        plugins: Box::new(move || {
            let browser = browser_camera(browser_media.clone());
            let browser_mics = Arc::new(browser.mics());
            let mut plugins: Vec<Box<dyn telescope_core::plugin::Plugin>> = vec![
                // In the order of plugins/__init__.py, which is also the order frames go through them within a stage
                Box::new({
                    let mut setup = telescope_setup::setup::SetupPlugin::new(outputs.clone(), mic_link.clone(), setup_media.clone(), &telescope_system::version::display_version(), languages.clone());
                    // Open log is there once the desktop log is (diagnostics.events.path)
                    setup.log_path = telescope_core::diagnostics::events().path().map(|p| p.display().to_string());
                    setup
                }),
                Box::new(connection.connection_plugin().with_outputs(outputs.clone())),
                Box::new(browser),
                Box::new(telescope_outputs::webcam::WebcamPlugin::system()),
                Box::new(telescope_effects::cinema::CinemaPlugin::new()),
                Box::new(telescope_effects::background::BackgroundPlugin::with_reader(pictures.clone())),
                Box::new(telescope_effects::face_exposure::FaceExposurePlugin::new()),
                Box::new(telescope_effects::chroma_key::ChromaKeyPlugin::with_reader(pictures.clone())),
                Box::new(telescope_effects::fun_pack::FunPackPlugin::new()),
                Box::new(telescope_effects::privacy_blur::PrivacyBlurPlugin::new()),
                Box::new(telescope_effects::colour_grading::ColourGradingPlugin::new()),
                Box::new(telescope_outputs::stream_output::StreamOutputPlugin::new(true)),
                Box::new(telescope_effects::transforms::TransformsPlugin::new()),
                Box::new(telescope_effects::framing::FramingPlugin::new()),
                Box::new(telescope_effects::desk::DeskPlugin::new()),
                Box::new(telescope_effects::name_tag::NameTagPlugin::new()),
                Box::new(telescope_effects::notes::NotesPlugin::new()),
                Box::new(telescope_effects::composite_plugin::CompositePlugin::with(pictures.clone(), Some(outputs.clone()))),
                Box::new(telescope_audio::plugin::MicrophonePlugin::with_link(mic_link.clone()).with_source_audio(browser_mics)),
                Box::new(updates_plugin()),
                Box::new(telescope_outputs::wait_screen_plugin::WaitScreenPlugin::new(outputs.clone())),
                Box::new(telescope_system::shortcuts::ShortcutsPlugin::new(remote.clone())),
                Box::new(telescope_system::startup::StartupPlugin::new(Arc::new(telescope_system::Desktop))),
                Box::new(telescope_effects::photos::PhotosPlugin::new()),
                Box::new(telescope_effects::qr_reader::QrReaderPlugin::new()),
                Box::new(telescope_effects::on_air_plugin::OnAirPlugin::new(Arc::new(telescope_link::web::SystemWeb), Some(sign_outputs.clone()))),
                Box::new(telescope_setup::monitoring::MonitoringPlugin::new()),
            ];
            plugins.extend(added());
            plugins
        }),
    })
}

/// Log lines from every crate to stderr, at the level RUST_LOG names; when unset, warnings from this app's crates and
/// only errors from libraries (wgpu warns about every Vulkan driver it skips).
struct Stderr {
    level: log::LevelFilter,
    quiet_libraries: bool,
}

impl log::Log for Stderr {
    fn enabled(&self, meta: &log::Metadata) -> bool {
        let floor = if self.quiet_libraries && !meta.target().starts_with("telescope") { log::Level::Error } else { log::Level::Trace };
        meta.level() <= self.level && meta.level() <= floor
    }
    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            eprintln!("[{} {}] {}", record.level(), record.target(), record.args());
        }
    }
    fn flush(&self) {}
}

/// The terminal and the desktop log (Copy diagnostics shows its latest lines), which wants Info from this app's crates.
struct Both {
    stderr: Stderr,
    events: &'static telescope_core::diagnostics::EventLog,
}

impl log::Log for Both {
    fn enabled(&self, meta: &log::Metadata) -> bool {
        self.stderr.enabled(meta) || self.events.enabled(meta)
    }
    fn log(&self, record: &log::Record) {
        self.stderr.log(record);
        self.events.log(record);
    }
    fn flush(&self) {}
}

fn start_logging() {
    let asked = std::env::var("RUST_LOG").ok().and_then(|v| v.parse::<log::LevelFilter>().ok());
    let level = asked.unwrap_or(log::LevelFilter::Warn);
    let stderr = Stderr { level, quiet_libraries: asked.is_none() };
    let logger: &'static Both = Box::leak(Box::new(Both { stderr, events: telescope_core::diagnostics::events() }));
    if log::set_logger(logger).is_ok() {
        log::set_max_level(level.max(log::LevelFilter::Info));
    }
}

/// Log to the desktop log file and keep panics in it (diagnostics.install), once the profile is settled.
fn start_diagnostics() {
    let version = telescope_system::version::display_version();
    telescope_core::diagnostics::start(telescope_core::diagnostics::events(), &telescope_core::diagnostics::log_dir(), Some(&app_dir()), &version);
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
    si::listen(lock, telescope_shell::window::raise, move |request| remote.handle(request), key);
    Some(())
}

/// The folder this program runs from, which an update replaces (main.py's APP_DIR).
fn app_dir() -> PathBuf {
    std::env::current_exe().ok().and_then(|p| p.parent().map(PathBuf::from)).unwrap_or_default()
}

/// The new version has started fine this long after its window came up (or once it's closed normally).
const STARTED_FINE: std::time::Duration = std::time::Duration::from_secs(3);

/// main.py's started_fine: stop being ready to roll this version back, and delete what earlier ones left.
fn started_fine() {
    static DONE: std::sync::Once = std::sync::Once::new();
    DONE.call_once(|| {
        let dir = app_dir();
        telescope_system::update_guard::confirm(&dir);
        // Every start, not just after an update: an old version's files can still be locked the first time
        telescope_system::update_guard::clean_up_after_update(&dir, None);
        // Only now: had this copy failed to start, the old one would still be there to use
        let moved_from = MOVED_FROM.get().cloned().flatten().or_else(|| telescope_system::setup_move::pending_move(&dir));
        if let Some(old) = moved_from {
            // Not detached from the process, so quitting straight away still lets it finish (a few seconds at most)
            let _ = std::thread::spawn(move || {
                telescope_system::setup_move::finish_move(&old, &dir, &telescope_system::setup_move::desktop_folders());
            });
        }
    });
}

/// --moved-from: the unzipped folder this installed copy replaced.
static MOVED_FROM: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();

/// A word from the command line taken out, true if it was there.
fn take_flag(words: &mut Vec<String>, name: &str) -> bool {
    let found = words.iter().position(|w| w == name);
    found.map(|i| words.remove(i)).is_some()
}

/// An option and its value taken out, None if it wasn't there or had no value.
fn take_value(words: &mut Vec<String>, name: &str) -> Option<String> {
    let i = words.iter().position(|w| w == name)?;
    words.remove(i);
    (i < words.len()).then(|| words.remove(i))
}

/// main.py's own options (parse_args), taken out of the words; what's left is for the commands.
#[derive(Debug, Default, PartialEq)]
struct Launch {
    minimized: bool,
    /// Started by an update: the old copy may still hold the lock.
    after_update: bool,
    restarted: bool,
    /// The unzipped folder the installed copy replaced, which it removes once it has started fine.
    moved_from: Option<PathBuf>,
}

fn take_launch(words: &mut Vec<String>) -> Launch {
    Launch {
        minimized: take_flag(words, "--minimized"),
        after_update: take_flag(words, "--after-update"),
        restarted: take_flag(words, "--restarted"),
        moved_from: take_value(words, "--moved-from").map(PathBuf::from),
    }
}

fn main() {
    // Before the decoder and the stream start their threads, which would each get a pool of their own
    telescope_system::memory::limit_malloc_arenas(4);
    // Before any other thread starts, so a logout's SIGTERM reaches the thread that confirms a trial
    telescope_system::update_guard::confirm_on_terminate(app_dir());
    start_logging();
    #[cfg(windows)]
    {
        use telescope_system::winapp;
        // Run from inside a zip it would lose its files with the temp folder; installed, it takes the Start menu entry's id
        if winapp::running_from_archive(&app_dir(), &std::env::temp_dir()) {
            winapp::warn_running_from_archive();
            std::process::exit(1);
        }
        winapp::use_installed_app_id(&app_dir());
    }
    let mut words: Vec<String> = std::env::args().skip(1).collect();
    if words.first().map(String::as_str) == Some("dev") {
        std::process::exit(dev::run(&words[1..]));
    }
    // --action takes every word after it
    let action = words.iter().position(|w| w == "--action").map(|i| words.split_off(i).split_off(1));
    let list_actions = words.iter().any(|w| w == "--list-actions");
    if action.is_some() || list_actions {
        use_own_profile();
        std::process::exit(run_action(action));
    }
    let (flag, value) = (take_flag, take_value);
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
    let launch = take_launch(&mut words);
    let (minimized, wait_for_lock, after_update) = (launch.minimized, launch.after_update || launch.restarted, launch.after_update);
    let _ = MOVED_FROM.set(launch.moved_from);
    let remote = telescope_system::shortcuts::RemoteActions::default();
    // Before the window: an update cut short is finished, or one that never started is rolled back
    if words.is_empty() && !telescope_system::update_guard::guard_start(&app_dir(), after_update) {
        return;
    }
    if words.is_empty() {
        use telescope_system::autostart;
        let source_build = cfg!(debug_assertions) || telescope_system::version::channel() == "dev";
        refresh_sign_in_entry(source_build, &autostart::launch_command(&app_dir(), true), &autostart::here());
    }
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
        start_diagnostics();
    }
    let result = match command {
        Some("config-snapshot") => cli::config_snapshot(words.get(1).map(String::as_str)),
        Some("pair") => cli::pair(&args, &services(None, remote, false)),
        Some("stream") => cli::stream(&args, &services(args.device.clone(), remote, false)),
        Some("control") => cli::control(&args, &services(None, remote, false), words.get(1).map(String::as_str).unwrap_or("{}")),
        Some("snapshot") => {
            let out = words.get(1).cloned().unwrap_or_else(|| "snapshot.ppm".into());
            telescope_shell::run(services(args.device.clone(), remote, false), args.window(Some(out), false, None))
        }
        Some(other) => Err(format!("Unknown command {other}")),
        None => match take_lock(wait_for_lock, &remote) {
            Some(()) => {
                let confirm: Arc<dyn Fn() + Send + Sync> = Arc::new(started_fine);
                confirm::run_confirming(STARTED_FINE, confirm, |shown| {
                    telescope_shell::run(services(args.device.clone(), remote, true), args.window(None, minimized, Some(shown)))
                })
            }
            None => Ok(()),
        },
    };
    if let Err(e) = result {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // main.py: no refresh for a dev checkout or a dev profile, and this app is on a profile of its own unless asked not to be
    #[test]
    fn the_sign_in_entry_is_the_real_apps_to_repoint_only_on_the_real_profile() {
        use telescope_system::autostart::{desktop_file, enable, Place};
        let own = [DEV_PROFILE_ENV, "TELESCOPE_RS_PROFILE", "TELESCOPE_RS_REAL_CONFIG"];
        let home = telescope_core::testing::scratch_dir("signin");
        let (python, rust) = (vec!["/opt/telescope/start.sh".to_string(), "--minimized".to_string()], vec!["/opt/rs/telescope-rs".to_string()]);
        let entry = || std::fs::read_to_string(desktop_file(&home)).unwrap();
        let reset = || {
            own.iter().for_each(|name| std::env::remove_var(name));
            enable(&python, &Place::Xdg(home.clone())).unwrap();
        };
        let place = Place::Xdg(home.clone());
        // On its own profile (the default), a release build leaves the Python install's entry alone
        reset();
        std::env::set_var("TELESCOPE_RS_PROFILE", home.join("profile"));
        assert!(!refresh_sign_in_entry(false, &rust, &place));
        assert!(entry().contains("/opt/telescope/start.sh"), "{}", entry());
        // A source build leaves it alone on the real profile too
        reset();
        std::env::set_var("TELESCOPE_RS_REAL_CONFIG", "1");
        assert!(!refresh_sign_in_entry(true, &rust, &place));
        assert!(entry().contains("/opt/telescope/start.sh"));
        // Only a release build on the real profile repoints it
        assert!(refresh_sign_in_entry(false, &rust, &place));
        assert!(entry().contains("/opt/rs/telescope-rs") && !entry().contains("start.sh"));
        own.iter().for_each(|name| std::env::remove_var(name));
    }

    // test_move_to_installed.py::test_main_takes_the_moved_from_folder
    #[test]
    fn main_takes_the_moved_from_folder() {
        let mut words: Vec<String> = ["--after-update", "--moved-from", r"C:\Users\l\Downloads\Telescope", "-style", "x"].map(String::from).to_vec();
        let launch = take_launch(&mut words);
        assert!(launch.after_update && !launch.restarted && !launch.minimized);
        assert_eq!(launch.moved_from, Some(PathBuf::from(r"C:\Users\l\Downloads\Telescope")));
        assert_eq!(words, ["-style", "x"]);
    }
}
