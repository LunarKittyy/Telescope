//! The Updates plugin (plugins/updates.py): checks a few seconds after launch and then daily, quietly; a newer build shows an Update
//! button in the header. Updating downloads, verifies, swaps the files and restarts, waiting for a running stream to stop first. A copy
//! that can't replace itself gets the release page instead. The network and the install go through a Backend, so tests stand in for them.

use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use telescope_core::event::Event;
use telescope_core::host::{Host, Poster};
use telescope_core::i18n::{n_, tr, tr_ctx};
use telescope_core::plugin::{ConfigRejected, Plugin, PluginInfo};
use telescope_core::ui::{SettingsSection, SlintSource, UiEvent, UiSpec};

use crate::setup_move;
use crate::updates::{self, Asset, InstallResult, Manifest, Updates, Web};
use crate::version;

const UI: &str = include_str!("../ui/updates.slint");
const FIRST_CHECK_MS: u64 = 8_000;
/// How often to see whether a day has passed.
const TICK_MS: u64 = 60 * 60 * 1000;
const CHECK_INTERVAL_S: f64 = 24.0 * 60.0 * 60.0;
const CHANNEL_LABELS: [(&str, &str); 2] = [("stable", n_("Stable")), ("nightly", n_("Nightly"))];
const SECTION: &str = "updates";
const DONE: &str = "done";

pub fn releases_url() -> String {
    format!("https://github.com/{}/releases", version::REPO)
}

/// The network, the install and the outside world, behind a trait so tests stand in for them.
pub trait Backend: Send + Sync {
    fn fetch(&self, channel: &str) -> Result<Option<Manifest>, String>;
    /// Downloads (trying a replaced nightly once more) and installs; the build that went in and what starts it.
    /// progress gets (done, total) while downloading, then None once it's installing.
    fn install(&self, manifest: &Manifest, asset: &Asset, progress: &mut dyn FnMut(Option<(u64, u64)>), cancelled: &dyn Fn() -> bool)
        -> Result<(i64, InstallResult), String>;
    /// Why this copy can't replace itself, or None.
    fn blocker(&self) -> Option<String>;
    fn is_newer(&self, manifest: Option<&Manifest>) -> bool;
    fn was_rolled_back(&self, manifest: Option<&Manifest>) -> bool;
    fn forget_rolled_back(&self);
    fn relaunch(&self, argv: &[String]) -> io::Result<()>;
    fn open_url(&self, url: &str);
    /// Whether checks and installs run on a thread of their own (tests run them straight away).
    fn threaded(&self) -> bool {
        true
    }
    /// An unzipped Windows copy, whose update can go through the installer (setup_move::is_unzipped_copy).
    fn is_unzipped_copy(&self) -> bool {
        false
    }
    /// An older Telescope registered the camera driver from this folder, so removing the folder would take the camera.
    fn driver_in_app_folder(&self) -> bool {
        false
    }
    /// install(), but with the release's TelescopeSetup.exe: it installs for this user and the result starts that copy.
    fn install_via_setup(&self, manifest: &Manifest, asset: &Asset, progress: &mut dyn FnMut(Option<(u64, u64)>), cancelled: &dyn Fn() -> bool)
        -> Result<(i64, InstallResult), String> {
        self.install(manifest, asset, progress, cancelled)
    }
    /// The installed copy is being started from here (mark_started_elsewhere).
    fn started_elsewhere(&self, _build: i64) {}
}

/// This copy, in the folder it runs from.
pub struct RealBackend {
    pub directory: PathBuf,
    pub build: i64,
    pub channel: String,
    /// Run before the installer, which can't replace a folder a running adb.exe holds (stop_adb_server).
    pub before_setup: Option<Arc<dyn Fn() + Send + Sync>>,
    unzipped: std::sync::OnceLock<bool>,
}

impl Default for RealBackend {
    fn default() -> Self {
        RealBackend { directory: crate::app_dir(), build: version::build(), channel: version::channel().into(), before_setup: None, unzipped: Default::default() }
    }
}

impl RealBackend {
    fn with<T>(&self, f: impl FnOnce(&Updates) -> T) -> T {
        let web = Web::default();
        f(&Updates { opener: &web, build: self.build, directory: self.directory.clone() })
    }
}

impl Backend for RealBackend {
    fn fetch(&self, channel: &str) -> Result<Option<Manifest>, String> {
        self.with(|u| u.fetch_manifest(channel)).map_err(|e| e.0)
    }

    fn install(&self, manifest: &Manifest, asset: &Asset, progress: &mut dyn FnMut(Option<(u64, u64)>), cancelled: &dyn Fn() -> bool)
        -> Result<(i64, InstallResult), String> {
        // Our own folder, not a shared /tmp path
        let folder = (0..100).map(|n| std::env::temp_dir().join(format!("telescope-update-{}-{n}", std::process::id())))
            .find(|p| std::fs::create_dir(p).is_ok()).ok_or_else(|| tr("The update failed."))?;
        let done = self.with(|u| u.download_update(manifest, asset, &folder, &mut |d, t| progress(Some((d, t))), cancelled))
            .and_then(|(selected, archive)| {
                progress(None);
                updates::install(&archive, &self.directory, selected.build).map(|r| (selected.build, r))
            });
        let _ = std::fs::remove_dir_all(&folder);
        done.map_err(|e| e.0)
    }

    fn blocker(&self) -> Option<String> {
        updates::self_update_blocker(&self.directory, &self.channel)
    }

    fn is_unzipped_copy(&self) -> bool {
        *self.unzipped.get_or_init(|| setup_move::is_unzipped_copy(&setup_move::Place::of(&self.directory)))
    }

    fn driver_in_app_folder(&self) -> bool {
        setup_move::driver_in_app_folder(&self.directory)
    }

