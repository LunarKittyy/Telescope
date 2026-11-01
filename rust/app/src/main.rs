// No console window behind the app, as telescope.spec's console=False; a debug build keeps one for its logs
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

//! telescope-rs: the Rust rewrite of the Telescope desktop app, wiring every crate together.
//!
//!   telescope-rs                          the window
//!   telescope-rs pair [--usb]             pair a phone from the terminal (QR code, or the USB offer over adb)
//!   telescope-rs stream                   stream without a window, printing stats each second
//!   telescope-rs control JSON             send one /v1/control body to a streaming phone and show the state it changed
//!   telescope-rs snapshot OUT             save the window as a PPM once it has drawn, for comparing with the Qt one
//!   telescope-rs config-snapshot FOLDER   what this app makes of a config fixture, in the shape of its expected.json
//!   telescope-rs scenes OUT [ids]         parity/render_scenes.py's scenes, drawn offscreen for compare_scenes.py
//!   telescope-rs camera-wait-screen OUT   the Windows 11 camera's built-in wait screen (scripts/write_camera_wait_screen.py)
//!   telescope-rs build-info               version, build, channel and commit as JSON, for the package checks
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

/// The translations built into the program (build.rs).
mod locale {
    include!(concat!(env!("OUT_DIR"), "/locale.rs"));
}

use std::path::PathBuf;
use std::sync::Arc;
use telescope_core::config::DEV_PROFILE_ENV;
use telescope_core::services::Services;

/// Until the rewrite replaces the Python app, a source build keeps its settings in its own profile unless asked not to. A stamped
/// release is the real app: it takes over the real settings, phones and cameras.
fn use_own_profile() {
    let release = telescope_system::version::channel() != "dev";
    if release || std::env::var_os(DEV_PROFILE_ENV).is_some() || std::env::var("TELESCOPE_RS_REAL_CONFIG").as_deref() == Ok("1") {
        return;
    }
    let folder = std::env::var_os("TELESCOPE_RS_PROFILE").map(PathBuf::from)
        .unwrap_or_else(|| telescope_core::config::PathEnv::current().own_profile_folder());
    // Only this thread runs yet, so changing the environment can't race anything
    std::env::set_var(DEV_PROFILE_ENV, folder);
}

/// The language picked in Settings > General ("" for the system's), read before any window exists (main.py's _chosen_language).
fn chosen_language() -> String {
    let cfg = telescope_core::config::ConfigFile::default().load();
    cfg["plugin_configs"]["setup"]["language"].as_str().unwrap_or_default().to_string()
}

/// Loads the language before any window or dialog is built, from the catalogs in the program (TELESCOPE_LOCALE_DIR names a folder of
/// .po files to try instead), once the profile is settled.
fn start_language() {
    register_languages();
    telescope_core::i18n::start(&chosen_language());
}

fn register_languages() {
    telescope_core::i18n::use_source(telescope_core::i18n::Source::Embedded { template: locale::TEMPLATE, catalogs: locale::CATALOGS });
}

type Splash = Arc<dyn telescope_system::splash::SplashWindow>;

/// The Windows start-up splash with its text in the person's language, for a start with a window; none elsewhere (main.py's splash.show_text).
fn start_splash() -> Option<Splash> {
    #[cfg(windows)]
    {
        use telescope_system::{splash, splash_windows};
        use_own_profile();
        register_languages();
        let window: Splash = splash_windows::Splash::open()?;
        splash::show_text(Some(window.clone()), chosen_language, splash_windows::since_launch(), splash::WHY_AFTER);
        Some(window)
    }
    #[cfg(not(windows))]
    None
}

fn close_splash(splash: &Option<Splash>) {
    if let Some(splash) = splash {
        splash.close();
    }
}

/// Points the sign-in entry at this copy; a source build or a copy on a profile of its own mustn't repoint the real app's (main.py).
fn refresh_sign_in_entry(source_build: bool, command: &[String], place: &telescope_system::autostart::Place) -> bool {
    owns_the_entries(source_build) && telescope_system::autostart::refresh(command, place)
}

