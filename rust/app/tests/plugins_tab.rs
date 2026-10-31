//! The Python tests of widgets/plugins_panel.py's model (tests/test_plugins_panel.py), named after them. They run here, in the binary's
//! crate, because the real plugins folder comes from the plugin host, which the shell never names.

use i_slint_backend_testing::ElementHandle;
use serde_json::{json, Value};
use slint::platform::WindowEvent;
use slint::{ComponentHandle, Model};
use slint_interpreter::{ComponentInstance, Value as SValue};
use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use telescope_core::config::ConfigFile;
use telescope_core::plugin::{Plugin, PluginInfo};
use telescope_core::plugin_admin::{AddedPlugin, InstallFailure, NewerRelease, PluginAdmin, AUTO_UPDATE, CHECK_UPDATES};
use telescope_core::testing::scratch_dir;
use telescope_core::vision::{VisionPack, VisionService};
use telescope_plugin_host::admin::Admin;
use telescope_plugin_host::package::PluginFolder;
use telescope_plugin_host::switches::Switches;
use telescope_plugin_host::updater::PluginUpdater;
use telescope_plugin_host::updates::Https;
use telescope_shell::{settings, window};
use telescope_shell::plugins_tab::{
    columns, split_link, Download, Filter, Link, NoticeKind, Outcome, Panel, Question, Request, Tile,
};

fn info(name: &str, title: &str) -> PluginInfo {
    PluginInfo { name: name.into(), title: title.into(), about: format!("What {title} does."), when_off: format!("The {title} card goes away."), ..Default::default() }
}

fn builtins() -> Vec<PluginInfo> {
    let mut connection = info("connection", "Connection");
    connection.required = true;
    vec![connection, info("camera_control", "Camera"), info("presets", "Presets")]
}

fn optional(name: &str, title: &str) -> PluginInfo {
    PluginInfo { optional: true, needs_vision: true, category: "vision".into(), ..info(name, title) }
}

fn names(list: &[PluginInfo]) -> BTreeSet<String> {
    list.iter().map(|p| p.name.clone()).collect()
}

/// The plugin host's admin, except that the last update check found `release` for every plugin and checking asks nobody.
struct Tested {
    inner: Admin,
    release: Option<NewerRelease>,
    checks: Arc<AtomicU32>,
}

impl PluginAdmin for Tested {
    fn builtin_off(&self) -> BTreeSet<String> { self.inner.builtin_off() }
    fn set_builtin_off(&self, name: &str, off: bool) -> io::Result<()> { self.inner.set_builtin_off(name, off) }
    fn all_builtin_on(&self) -> io::Result<()> { self.inner.all_builtin_on() }
    fn optional_on(&self) -> BTreeSet<String> { self.inner.optional_on() }
    fn set_optional_on(&self, name: &str, on: bool) -> io::Result<()> { self.inner.set_optional_on(name, on) }
    fn switch(&self, key: &str, default: bool) -> bool { self.inner.switch(key, default) }
    fn set_switch(&self, key: &str, on: bool) -> io::Result<()> { self.inner.set_switch(key, on) }
    fn plugins_dir(&self) -> PathBuf { self.inner.plugins_dir() }
    fn find_installed(&self) -> Vec<AddedPlugin> { self.inner.find_installed() }
    fn set_enabled(&self, name: &str, on: bool) -> io::Result<()> { self.inner.set_enabled(name, on) }
    fn install_zip(&self, archive: &Path, replace: bool) -> Result<AddedPlugin, InstallFailure> { self.inner.install_zip(archive, replace) }
    fn remove(&self, name: &str) -> Result<(), String> { self.inner.remove(name) }
    fn newer_release(&self, _name: &str) -> Option<NewerRelease> { self.release.clone().filter(|_| self.inner.switch(CHECK_UPDATES, true)) }
    fn maybe_check_updates(&self) { self.checks.fetch_add(1, Ordering::SeqCst); }
}

/// The download double of the Python tests: start() finishes at once.
#[derive(Clone)]
struct FakeDownload {
    ready: Rc<Cell<bool>>,
    available: bool,
    error: String,
    started: Rc<RefCell<Vec<String>>>,
    removed: Rc<Cell<u32>>,
    admin: Arc<dyn PluginAdmin>,
}

impl Download for FakeDownload {
    fn ready(&self) -> bool { self.ready.get() }
    fn available(&self) -> bool { self.available }
    fn running(&self) -> bool { false }
    fn size_mb(&self) -> u64 { 47 }
    fn waiting(&self) -> String { String::new() }
    fn error(&self) -> String { self.error.clone() }
    fn failed_for(&self) -> String { if self.error.is_empty() { String::new() } else { self.started.borrow().last().cloned().unwrap_or_default() } }
    fn progress(&self) -> (u64, u64) { (0, 0) }
    fn start(&self, switch_on: &str) {
        self.started.borrow_mut().push(switch_on.into());
        if self.error.is_empty() {
            self.ready.set(true);
            self.admin.set_optional_on(switch_on, true).unwrap();
        }
    }
    fn remove(&self) {
        self.removed.set(self.removed.get() + 1);
        self.ready.set(false);
    }
}

/// A download that's there already, for the tests that aren't about it.
struct NoDownload;