    fn install_via_setup(&self, manifest: &Manifest, asset: &Asset, progress: &mut dyn FnMut(Option<(u64, u64)>), cancelled: &dyn Fn() -> bool)
        -> Result<(i64, InstallResult), String> {
        let folder = (0..100).map(|n| std::env::temp_dir().join(format!("telescope-update-{}-{n}", std::process::id())))
            .find(|p| std::fs::create_dir(p).is_ok()).ok_or_else(|| tr("The update failed."))?;
        let done = self.with(|u| u.download_update(manifest, asset, &folder, &mut |d, t| progress(Some((d, t))), cancelled)).and_then(|(selected, setup)| {
            progress(None);
            if let Some(stop) = &self.before_setup {
                stop();
            }
            let run = |argv: &[String]| setup_move::run_setup(argv, setup_move::SETUP_TIMEOUT);
            let folders = setup_move::desktop_folders();
            let env = setup_move::SetupEnv { run: &run, locate: &setup_move::installed_location, folders: &folders };
            setup_move::install_with_setup(&setup, &self.directory, &env).map(|r| (selected.build, r))
        });
        let _ = std::fs::remove_dir_all(&folder);
        done.map_err(|e| e.0)
    }

    fn started_elsewhere(&self, build: i64) {
        setup_move::mark_started_elsewhere(build, &self.directory);
    }

    fn is_newer(&self, manifest: Option<&Manifest>) -> bool {
        self.with(|u| u.is_newer(manifest))
    }

    fn was_rolled_back(&self, manifest: Option<&Manifest>) -> bool {
        self.with(|u| u.was_rolled_back(manifest))
    }

    fn forget_rolled_back(&self) {
        self.with(|u| u.forget_rolled_back())
    }

    fn relaunch(&self, argv: &[String]) -> io::Result<()> {
        crate::update_guard::launch(argv)
    }

    fn open_url(&self, url: &str) {
        crate::open_url(url);
    }
}

/// What a check or an install handed back from its thread.
enum Done {
    Checked { id: u64, result: Result<Option<Manifest>, String> },
    Progress(u64, u64),
    Installing,
    Installed(Result<(i64, InstallResult), String>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Idle,
    Downloading,
    Installing,
    /// Installed during a stream: restarts once it stops.
    RestartPending,
}

pub struct UpdatesPlugin {
    backend: Arc<dyn Backend>,
    /// The channel this copy was built for (version::channel()); "dev" never checks by itself.
    built_channel: String,
    pub channel: String,
    pub auto_check: bool,
    pub keep_portable: bool,
    last_check: f64,
    pub available: Option<Manifest>,
    /// The last manifest seen, newer or not.
    pub latest: Option<Manifest>,
    /// A newer build held back: it was undone for not starting.
    pub rolled_back: Option<Manifest>,
    checked: bool,
    error: String,
    check_id: u64,
    checking: bool,
    manual: bool,
    phase: Phase,
    relaunch_argv: Option<Vec<String>>,
    progress: (u64, u64),
    cancel: Arc<AtomicBool>,
    done: Arc<Mutex<Vec<Done>>>,
    poster: Option<Arc<dyn Poster>>,
    /// The build being installed, for the folder it replaces to remember if the new copy never starts.
    install_build: i64,
    /// When the camera driver was last looked for in this folder, and what was found: reading the registry once is enough for a while.
    driver_here: Mutex<Option<(std::time::Instant, bool)>>,
}

impl Default for UpdatesPlugin {
    fn default() -> Self {
        Self::new(Arc::new(RealBackend::default()), version::channel())
    }
}

fn now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64())
}

impl UpdatesPlugin {
    pub fn new(backend: Arc<dyn Backend>, built_channel: &str) -> Self {
        UpdatesPlugin {
            backend,
            built_channel: built_channel.into(),
            channel: Self::default_channel_for(built_channel).into(),
            auto_check: true,
            keep_portable: false,
            last_check: 0.0,
            available: None,
            latest: None,
            rolled_back: None,
            checked: false,
            error: String::new(),
            check_id: 0,
            checking: false,
            manual: false,
            phase: Phase::Idle,
            relaunch_argv: None,
            progress: (0, 0),
            cancel: Arc::new(AtomicBool::new(false)),
            done: Arc::new(Mutex::new(Vec::new())),
            poster: None,
            install_build: 0,
            driver_here: Mutex::new(None),
        }
    }