/// Linux: the app menu entry follows this copy too, under the same rule (main.py's update_menu_entry). data is the data home, None for the real one.
fn refresh_menu_entry(source_build: bool, command: &[String], data: Option<&std::path::Path>) -> bool {
    owns_the_entries(source_build) && telescope_system::autostart::refresh_menu_entry(command, data)
}

/// Only a release build on the real profile may repoint the sign-in and menu entries.
fn owns_the_entries(source_build: bool) -> bool {
    // The own profile counts as a dev profile, so it has to be chosen before asking
    use_own_profile();
    !source_build && telescope_core::config::dev_profile().is_none()
}

/// The Updates plugin, with the adb server stopped before an install replaces the folder it runs from.
fn updates_plugin() -> telescope_system::updates_plugin::UpdatesPlugin {
    use telescope_system::updates_plugin::{RealBackend, UpdatesPlugin};
    let mut backend = RealBackend::default();
    backend.stop_adb = Some(Arc::new(telescope_link::adb::stop_adb_server));
    UpdatesPlugin::new(Arc::new(backend), telescope_system::version::channel())
}

/// Browser camera on web's addresses, with its certificate next to the settings file.
fn browser_camera(web: Arc<dyn telescope_core::web::Web>, media: Arc<dyn telescope_core::media::Media>) -> telescope_link::browser::plugin::BrowserCameraPlugin {
    let folder = telescope_core::config::config_path().parent().map(std::path::Path::to_path_buf).unwrap_or_default();
    let server = telescope_link::browser::plugin::system_server(web.clone(), folder, media);
    telescope_link::browser::plugin::BrowserCameraPlugin::new(server, move || web.addresses())
}

/// What the plugins reach this computer through: all of it, or none of it for config-snapshot, which only wants their settings.
struct Machine {
    outputs: Arc<dyn telescope_core::output::Outputs>,
    web: Arc<dyn telescope_core::web::Web>,
    quiet: bool,
}

impl Machine {
    fn real(device: Option<String>) -> Machine {
        Machine { outputs: Arc::new(telescope_outputs::VirtualCameras { device }), web: Arc::new(telescope_link::web::SystemWeb), quiet: false }
    }

    /// As config_fixtures.py's quiet_system and quiet_plugin leave the Python window.
    fn quiet() -> Machine {
        Machine { outputs: Arc::new(cli::NoCameras), web: Arc::new(cli::NoServers), quiet: true }
    }

    fn webcam(&self) -> telescope_outputs::webcam::WebcamPlugin {
        use telescope_outputs::webcam::{System, WebcamPlugin};
        if self.quiet { WebcamPlugin::new(Box::new(|| Ok(Vec::new())), Box::new(|_| None), System::default()) } else { WebcamPlugin::system() }
    }

    fn shortcuts(&self, remote: telescope_system::shortcuts::RemoteActions) -> telescope_system::shortcuts::ShortcutsPlugin {
        use telescope_system::shortcuts::ShortcutsPlugin;
        if self.quiet { ShortcutsPlugin::with_backend(remote, Box::new(|_| None)) } else { ShortcutsPlugin::new(remote) }
    }

    fn browser(&self, media: Arc<dyn telescope_core::media::Media>) -> telescope_link::browser::plugin::BrowserCameraPlugin {
        let browser = browser_camera(self.web.clone(), media);
        if self.quiet { browser.with_clock(|| cli::SNAPSHOT_NOW) } else { browser }
    }
}