impl Download for NoDownload {
    fn ready(&self) -> bool { true }
    fn available(&self) -> bool { true }
    fn running(&self) -> bool { false }
    fn size_mb(&self) -> u64 { 1 }
    fn waiting(&self) -> String { String::new() }
    fn error(&self) -> String { String::new() }
    fn failed_for(&self) -> String { String::new() }
    fn progress(&self) -> (u64, u64) { (0, 0) }
    fn start(&self, _: &str) {}
    fn remove(&self) {}
}

struct Rig {
    panel: Panel,
    admin: Arc<Tested>,
    download: FakeDownload,
    checks: Arc<AtomicU32>,
    plugins: PathBuf,
    root: PathBuf,
}

const RESERVED: [&str; 9] = ["connection", "camera_control", "presets", "framing", "background", "zoom_tool", "alpha", "echo", "lost"];

/// A plugins folder next to a settings file, as the app has them, and its admin.
fn admin_in(root: &Path, dev: Vec<PathBuf>, release: Option<NewerRelease>, checks: Arc<AtomicU32>) -> (PathBuf, Arc<Tested>) {
    let plugins = root.join("plugins");
    fs::create_dir_all(&plugins).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(root, fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(&plugins, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let folder = PluginFolder::new(plugins.clone(), Switches::new(ConfigFile::at(root.join("telescope_config.json"))), RESERVED.map(String::from)).with_dev_folders(dev);
    let updater = PluginUpdater::new(root.join("plugin-updates.json"), Arc::new(Https));
    (plugins, Arc::new(Tested { inner: Admin::new(folder, updater), release, checks }))
}

/// Writes an added plugin: a plugin.json with extra keys, and a main.lua.
fn added(plugins: &Path, name: &str, extra: Value) -> PathBuf {
    let target = plugins.join(name);
    fs::create_dir(&target).unwrap();
    let mut manifest = json!({"name": name, "title": name[..1].to_uppercase() + &name[1..], "api": 2});
    for (k, v) in extra.as_object().into_iter().flatten() {
        manifest[k] = v.clone();
    }
    fs::write(target.join("plugin.json"), manifest.to_string()).unwrap();
    fs::write(target.join("main.lua"), "return {}\n").unwrap();
    target
}

fn rig_with(list: Vec<PluginInfo>, running: &[&str], problems: &[(&str, &str)], download: impl FnOnce(&mut FakeDownload),
            release: Option<NewerRelease>, setup: impl FnOnce(&Path, &dyn PluginAdmin)) -> Rig {
    let root = scratch_dir("plugins-panel");
    let checks = Arc::new(AtomicU32::new(0));
    let (plugins, admin) = admin_in(&root, Vec::new(), release, checks.clone());
    setup(&plugins, admin.as_ref());
    let mut dl = FakeDownload { ready: Rc::new(Cell::new(false)), available: true, error: String::new(), started: Rc::default(), removed: Rc::default(), admin: admin.clone() };
    download(&mut dl);
    let running = running.iter().map(|s| s.to_string()).collect();
    let problems = problems.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    let panel = Panel::new(list, running, problems, admin.clone(), Box::new(dl.clone()));
    Rig { panel, admin, download: dl, checks, plugins, root }
}

fn rig(list: Vec<PluginInfo>, running: &[&str]) -> Rig {
    rig_with(list, running, &[], |_| {}, None, |_, _| {})
}

fn all_running() -> Vec<&'static str> {
    vec!["connection", "camera_control", "presets"]
}

fn yes(_: &Question) -> bool { true }
fn no(_: &Question) -> bool { false }

fn tile<'a>(panel: &'a Panel, title: &str) -> &'a Tile {
    panel.groups.iter().flat_map(|g| g.tiles.iter()).find(|t| t.title == title).unwrap_or_else(|| panic!("no tile {title}"))
}

fn flip(rig: &mut Rig, name: &str, on: bool, reply: &mut dyn FnMut(&Question) -> bool) -> Outcome {
    rig.panel.run(Request::Switch { name: name.into(), on }, reply)
}

fn labels(panel: &Panel) -> Vec<String> {
    panel.groups.iter().flat_map(|g| g.tiles.iter()).flat_map(|t| {
        let mut all = vec![t.title.clone(), t.about.clone(), t.note.clone(), t.byline.clone()];
        all.extend(t.badges.iter().map(|b| b.text.clone()));
        all
    }).filter(|s| !s.is_empty()).collect()
}

fn zip_of(folder: &Path, name: &str, path: &Path) {
    use std::io::Write;
    let mut zip = zip::ZipWriter::new(fs::File::create(path).unwrap());
    let options = zip::write::SimpleFileOptions::default();
    for entry in fs::read_dir(folder.join(name)).unwrap().flatten() {
        zip.start_file(format!("{name}/{}", entry.file_name().to_string_lossy()), options).unwrap();
        zip.write_all(&fs::read(entry.path()).unwrap()).unwrap();
    }
    zip.finish().unwrap();
}

fn off(r: &Rig) -> Vec<String> {
    r.admin.builtin_off().into_iter().collect()
}