    /// Nightly builds follow nightly; stable and source builds follow stable.
    fn default_channel_for(built: &str) -> &'static str {
        if built == "nightly" { "nightly" } else { "stable" }
    }

    pub fn busy(&self) -> bool {
        self.checking || self.phase != Phase::Idle
    }

    /// Whether updating goes through the installer: an unzipped copy, not kept portable, with a setup to use.
    fn moves_on_update(&self) -> bool {
        let setup_offered = self.available.as_ref().is_some_and(|m| updates::setup_asset(m).is_some());
        setup_move::moves_on_update(self.keep_portable, setup_offered, self.backend.is_unzipped_copy(), || self.driver_in_app_folder())
    }

    /// The backend's answer, remembered for half a minute since one refresh of the window asks more than once.
    fn driver_in_app_folder(&self) -> bool {
        let mut seen = self.driver_here.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, here)) = *seen {
            if at.elapsed() <= Duration::from_secs(30) {
                return here;
            }
        }
        let here = self.backend.driver_in_app_folder();
        *seen = Some((std::time::Instant::now(), here));
        here
    }

    /// Why updating has to go through the release page, or None.
    fn update_blocker(&self) -> Option<String> {
        if self.moves_on_update() { None } else { self.backend.blocker() }
    }

    pub fn status_text(&self, host: &dyn Host) -> (String, &'static str) {
        self.status_while(host.is_streaming())
    }

    fn status_while(&self, streaming: bool) -> (String, &'static str) {
        let with_version = |text: &str, m: &Manifest| tr(text).replace("{version}", &m.display_version());
        match self.phase {
            Phase::Downloading => {
                let (done, total) = self.progress;
                if let Some(percent) = (done * 100).checked_div(total) {
                    return (tr("Downloading {percent}%…").replace("{percent}", &percent.to_string()), "status_dim");
                }
                return (tr("Downloading…"), "status_dim");
            }
            Phase::Installing => return (tr("Installing…"), "status_dim"),
            Phase::RestartPending => return (tr("Updated. Telescope restarts when you stop streaming."), "status_ok"),
            Phase::Idle => {}
        }
        if self.checking {
            return (tr("Checking…"), "status_dim");
        }
        if !self.error.is_empty() {
            return (self.error.clone(), "status_err");
        }
        if let Some(m) = &self.available {
            let mut text = with_version("Telescope {version} is available.", m);
            if self.moves_on_update() {
                text = format!("{text} {}", tr("Updating also installs it for your user, with a Start menu entry, and removes this unzipped copy. Settings and phones come along."));
            }
            if let Some(blocker) = self.update_blocker() {
                return (format!("{text} {blocker}"), "status_warn");
            }
            if streaming {
                return (format!("{text} {}", tr("Stop streaming to update.")), "status_warn");
            }
            return (text, "status_ok");
        }
        if !self.checked {
            return (String::new(), "status_dim");
        }
        if let Some(m) = &self.rolled_back {
            return (with_version("Telescope {version} didn't start after updating, so it's held back. Try again?", m), "status_warn");
        }
        let nightly = self.channel == "nightly";
        if self.built_channel == "dev" {
            let mut text = tr("This is a source checkout, so it doesn't update itself.");
            if let Some(m) = &self.latest {
                let latest = if nightly { "The latest nightly build is {version}." } else { "The latest stable build is {version}." };
                text = format!("{text} {}", with_version(latest, m));
            }
            return (text, "status_dim");
        }
        if self.latest.is_none() {
            return (if nightly { tr("There's no nightly release yet.") } else { tr("There's no stable release yet.") }, "status_dim");
        }
        (tr("You have the latest version."), "status_dim")
    }

    fn refresh(&self, host: &mut dyn Host) {
        let tip = self.available.as_ref().map(|m| tr("Telescope {version} is available").replace("{version}", &m.display_version()));
        host.set_ui_props("header", &json!({"shown": self.available.is_some(), "tip": tip.unwrap_or_default()}));
        host.set_ui_props(SECTION, &self.section_props(host.is_streaming()));
    }

    fn section_props(&self, streaming: bool) -> Value {
        let (status, kind) = self.status_while(streaming);
        let available = self.available.is_some();
        let blocker = if available { self.update_blocker() } else { None };
        json!({
            "version-text": version::display_version(),
            "channels": CHANNEL_LABELS.iter().map(|(_, label)| tr(label)).collect::<Vec<_>>(),
            "channel": CHANNEL_LABELS.iter().position(|(key, _)| *key == self.channel).unwrap_or(0),
            "auto": self.auto_check,
            "portable-shown": self.backend.is_unzipped_copy(),
            "portable": self.keep_portable,
            "status-text": status,
            "status-kind": kind,
            "busy": self.busy(),
            "actions-shown": available,
            "retry-shown": self.rolled_back.is_some() && !available,
            "update-text": if blocker.is_some() { tr("Open download page") } else { tr("Update and restart") },
            "update-enabled": !self.busy() && (blocker.is_some() || !streaming),
        })
    }

    pub fn open_updates(&mut self, host: &mut dyn Host) {
        host.open_settings("about");
        if !self.checked && !self.checking {
            self.check(host, true);
        }
    }

    pub fn set_channel(&mut self, host: &mut dyn Host, channel: &str) {
        if !updates::CHANNELS.contains(&channel) || channel == self.channel {
            return;
        }
        self.channel = channel.into();
        self.available = None;
        self.latest = None;
        self.rolled_back = None;
        host.schedule_save();
        self.check(host, true);
    }

    pub fn try_again(&mut self, host: &mut dyn Host) {
        if self.busy() {
            return;
        }
        self.backend.forget_rolled_back();
        self.rolled_back = None;
        self.check(host, true);
    }

    /// Once each launch, then daily while Telescope stays open.
    pub fn maybe_auto_check(&mut self, host: &mut dyn Host) {
        if !self.auto_check || self.built_channel == "dev" || self.busy() {
            return;
        }
        if !self.checked || now() - self.last_check >= CHECK_INTERVAL_S {
            self.check(host, false);
        }
    }

    pub fn check(&mut self, host: &mut dyn Host, manual: bool) {
        if self.busy() {
            return;
        }
        self.check_id += 1;
        self.checking = true;
        self.manual = manual;
        self.error.clear();
        self.refresh(host);
        let (backend, id, channel) = (self.backend.clone(), self.check_id, self.channel.clone());
        self.run(host, move |push| push(Done::Checked { id, result: backend.fetch(&channel) }));
    }

    /// Runs work on a thread of its own (or right away for a backend that says so); what it pushes reaches drain on the host thread.
    fn run(&mut self, host: &mut dyn Host, work: impl FnOnce(&dyn Fn(Done)) + Send + 'static) {
        let (queue, poster) = (self.done.clone(), self.poster.clone());
        let push = move |d: Done| {
            queue.lock().unwrap_or_else(|e| e.into_inner()).push(d);
            if let Some(p) = &poster {
                p.post(DONE, Value::Null);
            }
        };
        if self.backend.threaded() {
            std::thread::spawn(move || work(&push));
        } else {
            let queue = self.done.clone();
            work(&move |d| queue.lock().unwrap_or_else(|e| e.into_inner()).push(d));
            self.drain(host);
        }
    }

    fn drain(&mut self, host: &mut dyn Host) {
        let done: Vec<Done> = std::mem::take(&mut *self.done.lock().unwrap_or_else(|e| e.into_inner()));
        for d in done {
            match d {
                Done::Checked { id, result } => self.on_checked(host, id, result),
                Done::Progress(done, total) => {
                    self.progress = (done, total);
                    self.refresh(host);
                }
                Done::Installing => {
                    self.phase = Phase::Installing;
                    self.refresh(host);
                }
                Done::Installed(result) => self.on_installed(host, result),
            }
        }
    }

    fn on_checked(&mut self, host: &mut dyn Host, id: u64, result: Result<Option<Manifest>, String>) {
        if id != self.check_id {
            return;
        }
        self.checking = false;
        match result {
            // A background check fails quietly (offline is normal); only a check you asked for says so
            Err(error) => self.error = if self.manual { error } else { String::new() },
            Ok(manifest) => {
                self.checked = true;
                self.last_check = now();
                host.schedule_save();
                let newer = self.backend.is_newer(manifest.as_ref()) && manifest.as_ref().is_some_and(|m| updates::platform_asset(m).is_some());
                self.available = if newer { manifest.clone() } else { None };
                self.rolled_back = if self.backend.was_rolled_back(manifest.as_ref()) { manifest.clone() } else { None };
                self.latest = manifest;
            }
        }
        self.refresh(host);
    }

    pub fn open_notes(&self) {
        let target = self.available.as_ref().or(self.latest.as_ref());
        self.backend.open_url(&target.map(|m| m.notes.clone()).filter(|n| !n.is_empty()).unwrap_or_else(releases_url));
    }

    pub fn update_now(&mut self, host: &mut dyn Host) {
        let Some(manifest) = self.available.clone() else { return };
        if self.busy() {
            return;
        }
        if self.update_blocker().is_some() {
            self.backend.open_url(&if manifest.notes.is_empty() { releases_url() } else { manifest.notes });
            return;
        }
        if host.is_streaming() {
            self.refresh(host);
            return;
        }
        let via_setup = self.moves_on_update();
        let chosen = if via_setup { updates::setup_asset(&manifest) } else { updates::platform_asset(&manifest) };
        let Some(asset) = chosen.cloned() else { return };
        self.phase = Phase::Downloading;
        self.progress = (0, asset.size);
        self.error.clear();
        self.cancel.store(false, Ordering::SeqCst);
        self.refresh(host);
        let (backend, cancel) = (self.backend.clone(), self.cancel.clone());
        self.run(host, move |push| {
            let mut progress = |p: Option<(u64, u64)>| push(p.map_or(Done::Installing, |(d, t)| Done::Progress(d, t)));
            let cancelled = || cancel.load(Ordering::SeqCst);
            let result = if via_setup { backend.install_via_setup(&manifest, &asset, &mut progress, &cancelled) } else { backend.install(&manifest, &asset, &mut progress, &cancelled) };
            push(Done::Installed(result));
        });
    }

    fn on_installed(&mut self, host: &mut dyn Host, result: Result<(i64, InstallResult), String>) {
        let result = match result {
            Ok((build, r)) => {
                self.install_build = build;
                r
            }
            Err(error) => {
                self.phase = Phase::Idle;
                self.error = error;
                self.refresh(host);
                return;
            }
        };
        if !result.skipped.is_empty() {
            log::warn!("Files in use kept their old version: {}", result.skipped.join(", "));
        }
        if host.is_streaming() {
            // Started (say, by an app opening the camera) while it downloaded: don't cut the stream off
            self.phase = Phase::RestartPending;
            self.relaunch_argv = Some(result.relaunch);
            self.refresh(host);
            return;
        }
        self.restart_into(host, &result.relaunch);
    }

    fn on_stream_stopped(&mut self, host: &mut dyn Host) {
        if self.phase == Phase::RestartPending && self.relaunch_argv.is_some() && !host.is_restarting() && !host.is_streaming() {
            self.phase = Phase::Installing;
            // After the stop that brought this has finished; quitting stops again, so only once
            host.schedule(0, "restart");
            return;
        }
        self.refresh(host);
    }

    fn restart_into(&mut self, host: &mut dyn Host, argv: &[String]) {
        if let Err(e) = self.backend.relaunch(argv) {
            // The new version is in place; staying open beats quitting with nothing to replace us
            log::error!("Couldn't restart after the update: {e}");
            self.phase = Phase::Idle;
            self.error = tr("Updated. Close and reopen Telescope to finish.");
            self.refresh(host);
            return;
        }
        if argv.iter().any(|a| a == setup_move::MOVED_FROM) {
            // The installed copy is starting now
            self.backend.started_elsewhere(self.install_build);
        }
        host.quit_app();
    }
}