/// tray: the window's run, which shows the tray icon.
fn services(machine: Machine, remote: telescope_system::shortcuts::RemoteActions, tray: bool) -> Arc<Services> {
    let link = Arc::new(telescope_link::LinkService::new());
    let problems = telescope_core::services::PluginProblems::default();
    // Added plugins can't take a built-in's name
    let reserved: Vec<String> = vec!["camera_control".into(), "onboarding".into(), "connection".into(), "browser_camera".into(), "microphone".into(), "stream_output".into(), "webcam".into(), "transforms".into(), "colour_grading".into(), "notes".into(), "name_tag".into(), "composite".into(), "desk".into(), "chroma_key".into(), "fun_pack".into(), "presets".into(), "wait_screen".into(), "startup".into(), "photos".into(), "qr_reader".into(), "on_air".into(), "cinema".into(), "background".into(), "privacy_blur".into(), "framing".into(), "face_exposure".into(), "setup".into(), "monitoring".into()];
    let admin: Arc<dyn telescope_core::plugin_admin::PluginAdmin> = Arc::new(telescope_plugin_host::admin::Admin::app(reserved.clone()));
    let added = telescope_plugin_host::load::factory(reserved, telescope_core::i18n::language(), problems.clone());
    let media: Arc<dyn telescope_core::media::Media> = Arc::new(telescope_media::FfmpegMedia);
    let pictures = telescope_effects::shared::picture_reader(media.clone());
    let browser_media = media.clone();
    let connection = link.clone();
    let mic_link: Arc<dyn telescope_core::link::Link> = link.clone();
    let outputs = machine.outputs.clone();
    let sign_outputs = outputs.clone();
    let setup_media = media.clone();
    let languages = telescope_setup::setup::Language::found(&[]);
    Arc::new(Services {
        link,
        media,
        outputs: outputs.clone(),
        audio: (!machine.quiet).then(|| Arc::new(telescope_audio::VirtualMic::default()) as _),
        notifier: Some(Arc::new(telescope_system::Desktop)),
        autostart: Some(Arc::new(telescope_system::Desktop)),
        updater: Some(Arc::new(telescope_system::Desktop)),
        tray: if tray { telescope_system::system_tray().map(std::sync::Mutex::new) } else { None },
        plugin_problems: problems,
        plugin_admin: Some(admin),
        vision: Some(Arc::new(telescope_vision::VisionHost::system())),
        plugins: Box::new(move || {
            let browser = machine.browser(browser_media.clone());
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
                Box::new(machine.webcam()),
                Box::new(telescope_effects::cinema::CinemaPlugin::new()),
                Box::new(telescope_effects::background::BackgroundPlugin::with_reader(pictures.clone())),
                Box::new(telescope_effects::face_exposure::FaceExposurePlugin::new()),
                Box::new(telescope_effects::chroma_key::ChromaKeyPlugin::with_reader(pictures.clone())),
                Box::new(telescope_effects::fun_pack::FunPackPlugin::new()),
                Box::new(telescope_effects::privacy_blur::PrivacyBlurPlugin::new()),
                Box::new(telescope_effects::colour_grading::ColourGradingPlugin::new()),
                Box::new(telescope_outputs::stream_output::StreamOutputPlugin::new(telescope_media::ffmpeg_available())),
                Box::new(telescope_effects::transforms::TransformsPlugin::new()),
                Box::new(telescope_effects::framing::FramingPlugin::new()),
                Box::new(telescope_effects::desk::DeskPlugin::new()),
                Box::new(telescope_effects::name_tag::NameTagPlugin::new()),
                Box::new(telescope_effects::notes::NotesPlugin::new()),
                Box::new(telescope_effects::composite_plugin::CompositePlugin::with(pictures.clone(), Some(outputs.clone()))),
                Box::new(telescope_audio::plugin::MicrophonePlugin::with_link(mic_link.clone()).with_source_audio(browser_mics)),
                Box::new(updates_plugin()),
                Box::new(telescope_outputs::wait_screen_plugin::WaitScreenPlugin::new(outputs.clone())),
                Box::new(machine.shortcuts(remote.clone())),
                Box::new(telescope_system::startup::StartupPlugin::new(Arc::new(telescope_system::Desktop))),
                Box::new(telescope_effects::photos::PhotosPlugin::new()),
                Box::new(telescope_effects::qr_reader::QrReaderPlugin::new()),
                Box::new(telescope_effects::on_air_plugin::OnAirPlugin::new(machine.web.clone(), Some(sign_outputs.clone()))),
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
fn take_lock(wait: bool, remote: &telescope_system::shortcuts::RemoteActions, splash: &Option<Splash>) -> Option<()> {
    use telescope_system::single_instance as si;
    let address = si::Address::current();
    let acquired = si::acquire(&address, std::time::Duration::from_secs(if wait { 15 } else { 0 }));
    let si::Acquired::Lock(lock) = acquired else {
        close_splash(splash);
        if let Some((title, text)) = si::notice(&acquired, &address) {
            // Quitting without a word looks like a crash when the lock is held by something that isn't Telescope
            eprintln!("{title}: {text}");
            if let Err(e) = telescope_shell::notice::show(&title, &text) {
                log::warn!("Couldn't show the message: {e}");
            }
        }
        return None;
    };
    // Straight away: an --action from a sign-in script mustn't find the last run's key
    let key = si::write_control_key(&si::control_key_path(&telescope_core::config::config_path()));
    let remote = remote.clone();
    si::listen(lock, telescope_shell::window::raise, move |request| remote.handle(request), key);
    Some(())
}

/// Linux: a source left by a crashed run goes, unless another copy shares that source's name and may be using it.
fn remove_stale_mic() {
    #[cfg(target_os = "linux")]
    {
        use telescope_audio::virtual_mic::{remove_stale_source, run_within, source};
        if telescope_system::single_instance::sole_user_of(source()) {
            remove_stale_source(&|cmd| run_within(cmd, std::time::Duration::from_secs(2)));
        }
    }
}

/// The folder this program runs from, which an update replaces (main.py's APP_DIR).
fn app_dir() -> PathBuf {
    std::env::current_exe().ok().and_then(|p| p.parent().map(PathBuf::from)).unwrap_or_default()
}

/// $XDG_DATA_HOME, or ~/.local/share.
fn data_home() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or_else(|| telescope_core::config::home().join(".local").join("share"))
}

/// The new version has started fine this long after its window came up (or once it's closed normally).
const STARTED_FINE: std::time::Duration = std::time::Duration::from_secs(3);

/// main.py's started_fine: stop being ready to roll this version back, and delete what earlier ones left.
fn started_fine() {
    static DONE: std::sync::Once = std::sync::Once::new();
    DONE.call_once(|| {
        let dir = app_dir();
        telescope_system::update_guard::confirm(&dir);
        if let Some(venv) = telescope_system::update_guard::python_venv_left(&dir, Some(telescope_system::version::build()), &data_home()) {
            // Tens of thousands of files, and nothing waits for them
            std::thread::spawn(move || telescope_system::update_guard::remove(&venv));
        }
        // Every start, not just after an update: an old version's files can still be locked the first time
        telescope_system::update_guard::clean_up_after_update(&dir, Some(telescope_system::version::build()));
        telescope_system::setup_move::note_installed_version(&dir);
        // Only now: had this copy failed to start, the old one would still be there to use
        let moved_from = MOVED_FROM.get().cloned().flatten().or_else(|| telescope_system::setup_move::pending_move(&dir));
        if let Some(old) = moved_from {
            // Waited for at the end of main, so quitting straight away still lets it finish (a few seconds at most)
            let tidying = std::thread::spawn(move || {
                telescope_system::setup_move::finish_move(&old, &dir, &telescope_system::setup_move::desktop_folders());
            });
            *TIDYING.lock().unwrap_or_else(|e| e.into_inner()) = Some(tidying);
        }
    });
}

/// Removing the replaced copy, which a process that's quitting has to wait for (a non-daemon thread in Python).
static TIDYING: std::sync::Mutex<Option<std::thread::JoinHandle<()>>> = std::sync::Mutex::new(None);

/// What only the copy holding the lock may do, after taking it (main.py): a command or a duplicate launch never gets here.
fn own_the_lock(take: impl FnOnce() -> Option<()>, clean_up: impl FnOnce()) -> Option<()> {
    take()?;
    clean_up();
    Some(())
}

/// dev_desktop.py --plugin: calls changed when one of the plugin's files is saved, so the change shows right away (_watch_dev_plugins).
fn watch_dev_plugins(folders: Vec<PathBuf>, every: std::time::Duration, changed: impl FnOnce() + Send + 'static) {
    use telescope_core::dev_profile::files_stamp;
    if folders.is_empty() {
        return;
    }
    let last = files_stamp(&folders);
    std::thread::spawn(move || loop {
        std::thread::sleep(every);
        if files_stamp(&folders) != last {
            let names: Vec<String> = folders.iter().filter_map(|f| f.file_name()).map(|n| n.to_string_lossy().into_owned()).collect();
            eprintln!("Saw a change in {}", names.join(", "));
            changed();
            return;
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
    let joined = format!("{name}=");
    if let Some(i) = words.iter().position(|w| w.starts_with(&joined)) {
        return Some(words.remove(i).split_off(joined.len()));
    }
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

/// The terminal this was started from, if any: a windows-subsystem program has none of its own, and `pair` or `--list-actions` print to it.
#[cfg(windows)]
fn use_parent_console() {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn AttachConsole(process: u32) -> i32;
    }
    const ATTACH_PARENT_PROCESS: u32 = u32::MAX;
    // SAFETY: takes no pointers, and failing just means there's nothing to print to
    unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
}

/// The main thread's COM mode before anything picks one: winit panics unless it's OLE's, and the Webcam scan and the sound
/// libraries that run here first ask for the multithreaded one, which then stays as it is.
#[cfg(windows)]
fn use_window_apartment() {
    #[link(name = "ole32")]
    unsafe extern "system" {
        fn OleInitialize(reserved: *mut std::ffi::c_void) -> i32;
    }
    // SAFETY: the reserved pointer must be null, and this is the program's first COM call on this thread
    unsafe { OleInitialize(std::ptr::null_mut()) };
}

fn main() {
    #[cfg(windows)]
    {
        use_parent_console();
        use_window_apartment();
    }
    use telescope_system::{memory, update_guard, version};
    telescope_shell::window::use_app_hooks(telescope_shell::window::AppHooks {
        build: version::display_version,
        commit: version::commit,
        release_memory: memory::release_freed_memory,
        relaunch: update_guard::launch,
    });
    // Before the decoder and the stream start their threads, which would each get a pool of their own
    telescope_system::memory::limit_malloc_arenas(4);
    telescope_system::update_guard::confirm_on_terminate(app_dir());
    start_logging();
    #[cfg(windows)]
    {
        use telescope_system::winapp;
        // Run from inside a zip it would lose its files with the temp folder; installed, it takes the Start menu entry's id
        if winapp::running_from_archive(&app_dir(), &std::env::temp_dir()) {
            // Its words are in the person's language
            use_own_profile();
            start_language();
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
        start_language();
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
        autostart: flag(&mut words, "--autostart") || telescope_core::dev_profile::resume_stream(),
        quit_after: value(&mut words, "--quit-after").and_then(|s| s.parse().ok()),
    };
    let launch = take_launch(&mut words);
    let (minimized, wait_for_lock, after_update) = (launch.minimized, launch.after_update || launch.restarted, launch.after_update);
    let _ = MOVED_FROM.set(launch.moved_from);
    let remote = telescope_system::shortcuts::RemoteActions::default();
    // A release that lost its folder, like an exe dragged out of the zip, can't draw anything (the launcher's check in build_bootloader.py)
    #[cfg(windows)]
    if words.is_empty() && telescope_system::winapp::left_its_folder(&app_dir(), telescope_system::version::channel()) {
        use_own_profile();
        start_language();
        telescope_system::winapp::warn_running_from_archive();
        std::process::exit(1);
    }
    // Before the window: an update cut short is finished, or one that never started is rolled back
    if words.is_empty() && !telescope_system::update_guard::guard_start(&app_dir(), after_update) {
        return;
    }
    // Once an update is settled, and before the rest loads, which is most of a first start's wait
    let splash = if words.is_empty() { start_splash() } else { None };
    if words.is_empty() {
        use telescope_system::autostart;
        let source_build = cfg!(debug_assertions) || autostart::is_dev_checkout(telescope_system::version::channel());
        refresh_menu_entry(source_build, &autostart::launch_command(&app_dir(), false), None);
        // A copy that doesn't own the menu entry still needs a hidden one for Wayland to find the window's icon
        let app_id = telescope_system::shortcuts::portal_app_id();
        if cfg!(target_os = "linux") {
            autostart::ensure_portal_entry(&app_id, &autostart::launch_command(&app_dir(), false), None);
        }
        telescope_shell::window::use_app_id(app_id);
        refresh_sign_in_entry(source_build, &autostart::launch_command(&app_dir(), true), &autostart::here());
    }
    telescope_shell::preview::use_gpu_painter(Arc::new(|device, queue| {
        // The second connection to the card opens here, before anything draws: opening it later, from a stream's thread, rescans the Vulkan drivers under the window's rendering
        let _ = telescope_media::gpu::Gpu::shared();
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
    if command == Some("build-info") {
        use telescope_system::version;
        println!("{}", serde_json::json!({"version": version::version(), "build": version::build(), "channel": version::channel(), "commit": version::commit()}));
        return;
    }
    if command == Some("camera-wait-screen") {
        let out = words.get(1).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("wait_screen.bin"));
        if let Err(e) = telescope_outputs::wait_screen::camera_wait_screen().and_then(|data| std::fs::write(&out, data).map_err(|e| e.to_string())) {
            eprintln!("{e}");
            std::process::exit(1);
        }
        return;
    }
    if command != Some("config-snapshot") {
        use_own_profile();
        start_diagnostics();
        start_language();
    }
    let result = match command {
        Some("config-snapshot") => {
            let done = cli::snapshot_profile().and_then(|scratch| cli::config_snapshot(words.get(1).map(String::as_str), services(Machine::quiet(), remote, false), &scratch));
            // The window's threads have nothing to finish
            std::process::exit(match done {
                Ok(()) => 0,
                Err(e) => {
                    eprintln!("{e}");
                    1
                }
            });
        }
        Some("pair") => cli::pair(&args, &services(Machine::real(None), remote, false)),
        Some("stream") => cli::stream(&args, &services(Machine::real(args.device.clone()), remote, false)),
        Some("control") => cli::control(&args, &services(Machine::real(None), remote, false), words.get(1).map(String::as_str).unwrap_or("{}")),
        Some("snapshot") => {
            let out = words.get(1).cloned().unwrap_or_else(|| "snapshot.ppm".into());
            telescope_shell::run(services(Machine::real(args.device.clone()), remote, false), args.window(Some(out), false, None))
        }
        Some(other) => Err(format!("Unknown command {other}")),
        None => match own_the_lock(|| take_lock(wait_for_lock, &remote, &splash), remove_stale_mic) {
            Some(()) => {
                watch_dev_plugins(telescope_plugin_host::package::dev_plugin_folders(), std::time::Duration::from_millis(700), telescope_shell::window::restart);
                let confirm: Arc<dyn Fn() + Send + Sync> = Arc::new(started_fine);
                confirm::run_confirming(STARTED_FINE, confirm, |shown| {
                    // The splash goes once the window shows (or is put in the tray)
                    let (inner, gone) = (shown.0.clone(), splash.clone());
                    let shown = telescope_shell::ShownHook(Arc::new(move || {
                        close_splash(&gone);
                        inner();
                    }));
                    telescope_shell::run(services(Machine::real(args.device.clone()), remote, true), args.window(None, minimized, Some(shown)))
                })
            }
            None => Ok(()),
        },
    };
    close_splash(&splash);
    let tidying = TIDYING.lock().unwrap_or_else(|e| e.into_inner()).take();
    if let Some(tidying) = tidying {
        let _ = tidying.join();
    }
    if let Err(e) = result {
        eprintln!("{e}");
        std::process::exit(1);
    }
    let code = telescope_shell::window::exit_code();
    if code != 0 {
        std::process::exit(code);
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
        #[cfg(target_os = "linux")]
        the_menu_entry_is_the_real_apps_to_write_only_on_the_real_profile();
    }

    // main.py writes the app menu entry only where it repoints the sign-in one: never for a source build, a dev profile or the own profile
    // (run by the sign-in entry's test, since both set the profile in the environment)
    #[cfg(target_os = "linux")]
    fn the_menu_entry_is_the_real_apps_to_write_only_on_the_real_profile() {
        let own = [DEV_PROFILE_ENV, "TELESCOPE_RS_PROFILE", "TELESCOPE_RS_REAL_CONFIG"];
        let data = telescope_core::testing::scratch_dir("menu");
        let menu = telescope_system::autostart::menu_file(Some(&data));
        let command = vec!["/opt/rs/telescope-rs".to_string()];
        own.iter().for_each(|name| std::env::remove_var(name));
        // The own profile (the default) and a source build leave the real app's entry alone
        std::env::set_var("TELESCOPE_RS_PROFILE", data.join("profile"));
        assert!(!refresh_menu_entry(false, &command, Some(&data)));
        own.iter().for_each(|name| std::env::remove_var(name));
        std::env::set_var("TELESCOPE_RS_REAL_CONFIG", "1");
        assert!(!refresh_menu_entry(true, &command, Some(&data)));
        assert!(!menu.exists());
        // A release build on the real profile writes the entry and its icon
        assert!(refresh_menu_entry(false, &command, Some(&data)));
        assert!(std::fs::read_to_string(&menu).unwrap().contains("Exec=/opt/rs/telescope-rs\n"));
        assert!(telescope_system::autostart::icon_file(Some(&data)).exists());
        own.iter().for_each(|name| std::env::remove_var(name));
    }

    // A shipped build has no folder of catalogs: they're in the program, all of them, and they read like the files do
    #[test]
    fn the_translations_are_in_the_program() {
        use telescope_core::i18n::Source;
        let embedded = Source::Embedded { template: locale::TEMPLATE, catalogs: locale::CATALOGS };
        let folder = Source::Folder(std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../desktop/telescope/locale")).to_path_buf());
        assert_eq!(embedded.available(), folder.available());
        assert_eq!(embedded.available().len(), 16);
        for lang in embedded.available().iter().map(String::as_str).chain(["pseudo"]) {
            let (a, b) = (embedded.catalog(lang).unwrap(), folder.catalog(lang).unwrap());
            assert_eq!(a.len(), b.len(), "{lang}");
            assert!(a.len() > 1700, "{lang} has only {} texts", a.len());
            assert_eq!(embedded.is_draft(lang), folder.is_draft(lang), "{lang}");
        }
    }

    // test_main.py: a command or a duplicate launch changes nothing, and the one that gets the lock cleans up only after it has it
    #[test]
    fn only_the_copy_with_the_lock_touches_what_the_running_copy_owns() {
        let cleaned = std::cell::Cell::new(false);
        assert_eq!(own_the_lock(|| None, || cleaned.set(true)), None);
        assert!(!cleaned.get(), "a duplicate launch leaves it alone");
        let owned = std::cell::Cell::new(false);
        assert_eq!(own_the_lock(|| { owned.set(true); Some(()) }, || { assert!(owned.get()); cleaned.set(true); }), Some(()));
        assert!(cleaned.get());
    }

    // test_splash.py::test_the_splash_text_is_in_the_chosen_language's last step: the app's own close reaches the splash, and none is no splash
    #[test]
    fn the_app_closes_the_splash_it_opened() {
        #[derive(Default)]
        struct Fake(std::sync::atomic::AtomicBool);

        impl telescope_system::splash::SplashWindow for Fake {
            fn update_text(&self, _text: &str) {}
            fn close(&self) {
                self.0.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }

        let fake = Arc::new(Fake::default());
        close_splash(&Some(fake.clone() as Splash));
        assert!(fake.0.load(std::sync::atomic::Ordering::SeqCst));
        close_splash(&None);
    }

    #[test]
    fn saving_a_plugins_file_restarts_the_dev_copy() {
        let dir = telescope_core::testing::scratch_dir("dev-plugin");
        std::fs::write(dir.join("plugin.lua"), "one").unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        watch_dev_plugins(vec![dir.clone()], std::time::Duration::from_millis(10), move || tx.send(()).unwrap());
        assert!(rx.recv_timeout(std::time::Duration::from_millis(100)).is_err(), "nothing changed yet");
        std::fs::write(dir.join("plugin.lua"), "changed").unwrap();
        rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        // No folders, no watching
        watch_dev_plugins(Vec::new(), std::time::Duration::from_millis(1), || panic!("nothing to watch"));
    }

    #[test]
    fn minimized_and_moved_from_come_off_the_words_like_argparse_takes_them() {
        let mut words: Vec<String> = ["--minimized", "--moved-from=/old/Telescope", "-platform", "x"].map(String::from).to_vec();
        let launch = take_launch(&mut words);
        assert!(launch.minimized);
        assert_eq!(launch.moved_from, Some(PathBuf::from("/old/Telescope")));
        assert_eq!(words, ["-platform", "x"]);
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