#[test]
fn switching_off_asks_first_names_what_goes_and_waits_for_a_restart() {
    let mut r = rig(builtins(), &all_running());
    let connection = tile(&r.panel, "Connection");
    assert!(connection.on && !connection.enabled && connection.locked);
    assert!(r.panel.pending_changes() == 0);
    let mut asked = vec![];
    flip(&mut r, "presets", false, &mut |q| { asked.push(q.clone()); false });
    assert!(r.admin.builtin_off().is_empty() && tile(&r.panel, "Presets").on);
    flip(&mut r, "presets", false, &mut |q| { asked.push(q.clone()); true });
    assert_eq!(off(&r), ["presets"]);
    let last = asked.last().unwrap();
    assert!(last.title == "Switch off Presets?" && last.text.contains("The Presets card goes away.") && last.text.contains("settings stay saved"));
    assert_eq!(r.panel.pending_changes(), 1);
    assert_eq!(r.panel.footer_note(), "Restart to apply 1 change");
    assert!(labels(&r.panel).contains(&"Waits for a restart".to_string()));
    flip(&mut r, "presets", true, &mut yes);
    assert!(r.admin.builtin_off().is_empty() && r.panel.pending_changes() == 0);
}

#[test]
fn switching_on_an_added_plugin_warns_about_trust() {
    let mut r = rig(builtins(), &all_running());
    added(&r.plugins, "extra", json!({}));
    r.panel.refresh();
    let mut asked = vec![];
    flip(&mut r, "extra", true, &mut |q| { asked.push(q.clone()); true });
    let q = asked.last().unwrap();
    assert!(q.title == "Switch on Extra?" && q.text.contains("trust") && q.danger);
    assert!(r.admin.find_installed()[0].enabled && r.panel.pending_changes() == 1);
}

#[test]
fn turn_all_back_on() {
    let mut r = rig(builtins(), &["connection", "camera_control"]);
    r.admin.set_builtin_off("presets", true).unwrap();
    r.panel.refresh();
    assert!(r.panel.all_on_enabled);
    r.panel.run(Request::AllOn, &mut yes);
    assert!(r.admin.builtin_off().is_empty() && !r.panel.all_on_enabled);
}

#[test]
fn restart_now_restarts() {
    let mut r = rig(builtins(), &[]);
    assert_eq!(r.panel.pending_changes(), 2);
    assert_eq!(r.panel.footer_note(), "Restart to apply 2 changes");
    let outcome = r.panel.run(Request::Restart { streaming: false }, &mut yes);
    assert!(outcome.restart && outcome.notices.is_empty());
    // Streaming: asks first, and No leaves it running
    assert!(!r.panel.run(Request::Restart { streaming: true }, &mut no).restart);
    let mut asked = vec![];
    assert!(r.panel.run(Request::Restart { streaming: true }, &mut |q| { asked.push(q.clone()); true }).restart);
    assert_eq!(asked[0].title, "Restart Telescope now?");
    assert_eq!(asked[0].text, "Streaming stops while Telescope restarts. It takes a few seconds.");
}

struct Named(PluginInfo);

impl Plugin for Named {
    fn info(&self) -> PluginInfo {
        self.0.clone()
    }
}

/// The vision download as the Plugins tab sees it: start() finishes at once, as the real one does once its thread is done.
struct Pack {
    ready: Mutex<bool>,
    admin: Arc<dyn PluginAdmin>,
}

impl VisionService for Pack {
    fn attach(&self, _bus: telescope_core::BusSender) {}
    fn state(&self) -> String { String::new() }
    fn load(&self) {}
    fn person_mask(&self, _source_id: &str, _frame: &telescope_core::Frame) -> Option<Arc<Vec<u8>>> { None }
    fn want_people(&self, _owner: &str, _on: bool) {}
    fn wants_people(&self) -> bool { false }
    fn feed_raw(&self, _source_id: &str, _frame: &telescope_core::Frame, _upright: telescope_core::vision::Upright) {}
    fn tracked_people(&self, _source_id: &str) -> (Vec<Value>, u32) { (Vec::new(), 0) }
    fn view_turned(&self, _old: telescope_core::vision::Turn, _new: telescope_core::vision::Turn) {}
    fn drop_stream(&self, _source_id: &str) {}
    fn set_power_saving(&self, _on: bool) {}
    fn diagnostics(&self) -> Vec<(String, String)> { Vec::new() }
    fn shutdown(&self) {}
    fn pack(&self) -> VisionPack {
        VisionPack { ready: *self.ready.lock().unwrap(), available: true, size_mb: 47, ..VisionPack::default() }
    }
    fn download(&self, switch_on: &str) {
        *self.ready.lock().unwrap() = true;
        self.admin.set_optional_on(switch_on, true).unwrap();
    }
    fn cancel_download(&self) {}
    fn remove_pack(&self) {}
}

/// What Telescope started again with.
static RELAUNCHED: Mutex<Vec<Vec<String>>> = Mutex::new(Vec::new());