impl Plugin for UpdatesPlugin {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: "updates".into(),
            title: n_("Updates").into(),
            about: n_("Checking for new versions of Telescope and installing them.").into(),
            category: "extras".into(),
            icon: "update".into(),
            required: true,
            header_side: "right".into(),
            ..Default::default()
        }
    }

    fn setup(&mut self, host: &mut dyn Host) {
        self.poster = host.poster();
        host.schedule(FIRST_CHECK_MS, "first");
        host.schedule(TICK_MS, "tick");
    }

    fn shutdown(&mut self, host: &mut dyn Host) {
        self.cancel.store(true, Ordering::SeqCst);
        host.cancel_schedule("first");
        host.cancel_schedule("tick");
        self.poster = None;
    }

    fn on_event(&mut self, host: &mut dyn Host, event: &Event) {
        match event {
            Event::TimerFired { token, .. } if token == "first" => self.maybe_auto_check(host),
            Event::TimerFired { token, .. } if token == "tick" => {
                host.schedule(TICK_MS, "tick");
                self.maybe_auto_check(host);
            }
            Event::TimerFired { token, .. } if token == "restart" => {
                if let Some(argv) = self.relaunch_argv.take() {
                    self.restart_into(host, &argv);
                }
            }
            Event::Posted { token, .. } if token == DONE => self.drain(host),
            Event::UpdateRequested => self.open_updates(host),
            Event::StreamStarted { .. } => self.refresh(host),
            Event::StreamStopped => self.on_stream_stopped(host),
            _ => {}
        }
    }

    fn header_widget(&mut self) -> Option<UiSpec> {
        Some(UiSpec { source: SlintSource::Text(UI.into()), component: "UpdateButton".into(), props: json!({"shown": false, "tip": ""}) })
    }

    fn settings_sections(&mut self) -> Vec<SettingsSection> {
        vec![SettingsSection {
            id: SECTION.into(),
            tab: "about".into(),
            ui: UiSpec { source: SlintSource::Text(UI.into()), component: "UpdatesSettings".into(), props: self.section_props(false) },
            order: 0,
            tab_title: String::new(),
            keywords: [
                tr_ctx("search", "update upgrade new version release download install latest"),
                tr_ctx("search", "current build number"),
                tr_ctx("search", "stable nightly beta release track builds testing"),
                tr_ctx("search", "automatic updates auto check every day background"),
            ].join(" "),
            footer: None,
        }]
    }

    fn on_ui(&mut self, host: &mut dyn Host, event: &UiEvent) {
        match (event.ui.as_str(), event.callback.as_str()) {
            ("header", "clicked") => self.open_updates(host),
            (SECTION, "check-now") => self.check(host, true),
            (SECTION, "channel-picked") => {
                let i = event.args.first().and_then(Value::as_u64).unwrap_or(0) as usize;
                if let Some((key, _)) = CHANNEL_LABELS.get(i) {
                    self.set_channel(host, key);
                }
                self.refresh(host);
            }
            (SECTION, "auto-toggled") => {
                self.auto_check = event.args.first().and_then(Value::as_bool).unwrap_or(true);
                host.schedule_save();
            }
            (SECTION, "portable-toggled") => {
                self.keep_portable = event.args.first().and_then(Value::as_bool).unwrap_or(false);
                host.schedule_save();
                self.refresh(host);
            }
            (SECTION, "update") => self.update_now(host),
            (SECTION, "notes") => self.open_notes(),
            (SECTION, "retry") => self.try_again(host),
            _ => {}
        }
    }

    fn get_config(&self) -> Value {
        json!({"channel": self.channel, "auto_check": self.auto_check, "last_check": self.last_check, "keep_portable": self.keep_portable})
    }

    fn set_config(&mut self, cfg: &Value) -> Result<(), ConfigRejected> {
        let channel = cfg.get("channel").and_then(Value::as_str).filter(|c| updates::CHANNELS.contains(c));
        self.channel = channel.unwrap_or(Self::default_channel_for(&self.built_channel)).into();
        self.auto_check = cfg.get("auto_check") != Some(&Value::Bool(false));
        self.keep_portable = cfg.get("keep_portable") == Some(&Value::Bool(true));
        self.last_check = cfg.get("last_check").and_then(Value::as_f64).unwrap_or(0.0);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use telescope_core::testing::FakeHost;

    fn asset(name: &str) -> Asset {
        Asset { name: name.into(), url: "https://x".into(), sha256: "a".repeat(64), size: 10 }
    }

    fn manifest(version: &str, build: i64, notes: &str) -> Manifest {
        let assets: BTreeMap<String, Asset> = [updates::LINUX_ASSET, updates::WINDOWS_ASSET].iter().map(|n| (n.to_string(), asset(n))).collect();
        Manifest { version: version.into(), build, channel: "nightly".into(), version_name: String::new(), notes: notes.into(), protocol: 2, assets }
    }

    fn newer() -> Manifest {
        manifest("0.6.0", 200, "https://notes")
    }

    #[derive(Default)]
    struct Fake {
        result: Mutex<Option<Result<Option<Manifest>, String>>>,
        channels: Mutex<Vec<String>>,
        installed: Mutex<Option<Result<(i64, InstallResult), String>>>,
        installs: Mutex<u32>,
        relaunched: Mutex<Vec<Vec<String>>>,
        relaunch_fails: AtomicBool,
        blocker: Mutex<Option<String>>,
        opened: Mutex<Vec<String>>,
        failed: Mutex<Option<i64>>,
        unzipped: AtomicBool,
        driver_here: AtomicBool,
        driver_asked: Mutex<u32>,
        /// (asset name, through the installer) for each install.
        asked_for: Mutex<Vec<(String, bool)>>,
        started_from: Mutex<Vec<i64>>,
    }

    impl Backend for Fake {
        fn fetch(&self, channel: &str) -> Result<Option<Manifest>, String> {
            self.channels.lock().unwrap().push(channel.into());
            self.result.lock().unwrap().clone().unwrap_or_else(|| Ok(Some(newer())))
        }
        fn install(&self, _m: &Manifest, a: &Asset, _p: &mut dyn FnMut(Option<(u64, u64)>), _c: &dyn Fn() -> bool) -> Result<(i64, InstallResult), String> {
            *self.installs.lock().unwrap() += 1;
            self.asked_for.lock().unwrap().push((a.name.clone(), false));
            self.installed.lock().unwrap().take().unwrap_or_else(|| Ok((200, InstallResult { relaunch: vec!["start.sh".into(), "--after-update".into()], skipped: vec![] })))
        }
        fn blocker(&self) -> Option<String> {
            self.blocker.lock().unwrap().clone()
        }
        fn is_newer(&self, m: Option<&Manifest>) -> bool {
            m.is_some_and(|m| m.build > 150 && *self.failed.lock().unwrap() != Some(m.build))
        }
        fn was_rolled_back(&self, m: Option<&Manifest>) -> bool {
            m.is_some_and(|m| m.build > 150 && *self.failed.lock().unwrap() == Some(m.build))
        }
        fn forget_rolled_back(&self) {
            *self.failed.lock().unwrap() = None;
        }
        fn relaunch(&self, argv: &[String]) -> io::Result<()> {
            if self.relaunch_fails.load(Ordering::SeqCst) {
                return Err(io::Error::new(io::ErrorKind::PermissionDenied, "blocked"));
            }
            self.relaunched.lock().unwrap().push(argv.to_vec());
            Ok(())
        }
        fn open_url(&self, url: &str) {
            self.opened.lock().unwrap().push(url.into());
        }
        fn threaded(&self) -> bool {
            false
        }
        fn is_unzipped_copy(&self) -> bool {
            self.unzipped.load(Ordering::SeqCst)
        }
        fn driver_in_app_folder(&self) -> bool {
            *self.driver_asked.lock().unwrap() += 1;
            self.driver_here.load(Ordering::SeqCst)
        }
        fn install_via_setup(&self, m: &Manifest, a: &Asset, p: &mut dyn FnMut(Option<(u64, u64)>), c: &dyn Fn() -> bool) -> Result<(i64, InstallResult), String> {
            let result = self.install(m, a, p, c);
            self.asked_for.lock().unwrap().last_mut().unwrap().1 = true;
            result
        }
        fn started_elsewhere(&self, build: i64) {
            self.started_from.lock().unwrap().push(build);
        }
    }

    fn env() -> (UpdatesPlugin, FakeHost, Arc<Fake>) {
        let fake = Arc::new(Fake::default());
        let mut plugin = UpdatesPlugin::new(fake.clone(), "nightly");
        let mut host = FakeHost::default();
        plugin.setup(&mut host);
        (plugin, host, fake)
    }

    fn button_shown(host: &FakeHost) -> bool {
        last_props(host, "header")["shown"] == true
    }

    fn last_props(host: &FakeHost, ui: &str) -> Value {
        let prefix = format!("set_ui_props {ui} ");
        host.calls.iter().rev().find_map(|c| c.strip_prefix(&prefix)).map(|j| serde_json::from_str(j).unwrap()).unwrap_or(Value::Null)
    }

    fn quits(host: &FakeHost) -> usize {
        host.calls.iter().filter(|c| *c == "quit_app").count()
    }

    #[test]
    fn a_newer_build_shows_the_update_button() {
        let (mut plugin, mut host, fake) = env();
        assert!(host.is_scheduled("first") && host.is_scheduled("tick"));
        plugin.on_event(&mut host, &Event::TimerFired { owner: "updates".into(), token: "first".into() });
        assert_eq!(*fake.channels.lock().unwrap(), vec!["nightly"]);
        assert!(button_shown(&host));
        assert!(last_props(&host, "header")["tip"].as_str().unwrap().contains("0.6.0 nightly 200"));
    }

    #[test]
    fn the_daily_check_waits_a_day_and_skips_source_checkouts() {
        let (mut plugin, mut host, fake) = env();
        plugin.maybe_auto_check(&mut host);
        plugin.maybe_auto_check(&mut host);
        assert_eq!(fake.channels.lock().unwrap().len(), 1);
        plugin.last_check = 0.0;
        plugin.built_channel = "dev".into();
        plugin.maybe_auto_check(&mut host);
        assert_eq!(fake.channels.lock().unwrap().len(), 1);
    }

    #[test]
    fn each_launch_checks_even_if_the_last_check_was_recent() {
        let (mut plugin, mut host, fake) = env();
        plugin.set_config(&json!({"last_check": now() - 60.0})).unwrap();
        plugin.maybe_auto_check(&mut host);
        assert_eq!(*fake.channels.lock().unwrap(), vec!["nightly"]);
        plugin.maybe_auto_check(&mut host);
        assert_eq!(fake.channels.lock().unwrap().len(), 1);
    }

    #[test]
    fn background_failures_stay_quiet_but_a_manual_check_explains() {
        let (mut plugin, mut host, fake) = env();
        *fake.result.lock().unwrap() = Some(Err("Couldn't check for updates. Check the internet connection.".into()));
        plugin.check(&mut host, false);
        assert_eq!(plugin.status_text(&host), (String::new(), "status_dim"));
        assert!(!button_shown(&host));
        plugin.check(&mut host, true);
        assert_eq!(plugin.status_text(&host).1, "status_err");
    }

    #[test]
    fn a_launch_without_internet_checks_again_on_the_next_tick() {
        let (mut plugin, mut host, fake) = env();
        *fake.result.lock().unwrap() = Some(Err("Couldn't check for updates. Check the internet connection.".into()));
        plugin.maybe_auto_check(&mut host);
        assert_eq!(plugin.last_check, 0.0);
        *fake.result.lock().unwrap() = None;
        plugin.on_event(&mut host, &Event::TimerFired { owner: "updates".into(), token: "tick".into() });
        assert_eq!(fake.channels.lock().unwrap().len(), 2);
        assert!(button_shown(&host));
        assert!(host.is_scheduled("tick"));
    }

    #[test]
    fn up_to_date_and_no_release_yet() {
        let (mut plugin, mut host, fake) = env();
        *fake.result.lock().unwrap() = Some(Ok(Some(manifest("0.5.0", 150, ""))));
        plugin.check(&mut host, true);
        assert_eq!(plugin.status_text(&host).0, "You have the latest version.");
        assert!(!button_shown(&host));
        *fake.result.lock().unwrap() = Some(Ok(None));
        plugin.set_channel(&mut host, "stable");
        assert_eq!(fake.channels.lock().unwrap().last().unwrap(), "stable");
        assert_eq!(plugin.status_text(&host).0, "There's no stable release yet.");
    }

    #[test]
    fn updating_downloads_installs_relaunches_and_quits() {
        let (mut plugin, mut host, fake) = env();
        plugin.check(&mut host, false);
        plugin.update_now(&mut host);
        assert_eq!(*fake.relaunched.lock().unwrap(), vec![vec!["start.sh".to_string(), "--after-update".into()]]);
        assert_eq!(quits(&host), 1);
    }

    // An unzipped Windows copy updates through the installer (plugins/updates.py's moves_on_update)

    fn with_setup() -> Manifest {
        let mut m = newer();
        m.assets.insert(updates::WINDOWS_SETUP_ASSET.into(), asset(updates::WINDOWS_SETUP_ASSET));
        m
    }

    /// An unzipped copy whose camera driver is in Program Files, with a release that has the setup.
    fn unzipped_env() -> (UpdatesPlugin, FakeHost, Arc<Fake>) {
        let (plugin, host, fake) = env();
        fake.unzipped.store(true, Ordering::SeqCst);
        *fake.result.lock().unwrap() = Some(Ok(Some(with_setup())));
        (plugin, host, fake)
    }

    #[test]
    fn an_unzipped_copy_updates_through_the_installer_and_says_so() {
        let (mut plugin, mut host, fake) = unzipped_env();
        plugin.check(&mut host, false);
        assert!(plugin.moves_on_update());
        assert!(plugin.status_text(&host).0.contains("installs it for your user"));
        // The installer needs no writable folder, so a move has no blocker
        *fake.blocker.lock().unwrap() = Some("Telescope's folder isn't writable.".into());
        assert_eq!(plugin.update_blocker(), None);
        plugin.update_now(&mut host);
        assert_eq!(*fake.asked_for.lock().unwrap(), vec![(updates::WINDOWS_SETUP_ASSET.to_string(), true)]);
    }

    #[test]
    fn keeping_it_portable_updates_in_place_with_the_zip() {
        let (mut plugin, mut host, fake) = unzipped_env();
        plugin.check(&mut host, false);
        plugin.on_ui(&mut host, &UiEvent { ui: SECTION.into(), callback: "portable-toggled".into(), args: vec![json!(true)] });
        assert!(plugin.keep_portable && !plugin.moves_on_update());
        assert!(!plugin.status_text(&host).0.contains("installs it for your user"));
        plugin.update_now(&mut host);
        assert_eq!(*fake.asked_for.lock().unwrap(), vec![(updates::platform_asset(&newer()).unwrap().name.clone(), false)]);
    }

    #[test]
    fn no_move_without_a_setup_or_with_the_driver_in_this_folder() {
        let (mut plugin, mut host, fake) = unzipped_env();
        *fake.result.lock().unwrap() = Some(Ok(Some(newer()))); // from before the installer
        plugin.check(&mut host, false);
        assert!(!plugin.moves_on_update());
        *fake.result.lock().unwrap() = Some(Ok(Some(with_setup())));
        plugin.check(&mut host, false);
        fake.driver_here.store(true, Ordering::SeqCst); // deleting this folder would take the camera
        plugin.driver_here = Mutex::new(None);
        assert!(!plugin.moves_on_update());
        plugin.update_now(&mut host);
        assert!(!fake.asked_for.lock().unwrap()[0].1);
    }

    #[test]
    fn an_installed_copy_never_moves_and_hides_the_portable_switch() {
        let (mut plugin, mut host, fake) = unzipped_env();
        fake.unzipped.store(false, Ordering::SeqCst);
        plugin.check(&mut host, false);
        assert!(!plugin.moves_on_update());
        assert_eq!(last_props(&host, SECTION)["portable-shown"], false);
        fake.unzipped.store(true, Ordering::SeqCst);
        plugin.refresh(&mut host);
        assert_eq!(last_props(&host, SECTION)["portable-shown"], true);
    }

    #[test]
    fn the_camera_driver_check_is_remembered_for_half_a_minute() {
        let (mut plugin, mut host, fake) = unzipped_env();
        plugin.check(&mut host, false);
        let before = *fake.driver_asked.lock().unwrap();
        assert!(plugin.moves_on_update() && plugin.moves_on_update());
        assert_eq!(*fake.driver_asked.lock().unwrap(), before.max(1), "asked once for both");
        let asked = *fake.driver_asked.lock().unwrap();
        *plugin.driver_here.lock().unwrap() = Some((std::time::Instant::now() - Duration::from_secs(31), false));
        plugin.moves_on_update();
        assert_eq!(*fake.driver_asked.lock().unwrap(), asked + 1);
    }

    #[test]
    fn the_installed_copy_starting_marks_the_old_folder_with_the_build() {
        let (mut plugin, mut host, fake) = unzipped_env();
        plugin.check(&mut host, false);
        *fake.installed.lock().unwrap() = Some(Ok((200, InstallResult {
            relaunch: vec!["new/TelescopeDesktop.exe".into(), "--after-update".into(), setup_move::MOVED_FROM.into(), "old".into()],
            skipped: vec![],
        })));
        plugin.update_now(&mut host);
        assert_eq!(*fake.started_from.lock().unwrap(), vec![200]);
        assert_eq!(quits(&host), 1);
    }

    #[test]
    fn an_in_place_update_marks_nothing() {
        let (mut plugin, mut host, fake) = env();
        plugin.check(&mut host, false);
        plugin.update_now(&mut host);
        assert!(fake.started_from.lock().unwrap().is_empty());
    }

    #[test]
    fn an_update_that_cant_restart_stays_open_and_says_so() {
        let (mut plugin, mut host, fake) = env();
        plugin.check(&mut host, false);
        fake.relaunch_fails.store(true, Ordering::SeqCst);
        plugin.update_now(&mut host);
        assert_eq!(quits(&host), 0);
        assert!(!plugin.busy());
        assert!(plugin.status_text(&host).0.contains("reopen Telescope"));
    }

    #[test]
    fn an_update_that_lands_during_a_stream_restarts_only_once_it_stops() {
        let (mut plugin, mut host, fake) = env();
        plugin.check(&mut host, false);
        plugin.phase = Phase::Downloading;
        host.streaming = true;
        plugin.on_installed(&mut host, Ok((200, InstallResult { relaunch: vec!["start.sh".into()], skipped: vec![] })));
        assert!(fake.relaunched.lock().unwrap().is_empty() && quits(&host) == 0);
        assert!(plugin.status_text(&host).0.contains("when you stop streaming"));
        host.streaming = false;
        plugin.on_event(&mut host, &Event::StreamStopped);
        plugin.on_event(&mut host, &Event::StreamStopped);
        assert!(host.is_scheduled("restart"));
        plugin.on_event(&mut host, &Event::TimerFired { owner: "updates".into(), token: "restart".into() });
        plugin.on_event(&mut host, &Event::TimerFired { owner: "updates".into(), token: "restart".into() });
        assert_eq!(*fake.relaunched.lock().unwrap(), vec![vec!["start.sh".to_string()]]);
        assert_eq!(quits(&host), 1);
    }

    #[test]
    fn no_update_while_streaming() {
        let (mut plugin, mut host, fake) = env();
        plugin.check(&mut host, false);
        host.streaming = true;
        plugin.on_event(&mut host, &Event::StreamStarted { url: "x".into() });
        let props = last_props(&host, SECTION);
        assert_eq!(props["update-enabled"], false);
        assert!(props["status-text"].as_str().unwrap().contains("Stop streaming to update"));
        plugin.update_now(&mut host);
        assert_eq!(*fake.installs.lock().unwrap(), 0);
        host.streaming = false;
        plugin.on_event(&mut host, &Event::StreamStopped);
        assert_eq!(last_props(&host, SECTION)["update-enabled"], true);
    }

    #[test]
    fn a_failed_update_says_why_and_keeps_running() {
        let (mut plugin, mut host, fake) = env();
        plugin.check(&mut host, false);
        *fake.installed.lock().unwrap() = Some(Err("The download didn't match its checksum.".into()));
        plugin.update_now(&mut host);
        assert_eq!(plugin.status_text(&host), ("The download didn't match its checksum.".to_string(), "status_err"));
        assert_eq!(quits(&host), 0);
        assert!(!plugin.busy());
    }

    #[test]
    fn a_copy_that_cant_replace_itself_links_to_the_release() {
        let (mut plugin, mut host, fake) = env();
        plugin.check(&mut host, false);
        *fake.blocker.lock().unwrap() = Some("This is a source checkout. Update it with git.".into());
        plugin.on_event(&mut host, &Event::StreamStopped);
        assert_eq!(last_props(&host, SECTION)["update-text"], "Open download page");
        plugin.on_ui(&mut host, &UiEvent { ui: SECTION.into(), callback: "update".into(), args: vec![] });
        assert_eq!(*fake.opened.lock().unwrap(), vec!["https://notes"]);
        assert_eq!(*fake.installs.lock().unwrap(), 0);
    }

    #[test]
    fn config_round_trips_and_defaults_to_the_build_s_channel() {
        let (mut plugin, _host, _fake) = env();
        plugin.set_config(&json!({"channel": "stable", "auto_check": false, "last_check": 5.0})).unwrap();
        assert_eq!(plugin.get_config(), json!({"channel": "stable", "auto_check": false, "last_check": 5.0, "keep_portable": false}));
        plugin.set_config(&json!({"channel": "bogus", "last_check": "x", "keep_portable": "yes"})).unwrap();
        assert_eq!(plugin.get_config(), json!({"channel": "nightly", "auto_check": true, "last_check": 0.0, "keep_portable": false}));
        plugin.set_config(&json!({"keep_portable": true})).unwrap();
        assert_eq!(plugin.get_config()["keep_portable"], true);
    }

    #[test]
    fn a_newer_phone_app_opens_settings_on_updates() {
        let (mut plugin, mut host, _fake) = env();
        plugin.on_event(&mut host, &Event::UpdateRequested);
        assert!(host.calls.contains(&"open_settings about".to_string()));
    }

    #[test]
    fn the_dialog_says_a_build_was_rolled_back_and_lets_the_user_try_again() {
        let (mut plugin, mut host, fake) = env();
        *fake.failed.lock().unwrap() = Some(200);
        plugin.check(&mut host, true);
        let (text, kind) = plugin.status_text(&host);
        assert!(text.contains("0.6.0 nightly 200 didn't start after updating, so it's held back"), "{text}");
        assert_eq!(kind, "status_warn");
        assert!(plugin.available.is_none() && !button_shown(&host));
        assert_eq!(last_props(&host, SECTION)["retry-shown"], true);
        plugin.on_ui(&mut host, &UiEvent { ui: SECTION.into(), callback: "retry".into(), args: vec![] });
        assert!(plugin.rolled_back.is_none());
        assert_eq!(plugin.available, Some(newer()));
        assert!(button_shown(&host));
    }

    #[test]
    fn a_thread_hands_its_result_back_through_the_poster() {
        struct Threaded(Fake);
        impl Backend for Threaded {
            fn fetch(&self, c: &str) -> Result<Option<Manifest>, String> { self.0.fetch(c) }
            fn install(&self, m: &Manifest, a: &Asset, p: &mut dyn FnMut(Option<(u64, u64)>), c: &dyn Fn() -> bool) -> Result<(i64, InstallResult), String> { self.0.install(m, a, p, c) }
            fn blocker(&self) -> Option<String> { None }
            fn is_newer(&self, m: Option<&Manifest>) -> bool { self.0.is_newer(m) }
            fn was_rolled_back(&self, m: Option<&Manifest>) -> bool { self.0.was_rolled_back(m) }
            fn forget_rolled_back(&self) {}
            fn relaunch(&self, a: &[String]) -> io::Result<()> { self.0.relaunch(a) }
            fn open_url(&self, _u: &str) {}
        }
        let mut plugin = UpdatesPlugin::new(Arc::new(Threaded(Fake::default())), "nightly");
        let mut host = FakeHost::default();
        plugin.setup(&mut host);
        plugin.check(&mut host, true);
        assert!(plugin.busy());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while host.posted.lock().unwrap().is_empty() && std::time::Instant::now() < deadline {
            std::thread::yield_now();
        }
        let posted: Vec<(String, Value)> = std::mem::take(&mut *host.posted.lock().unwrap());
        for (token, data) in posted {
            plugin.on_event(&mut host, &Event::Posted { owner: "updates".into(), token, data });
        }
        assert!(!plugin.busy());
        assert!(plugin.available.is_some());
    }
}