/// The real window and its Settings, on the Plugins tab, with these plugins and a plugins folder `setup` fills.
fn settings_with(label: &str, list: Vec<PluginInfo>, setup: impl FnOnce(&Path, &dyn PluginAdmin)) -> (Rc<window::Window>, Rc<settings::Settings>, Arc<Tested>) {
    i_slint_backend_testing::init_no_event_loop();
    window::use_app_hooks(window::AppHooks {
        build: String::new,
        commit: || "",
        release_memory: || {},
        relaunch: |argv| {
            RELAUNCHED.lock().unwrap().push(argv.to_vec());
            Ok(())
        },
    });
    let root = scratch_dir(label);
    let (plugins, admin) = admin_in(&root, Vec::new(), None, Arc::default());
    setup(&plugins, admin.as_ref());
    let w = window::build(ConfigFile::at(root.join("telescope_config.json")), move |_| {
        list.into_iter().map(|i| Box::new(Named(i)) as Box<dyn Plugin>).collect()
    }, None, None).unwrap();
    w.state.borrow_mut().admin = admin.clone();
    w.state.borrow_mut().vision = Some(Arc::new(Pack { ready: Mutex::new(false), admin: admin.clone() }));
    settings::open(&w, "plugins");
    let s = settings::current().expect("the settings window");
    // Only what's inside the page's clip can be found
    s.ui.window().set_size(slint::LogicalSize::new(700.0, 6000.0));
    (w, s, admin)
}

fn labelled(ui: &ComponentInstance, label: &str) -> Vec<ElementHandle> {
    ElementHandle::find_by_accessible_label(ui, label).collect()
}

fn switch_named(ui: &ComponentInstance, name: &str) -> ElementHandle {
    labelled(ui, name).into_iter().find(|e| e.accessible_checkable() == Some(true)).unwrap_or_else(|| panic!("no switch {name}"))
}

fn click(ui: &ComponentInstance, e: &ElementHandle) {
    let (p, s) = (e.absolute_position(), e.size());
    let position = slint::LogicalPosition::new(p.x + s.width / 2.0, p.y + s.height / 2.0);
    let button = slint::platform::PointerEventButton::Left;
    ui.window().dispatch_event(WindowEvent::PointerMoved { position });
    ui.window().dispatch_event(WindowEvent::PointerPressed { position, button });
    ui.window().dispatch_event(WindowEvent::PointerReleased { position, button });
}

fn scroll_of(s: &settings::Settings) -> String {
    format!("page{}-offset", s.tabs().iter().position(|t| t == "plugins").expect("the Plugins tab"))
}

// The press has to reach the window's own restart, not only the tab
#[test]
fn the_restart_button_restarts_telescope() {
    let (_w, s, _) = settings_with("plugins-restart", builtins(), |_, _| {});
    switch_named(&s.ui, "Presets").invoke_accessible_default_action();
    s.ui.invoke("answered", &[SValue::Bool(true)]).unwrap();
    let buttons: Vec<ElementHandle> = labelled(&s.ui, "Restart now").into_iter().filter(|e| e.accessible_role() == Some(i_slint_backend_testing::AccessibleRole::Button)).collect();
    assert_eq!(buttons.len(), 1, "one button, in the footer");
    assert_eq!(s.ui.get_property("footer-note").unwrap(), SValue::String("Restart to apply 1 change".into()));
    buttons[0].invoke_accessible_default_action();
    assert_eq!(RELAUNCHED.lock().unwrap().last().map(|argv| argv[1].as_str()), Some("--restarted"));
    settings::forget();
}

#[test]
fn switching_one_keeps_the_list_where_it_was() {
    let (_w, s, admin) = settings_with("plugins-scroll-keep", builtins(), |plugins, _| {
        for n in 0..40 {
            added(plugins, &format!("extra{n}"), json!({}));
        }
    });
    s.show_panel();
    // Near the end of the list: where the page starts it, then with the window small enough to scroll
    let top = || switch_named(&s.ui, "Extra9").absolute_position().y;
    let unscrolled = top();
    s.ui.window().set_size(slint::LogicalSize::new(700.0, 1000.0));
    let offset = scroll_of(&s);
    s.ui.set_property(&offset, SValue::Number(1_000_000.0)).unwrap();
    let range = unscrolled - top();
    assert!(range > 0.0, "forty plugins overflow the page");
    s.ui.set_property(&offset, SValue::Number(range as f64)).unwrap();
    let at = top();
    switch_named(&s.ui, "Extra9").invoke_accessible_default_action();
    s.ui.invoke("answered", &[SValue::Bool(true)]).unwrap();
    assert!(admin.find_installed().iter().any(|i| i.name == "extra9" && i.enabled));
    assert_eq!(s.ui.get_property(&offset).unwrap(), SValue::Number(range as f64));
    assert_eq!(top(), at);
    settings::forget();
}

#[test]
fn clicking_a_switch_at_the_top_that_downloads_first_doesnt_scroll_the_list_away() {
    let mut list = builtins();
    list.push(optional("framing", "Auto framing"));
    let (w, s, admin) = settings_with("plugins-scroll-top", list, |plugins, _| {
        for n in 0..40 {
            added(plugins, &format!("extra{n}"), json!({}));
        }
    });
    s.show_panel();
    s.ui.window().set_size(slint::LogicalSize::new(700.0, 500.0));
    let offset = scroll_of(&s);
    assert_eq!(s.ui.get_property(&offset).unwrap(), SValue::Number(0.0));
    click(&s.ui, &switch_named(&s.ui, "Auto framing"));
    assert_eq!(s.ui.get_property("ask-title").unwrap(), SValue::String("Download about 47 MB?".into()));
    s.ui.invoke("answered", &[SValue::Bool(true)]).unwrap();
    w.process();
    assert_eq!(admin.optional_on().into_iter().collect::<Vec<_>>(), ["framing"]);
    assert_eq!(s.ui.get_property(&offset).unwrap(), SValue::Number(0.0));
    settings::forget();
}

#[test]
fn the_auto_update_switch_looks_red_and_warns_on_the_tab() {
    let (_w, s, _) = settings_with("plugins-auto-red", builtins(), |plugins, _| { added(plugins, "extra", json!({})); });
    s.show_panel();
    let danger = switch_named(&s.ui, "Install updates without asking");
    assert_eq!(danger.accessible_checked(), Some(false));
    let SValue::Model(rows) = s.ui.get_property("update-rows").unwrap() else { panic!("no rows") };
    let auto = rows.iter().find(|r| matches!(r, SValue::Struct(r) if r.get_field("title") == Some(&SValue::String("Install updates without asking".into())))).expect("its row");
    let SValue::Struct(auto) = auto else { panic!("{auto:?}") };
    assert_eq!(auto.get_field("danger"), Some(&SValue::Bool(true)));
    let warning = "Dangerous! Whoever controls a plugin's repo can then run new code on this computer every time they publish a release.";
    assert!(!labelled(&s.ui, warning).is_empty(), "the warning stands on the tab");
    settings::forget();
}

#[test]
fn a_plugin_that_didnt_start_counts_as_a_change_a_restart_applies() {
    let list = builtins();
    let running: Vec<&str> = vec!["connection", "camera_control", "presets"];
    let r = rig_with(list, &running, &[("extra", "RuntimeError: no card today")], |_| {}, None, |plugins, admin| {
        added(plugins, "extra", json!({}));
        admin.set_enabled("extra", true).unwrap();
    });
    assert_eq!(r.panel.pending_changes(), 1);
    // And it says why on its tile
    assert!(labels(&r.panel).iter().any(|t| t.contains("It didn't start (RuntimeError: no card today).")));
}

#[test]
fn a_plugin_that_didnt_start_says_why_in_the_list() {
    let r = rig_with(builtins(), &[], &[("extra", "RuntimeError: no card today")], |_| {}, None, |plugins, admin| {
        added(plugins, "extra", json!({}));
        admin.set_enabled("extra", true).unwrap();
    });
    assert!(labels(&r.panel).iter().any(|t| t.contains("It didn't start (RuntimeError: no card today).")));
    assert_eq!(tile(&r.panel, "Extra").note_kind, "status_err");
}

#[test]
fn one_thats_on_can_be_switched_off_even_with_a_problem() {
    let list = builtins();
    let running: Vec<&str> = vec!["connection", "camera_control", "presets", "extra"];
    let mut r = rig_with(list, &running, &[], |_| {}, None, |plugins, admin| {
        let path = added(plugins, "extra", json!({}));
        admin.set_enabled("extra", true).unwrap();
        let manifest = fs::read_to_string(path.join("plugin.json")).unwrap().replace("\"api\":2", "\"api\":99");
        fs::write(path.join("plugin.json"), manifest).unwrap();
        admin.set_enabled("extra", true).unwrap(); // on, as this version
    });
    let extra = tile(&r.panel, "Extra");
    assert!(extra.enabled && extra.on);
    r.panel.run(Request::Switch { name: "extra".into(), on: false }, &mut yes);
    assert!(!r.admin.find_installed()[0].enabled);
}

#[test]
fn a_plugin_being_written_shows_where_its_from_with_nothing_to_switch() {
    let root = scratch_dir("plugins-panel-dev");
    let work = root.join("work");
    fs::create_dir(&work).unwrap();
    let mine = added(&work, "mine", json!({}));
    let (_, admin) = admin_in(&root, vec![mine.clone()], None, Arc::default());
    let list = builtins();
    let running = names(&list).into_iter().chain(["mine".to_string()]).collect();
    let panel = Panel::new(list, running, Default::default(), admin, Box::new(NoDownload));
    let mine_tile = tile(&panel, "Mine");
    assert!(!mine_tile.enabled && mine_tile.locked && !mine_tile.added);
    assert!(mine_tile.note.starts_with(&format!("Loaded from {} by dev_desktop.py.", mine.display())));
    assert_eq!(mine_tile.badges[0].text, "Being written");
    assert_eq!(panel.pending_changes(), 0);
}

#[test]
fn switching_one_on_asks_to_download_first_then_switches_it_on() {
    let mut list = builtins();
    list.push(optional("framing", "Auto framing"));
    let mut r = rig(list, &all_running());
    let group = r.panel.groups.iter().find(|g| g.id == "vision").unwrap();
    assert_eq!(group.status, "Switching one of these on downloads about 47 MB first, once for all of them.");
    assert_eq!(group.tiles[0].badges[0].text, "Needs the vision download");
    let mut asked = vec![];
    flip(&mut r, "framing", true, &mut |q| { asked.push(q.clone()); false });
    assert!(r.download.started.borrow().is_empty() && r.admin.optional_on().is_empty());
    assert!(!tile(&r.panel, "Auto framing").on);
    flip(&mut r, "framing", true, &mut |q| { asked.push(q.clone()); true });
    let q = asked.last().unwrap();
    assert!(q.title == "Download about 47 MB?" && q.text.contains("MediaPipe") && q.yes == "Download and switch on");
    assert_eq!(*r.download.started.borrow(), ["framing"]);
    assert_eq!(r.admin.optional_on().into_iter().collect::<Vec<_>>(), ["framing"]);
    assert!(tile(&r.panel, "Auto framing").on && r.panel.pending_changes() == 1);
    assert!(!r.panel.groups.iter().find(|g| g.id == "vision").unwrap().remove_pack); // something uses it
}

#[test]
fn with_the_download_there_switching_on_is_just_a_switch() {
    let mut list = builtins();
    list.extend([optional("framing", "Auto framing"), optional("background", "Background")]);
    let mut r = rig_with(list, &["connection"], &[], |d| d.ready.set(true), None, |_, _| {});
    let mut asked = 0;
    flip(&mut r, "background", true, &mut |_| { asked += 1; true });
    assert!(asked == 0 && r.download.started.borrow().is_empty());
    assert_eq!(r.admin.optional_on().into_iter().collect::<Vec<_>>(), ["background"]);
    flip(&mut r, "background", false, &mut yes);
    assert!(r.admin.optional_on().is_empty());
    assert!(r.panel.groups.iter().find(|g| g.id == "vision").unwrap().remove_pack);
    r.panel.run(Request::RemovePack, &mut yes);
    assert_eq!(r.download.removed.get(), 1);
}

#[test]
fn a_failed_download_leaves_it_off_and_says_why() {
    let mut list = builtins();
    list.push(optional("framing", "Auto framing"));
    let mut r = rig_with(list, &["connection"], &[], |d| d.error = "The download failed. Check the internet connection and try again.".into(), None, |_, _| {});
    flip(&mut r, "framing", true, &mut yes);
    assert!(r.admin.optional_on().is_empty() && !tile(&r.panel, "Auto framing").on);
    assert!(labels(&r.panel).iter().any(|t| t.starts_with("The download didn't work. The download failed.")));
}

#[test]
fn without_a_pack_for_this_computer_it_cant_be_switched_on() {
    let mut list = builtins();
    list.push(optional("framing", "Auto framing"));
    let r = rig_with(list, &["connection"], &[], |d| d.available = false, None, |_, _| {});
    assert!(!tile(&r.panel, "Auto framing").enabled);
    assert_eq!(r.panel.groups.iter().find(|g| g.id == "vision").unwrap().status, "There's no vision download for this computer yet.");
}

#[test]
fn installing_a_plugin_thats_there_asks_then_replaces_it() {
    let mut r = rig(builtins(), &["connection", "camera_control", "presets", "extra"]);
    added(&r.plugins, "extra", json!({"version": "1.0"}));
    r.admin.set_enabled("extra", true).unwrap();
    let newer = scratch_dir("newer");
    added(&newer, "extra", json!({"version": "1.1"}));
    let archive = r.root.join("extra.zip");
    zip_of(&newer, "extra", &archive);
    r.panel.refresh();

    let mut asked = vec![];
    r.panel.run(Request::Install { path: archive.clone() }, &mut |q| { asked.push(q.clone()); false });
    assert!(asked[0].title == "Replace Extra 1.0?" && asked[0].text.contains("same settings"));
    assert_eq!(r.admin.find_installed()[0].version, "1.0"); // cancelled: untouched

    let outcome = r.panel.run(Request::Install { path: archive }, &mut yes);
    let item = &r.admin.find_installed()[0];
    assert!(item.version == "1.1" && !item.enabled);
    let note = &outcome.notices[0];
    assert!(note.kind == NoticeKind::Info && note.title == "Replaced" && note.text.contains("Extra 1.1"));
    assert!(!tile(&r.panel, "Extra 1.1").on);
    assert!(labels(&r.panel).contains(&"Waits for a restart".to_string()) && r.panel.pending_changes() == 1); // the old one still runs
}

#[test]
fn a_zip_that_isnt_one_says_so_and_a_new_one_says_where_it_goes() {
    let mut r = rig(builtins(), &all_running());
    let bad = r.root.join("bad.zip");
    fs::write(&bad, "not a zip").unwrap();
    let outcome = r.panel.run(Request::Install { path: bad }, &mut yes);
    assert!(outcome.notices[0].title == "Couldn't install it" && outcome.notices[0].kind == NoticeKind::Warning);
    let source = scratch_dir("fresh");
    added(&source, "fresh", json!({}));
    let archive = r.root.join("fresh.zip");
    zip_of(&source, "fresh", &archive);
    let outcome = r.panel.run(Request::Install { path: archive }, &mut yes);
    assert_eq!(outcome.notices[0].title, "Installed");
    assert_eq!(outcome.notices[0].text, "Fresh is installed but switched off for now. To use it, switch it on and restart Telescope.");
    assert!(!tile(&r.panel, "Fresh").on);
}

#[test]
fn the_repo_shows_as_a_link_next_to_the_author() {
    let r = rig_with(builtins(), &[], &[], |_| {}, None, |plugins, _| {
        added(plugins, "extra", json!({"author": "Someone <b>", "description": "Does things.", "repo": "https://github.com/someone/extra"}));
    });
    assert!(labels(&r.panel).contains(&"Does things.".to_string()));
    let extra = tile(&r.panel, "Extra");
    assert_eq!(extra.byline, "By Someone <b>.");
    assert_eq!(extra.byline_link, Some(Link { text: "github.com/someone/extra".into(), url: "https://github.com/someone/extra".into() }));
}

#[test]
fn a_newer_release_is_mentioned_with_a_link_to_it() {
    let release = NewerRelease { tag: "v1.1".into(), page: "https://github.com/someone/extra/releases/tag/v1.1".into() };
    let mut r = rig_with(builtins(), &[], &[], |_| {}, Some(release.clone()), |plugins, _| {
        added(plugins, "extra", json!({"version": "1.0", "repo": "https://github.com/someone/extra"}));
    });
    let extra = tile(&r.panel, "Extra 1.0").clone();
    assert!(extra.note.contains("Version v1.1 is out.") && extra.note_kind == "status_warn");
    assert_eq!(extra.note_link, Some(Link { text: "Download it from its release page".into(), url: release.page }));
    r.panel.run(Request::CheckUpdates(false), &mut yes);
    assert!(!r.admin.switch(CHECK_UPDATES, true));
    assert!(!labels(&r.panel).iter().any(|t| t.contains("Version v1.1")));
}

#[test]
fn the_auto_update_switch_is_red_warns_and_asks_first() {
    let mut r = rig_with(builtins(), &[], &[], |_| {}, None, |plugins, _| { added(plugins, "extra", json!({})); });
    let rows = r.panel.update_rows.clone().unwrap();
    assert!(!rows.auto_on && rows.auto_enabled && rows.check_on);
    let mut asked = vec![];
    r.panel.run(Request::AutoUpdate(true), &mut |q| { asked.push(q.clone()); false });
    assert!(asked[0].danger && asked[0].text.contains("every time they publish a release"));
    assert!(!r.panel.update_rows.clone().unwrap().auto_on && !r.admin.switch(AUTO_UPDATE, false));
    r.panel.run(Request::AutoUpdate(true), &mut yes);
    assert!(r.admin.switch(AUTO_UPDATE, false) && r.checks.load(Ordering::SeqCst) == 1);
    assert!(r.panel.update_rows.clone().unwrap().auto_on);
    let count = asked.len();
    r.panel.run(Request::AutoUpdate(false), &mut |q| { asked.push(q.clone()); true }); // off again without asking
    assert!(asked.len() == count && !r.admin.switch(AUTO_UPDATE, true));
}

#[test]
fn without_the_check_the_auto_switch_cant_go_on() {
    let r = rig_with(builtins(), &[], &[], |_| {}, None, |plugins, admin| {
        added(plugins, "extra", json!({}));
        admin.set_switch(CHECK_UPDATES, false).unwrap();
    });
    let rows = r.panel.update_rows.unwrap();
    assert!(!rows.auto_enabled && !rows.check_on);
}

#[test]
fn with_no_added_plugins_there_are_no_update_switches() {
    assert!(rig(builtins(), &[]).panel.update_rows.is_none());
}

fn grouped() -> Vec<PluginInfo> {
    let camera = PluginInfo { category: "camera".into(), about: "Makes the picture bigger.".into(), ..info("zoom_tool", "Zoom tool") };
    let other = PluginInfo { category: "camera".into(), ..info("alpha", "Alpha") };
    let sound = PluginInfo { category: "sound".into(), ..info("echo", "Echo") };
    let lost = PluginInfo { category: "no_such_group".into(), ..info("lost", "Lost") };
    let mut all = builtins();
    all.extend([camera, other, sound, lost]);
    all
}

fn group_names(panel: &Panel, query: &str, filter: Filter) -> Vec<String> {
    panel.shown(query, filter).into_iter().map(|(g, _)| g.title).collect()
}

#[test]
fn tiles_are_grouped_by_category_and_sorted_by_name() {
    let r = rig(grouped(), &[]);
    assert_eq!(group_names(&r.panel, "", Filter::All), ["Camera", "Sound", "Extras"]); // empty groups aren't shown
    assert_eq!(r.panel.shown_tiles("", Filter::All), ["Alpha", "Zoom tool", "Echo", "Camera", "Connection", "Lost", "Presets"]);
}

#[test]
fn the_filter_shows_the_ones_on_or_off() {
    let mut r = rig(grouped(), &[]);
    r.admin.set_builtin_off("presets", true).unwrap();
    r.admin.set_builtin_off("echo", true).unwrap();
    r.panel.refresh();
    assert_eq!(r.panel.shown_tiles("", Filter::Off), ["Echo", "Presets"]);
    assert_eq!(r.panel.shown_tiles("", Filter::On), ["Alpha", "Zoom tool", "Camera", "Connection", "Lost"]); // a required one is on
    assert!(r.panel.shown_tiles("echo", Filter::On).is_empty() && Panel::empty_note("echo", Filter::On) == "No plugins match “echo”.");
    r.admin.all_builtin_on().unwrap();
    r.panel.refresh();
    assert!(r.panel.shown_tiles("", Filter::Off).is_empty());
    assert_eq!(Panel::empty_note("", Filter::Off), "Every plugin is switched on.");
}

#[test]
fn the_search_finds_by_name_and_description() {
    let r = rig(grouped(), &[]);
    assert_eq!(r.panel.shown_tiles("pres", Filter::All), ["Presets"]);
    assert_eq!(r.panel.shown_tiles("picture bigger", Filter::All), ["Zoom tool"]); // every word, anywhere in the name or the description
    assert!(r.panel.shown_tiles("nothing like it", Filter::All).is_empty());
    assert_eq!(Panel::empty_note("nothing like it", Filter::All), "No plugins match “nothing like it”.");
    assert_eq!(Panel::empty_note("", Filter::All), "No plugin is switched on.");
    assert_eq!(r.panel.shown_tiles("", Filter::All).len(), 7);
}

#[test]
fn the_search_finds_an_added_plugin_by_its_author() {
    let r = rig_with(builtins(), &[], &[], |_| {}, None, |plugins, _| { added(plugins, "extra", json!({"author": "Ada Lovelace"})); });
    assert_eq!(r.panel.shown_tiles("lovelace", Filter::All), ["Extra"]);
}

#[test]
fn the_footer_counts_the_changes_that_wait_for_a_restart() {
    let list = grouped();
    let running: Vec<String> = list.iter().map(|p| p.name.clone()).collect();
    let mut r = rig(list, &running.iter().map(String::as_str).collect::<Vec<_>>());
    assert_eq!(r.panel.pending_changes(), 0);
    flip(&mut r, "presets", false, &mut yes);
    assert_eq!(r.panel.footer_note(), "Restart to apply 1 change");
    flip(&mut r, "echo", false, &mut yes);
    assert!(r.panel.footer_note() == "Restart to apply 2 changes" && r.panel.pending_changes() == 2);
    r.panel.run(Request::AllOn, &mut yes);
    assert_eq!(r.panel.pending_changes(), 0);
}

#[test]
fn the_footer_counts_a_removed_plugin_that_still_runs() {
    let r = rig(builtins(), &["connection", "camera_control", "presets", "gone"]);
    assert_eq!(r.panel.pending_changes(), 1);
}

#[test]
fn required_plugins_are_locked_on_and_say_so() {
    let r = rig(builtins(), &[]);
    let connection = tile(&r.panel, "Connection");
    assert!(connection.on && !connection.enabled);
    assert!(connection.badges.iter().any(|b| b.text == "Required"));
    assert!(connection.tooltip.contains("can't be switched off"));
}

#[test]
fn an_added_plugin_goes_in_the_group_its_plugin_json_names() {
    let r = rig_with(builtins(), &[], &[], |_| {}, None, |plugins, _| {
        added(plugins, "extra", json!({"category": "sound"}));
        added(plugins, "other", json!({"category": "made_up"}));
    });
    let group = |id: &str| r.panel.groups.iter().find(|g| g.id == id).unwrap().tiles.iter().map(|t| t.title.clone()).collect::<Vec<_>>();
    assert_eq!(group("sound"), ["Extra"]);
    assert!(group("extras").contains(&"Other".to_string()));
    assert!(labels(&r.panel).contains(&"Added".to_string()));
}

#[test]
fn a_dropped_file_has_to_be_one_zip() {
    let one = Path::new("/home/me/Downloads/Plugin.ZIP");
    assert_eq!(Panel::dropped_zip(&[one]), Some(one.to_path_buf()));
    assert_eq!(Panel::dropped_zip(&[one, one]), None);
    assert_eq!(Panel::dropped_zip(&[Path::new("/home/me/notes.txt")]), None);
    assert_eq!(Panel::dropped_zip(&[]), None);
}

#[test]
fn removing_one_asks_and_deletes_its_folder() {
    let mut r = rig_with(builtins(), &[], &[], |_| {}, None, |plugins, _| { added(plugins, "extra", json!({})); });
    let mut asked = vec![];
    r.panel.run(Request::Remove { name: "extra".into() }, &mut |q| { asked.push(q.clone()); false });
    assert!(r.plugins.join("extra").is_dir());
    r.panel.run(Request::Remove { name: "extra".into() }, &mut |q| { asked.push(q.clone()); true });
    assert!(!r.plugins.join("extra").exists() && r.panel.installed.is_empty());
    assert!(asked[0].title == "Remove Extra?" && asked[0].danger && asked[0].text.contains("(extra)"));
}

#[test]
fn the_columns_follow_the_width() {
    assert_eq!((columns(645.0), columns(740.0), columns(390.0)), (2, 3, 1));
}

#[test]
fn the_tab_says_how_many_are_off() {
    assert_eq!(Panel::tab_title(16), "Plugins (16 off)");
    assert_eq!(Panel::tab_title(0), "");
}

#[test]
fn a_link_is_split_out_of_a_text() {
    let (plain, link) = split_link("Needs <a href=\"https://x.test/\">a thing</a> to work.");
    assert_eq!(plain, "Needs a thing to work.");
    assert_eq!(link, Some(Link { text: "a thing".into(), url: "https://x.test/".into() }));
    assert_eq!(split_link("No link."), ("No link.".to_string(), None));
}
