//! The settings window (widgets/settings_window.py): tabs down the left, one scrolling page of plugins' sections for
//! each, the Plugins tab, and the search over all of it. Built from the plugins' SettingsSections the first time it
//! opens, then kept.

use crate::doc::{self, Piece, SectionPlace};
use crate::plugins_tab::{self, group_icon, Commit, Download, Filter, NoticeKind, Notice, Outcome, Panel, Question, Request, Step, VisionDownload};
use crate::search::{search_ex, Entry};
use crate::source::{scan, to_json, to_property};
use crate::window::Window;
use crate::texts::fill;
use i_slint_core::accessibility::AccessibleStringProperty;
use i_slint_core::item_tree::{ItemRc, ParentItemTraversalMode};
use i_slint_core::items::{AccessibleRole, Clip};
use i_slint_core::window::WindowInner;
use slint::{ComponentHandle, Image, ModelRc, VecModel};
use slint_interpreter::{ComponentInstance, Struct, Value as SValue};
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::ops::ControlFlow;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use telescope_core::i18n::tr;
use telescope_core::plugin::SETTINGS_TABS;
use telescope_core::ui::{SettingsSection, SlintSource, UiEvent};
use telescope_core::plugin_admin::PluginAdmin;

const FLASH_MS: u64 = 1800;
/// The size it opens at and its minimum, as in the window's source, in logical pixels.
const SIZE: (f32, f32) = (940.0, 680.0);
const LEAST: (f32, f32) = (720.0, 480.0);
/// What a window keeps clear of the screen's edges (controls.SCREEN_MARGIN).
const SCREEN_MARGIN: f32 = 48.0;

type Rect = (f32, f32, f32, f32);

/// (tab id, its name, the sections on it in order) for the tabs that have any (settings_window.ordered_tabs).
pub fn ordered_tabs(sections: &[(String, SettingsSection)], plugins_title: &str) -> Vec<(String, String, Vec<usize>)> {
    let mut names: Vec<(String, String)> = SETTINGS_TABS.iter().map(|(id, n)| (id.to_string(), n.to_string())).collect();
    let mut extras = Vec::new();
    for (_, s) in sections {
        if !names.iter().any(|(id, _)| *id == s.tab) && !extras.contains(&s.tab) {
            let mut name = s.tab.replace('_', " ");
            if let Some(first) = name.get(..1) {
                name = first.to_uppercase() + &name[1..].to_lowercase();
            }
            names.push((s.tab.clone(), name));
            extras.push(s.tab.clone());
        }
        if !s.tab_title.is_empty() {
            if let Some(n) = names.iter_mut().find(|(id, _)| *id == s.tab) {
                n.1 = s.tab_title.clone();
            }
        }
    }
    let mut ids: Vec<String> = SETTINGS_TABS.iter().map(|(id, _)| id.to_string()).collect();
    let at = ids.iter().position(|i| i == "plugins").unwrap_or(ids.len());
    ids.splice(at..at, extras);
    ids.into_iter().filter_map(|tab| {
        let mut mine: Vec<usize> = (0..sections.len()).filter(|&i| sections[i].1.tab == tab).collect();
        mine.sort_by_key(|&i| sections[i].1.order);
        let name = names.iter().find(|(id, _)| *id == tab).map(|(_, n)| n.clone()).unwrap_or_default();
        if tab == "plugins" {
            let name = if plugins_title.is_empty() { tr(&name) } else { plugins_title.to_string() };
            return Some((tab, name, mine));
        }
        (!mine.is_empty()).then(|| (tab.clone(), tr(&name), mine))
    }).collect()
}

fn tab_icon(tab: &str) -> &'static str {
    match tab {
        "general" => "sliders",
        "streaming" => "bolt",
        "video" => "video",
        "audio" => "mic",
        "devices" => "phone",
        "shortcuts" => "keyboard",
        "plugins" => "plug",
        "about" => "download",
        _ => "sliders",
    }
}

/// One of the kit's icons by name: a line icon, else the vector icon of that name.
fn icon(name: &str) -> Image {
    let dir = telescope_ui_kit::library_path().join("icons");
    let path = [format!("{name}.svg"), format!("v_{name}.svg")].into_iter().map(|f| dir.join(f)).find(|p| p.exists());
    path.and_then(|p| Image::load_from_path(&p).ok()).unwrap_or_default()
}

fn s(text: &str) -> SValue {
    SValue::String(text.into())
}

fn model(rows: Vec<SValue>) -> SValue {
    SValue::Model(ModelRc::new(VecModel::from(rows)))
}

fn hue(group: &str, own: bool) -> slint::Color {
    let hex = if !own { "#aa9cf5" } else {
        match group {
            "camera" => "#7fd3c4", "look" => "#e6a6e0", "vision" => "#bcd98c", "preview" => "#f2b48c",
            "on_screen" => "#f2a0bf", "sound" => "#e8d08a", "streaming" => "#8cc4f2", _ => "#b9bdcc",
        }
    };
    crate::source::parse_color(hex).unwrap_or_default()
}

pub struct Settings {
    pub ui: ComponentInstance,
    tabs: Vec<(String, String)>,
    plugins_tab: Option<usize>,
    /// (plugin, section id, element id) of every section and footer.
    pieces: Vec<(String, String, String)>,
    /// (plugin, section id) of the sections on each tab, which hear "shown" when it comes on screen.
    tab_sections: Vec<Vec<(String, String)>>,
    /// (element id, search words) of the sections on each tab that have any (SettingsSection.keywords).
    section_words: Vec<Vec<(String, String)>>,
    panel: RefCell<Panel>,
    /// What a search looks through, read when it starts (rows come and go) and dropped when it ends.
    entries: RefCell<Vec<Entry>>,
    hits: RefCell<Vec<Entry>>,
    /// What waits for an answer to the question on screen, and the messages that wait their turn after it.
    asking: RefCell<Option<Commit>>,
    notices: RefCell<VecDeque<Notice>>,
    /// A message is on screen (it has no commit to wait for).
    telling: Cell<bool>,
    window: Weak<Window>,
    fitted: Cell<bool>,
    flash: slint::Timer,
    /// Looks at the download and the update check while the window is up, and shows what they found.
    watch: slint::Timer,
    seen: RefCell<String>,
}

thread_local! {
    /// The timers that run the update check (load_plugins starts them), and how many checks have finished.
    static UPDATE_TIMERS: RefCell<Vec<slint::Timer>> = const { RefCell::new(Vec::new()) };
    static CHECKED: Cell<u64> = const { Cell::new(0) };
}

/// After the app's own update check, and well clear of starting up (plugin_updater._FIRST_CHECK_MS).
const FIRST_CHECK: std::time::Duration = std::time::Duration::from_secs(30);
/// How often the window asks after that.
const TICK: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// Starts the update check for added plugins: a while after start, then hourly, with the finished ones taken in as they
/// come (PluginUpdater.start).
pub fn start_plugin_updates(admin: Arc<dyn PluginAdmin>) {
    let timers = [(slint::TimerMode::SingleShot, FIRST_CHECK), (slint::TimerMode::Repeated, TICK)].into_iter().map(|(mode, every)| {
        let timer = slint::Timer::default();
        let admin = admin.clone();
        timer.start(mode, every, move || admin.maybe_check_updates());
        timer
    }).collect::<Vec<_>>();
    let poll = slint::Timer::default();
    poll.start(slint::TimerMode::Repeated, std::time::Duration::from_secs(5), move || { poll_updates(admin.as_ref()); });
    UPDATE_TIMERS.with(|t| *t.borrow_mut() = timers.into_iter().chain([poll]).collect());
}

/// Takes in a finished check; true when one had (the Plugins tab then shows what's newer).
fn poll_updates(admin: &dyn PluginAdmin) -> bool {
    let found = admin.poll_updates();
    if found {
        CHECKED.with(|c| c.set(c.get() + 1));
    }
    found
}

thread_local! {
    static SETTINGS: RefCell<Option<Rc<Settings>>> = const { RefCell::new(None) };
}

/// The settings window, once it's been built.
pub fn window() -> Option<ComponentInstance> {
    SETTINGS.with(|s| s.borrow().as_ref().map(|s| s.ui.clone_strong()))
}

/// The settings window, built the first time it's asked for; on `tab` if that's one it shows.
/// Host::set_plugin_hues reaches the settings window too, which has its own Theme.
pub fn set_hues(on: bool) {
    if let Some(settings) = SETTINGS.with(|s| s.borrow().clone()) {
        let _ = settings.ui.set_global_property("Theme", "own-hues", SValue::Bool(on));
        settings.show_panel();
    }
}

pub fn open(window: &Rc<Window>, tab: &str) {
    let existing = SETTINGS.with(|s| s.borrow().clone());
    let settings = match existing {
        Some(s) => s,
        None => match build(window) {
            Ok(s) => {
                SETTINGS.with(|slot| *slot.borrow_mut() = Some(s.clone()));
                if let Ok(own) = window.ui.get_global_property("Theme", "own-hues") {
                    let _ = s.ui.set_global_property("Theme", "own-hues", own);
                }
                s
            }
            Err(e) => {
                log::error!("The settings window couldn't be built: {e}");
                return;
            }
        },
    };
    let coming_up = !settings.ui.window().is_visible();
    if !tab.is_empty() {
        settings.show_tab(tab);
    }
    crate::window::set_app_icon(&settings.ui);
    let _ = settings.ui.show();
    crate::parent::attach(&settings.ui, &window.ui);
    if coming_up {
        settings.keep_on_screen();
        settings.tell_shown();
    } else {
        crate::parent::raise(&settings.ui, &window.ui);
    }
    let _ = settings.ui.invoke("focus-search", &[]);
}

/// Drops the settings window, so the next open builds it again (each scene has a window of its own).
pub fn forget() {
    SETTINGS.with(|s| *s.borrow_mut() = None);
}

/// The window if it's been built, for tests and scenes.
pub fn current() -> Option<Rc<Settings>> {
    SETTINGS.with(|s| s.borrow().clone())
}

pub fn build(window: &Rc<Window>) -> Result<Rc<Settings>, String> {
    let mut sections: Vec<(String, SettingsSection)> = Vec::new();
    {
        let mut state = window.state.borrow_mut();
        for i in 0..state.slots.len() {
            let name = state.slots[i].info.name.clone();
            match state.call(i, |p, _| p.settings_sections()) {
                Some(list) => sections.extend(list.into_iter().map(|s| (name.clone(), s))),
                None => log::error!("Plugin {name} couldn't list its settings"),
            }
        }
    }
    let (panel, off, categories) = {
        let state = window.state.borrow();
        let categories: std::collections::HashMap<String, String> = state.slots.iter().map(|s| (s.info.name.clone(), s.info.category.clone())).collect();
        let running = state.slots.iter().map(|s| s.info.name.clone()).collect();
        let download: Box<dyn Download> = match &state.vision {
            Some(service) => Box::new(VisionDownload::new(service.clone())),
            None => Box::new(plugins_tab::NoVision),
        };
        (Panel::new(state.builtins.clone(), running, state.problems.clone(), state.admin.clone(), download), state.off.len(), categories)
    };
    let tabs = ordered_tabs(&sections, &Panel::tab_title(off));
    let plugins_tab = tabs.iter().position(|(id, _, _)| id == "plugins");
    let (mut places, mut files, mut pieces) = (Vec::new(), Vec::new(), Vec::new());
    for (t, (_, _, mine)) in tabs.iter().enumerate() {
        for &i in mine {
            let (plugin, section) = &sections[i];
            let id = format!("s{i}");
            let (file, text) = source_of(plugin, &section.id, &section.ui.source);
            files.push((file.clone(), text.clone()));
            pieces.push((plugin.clone(), section.id.clone(), id.clone()));
            let footer = section.footer.as_ref().map(|f| {
                let fid = format!("f{i}");
                let (ffile, ftext) = source_of(plugin, &format!("{}-footer", section.id), &f.source);
                files.push((ffile.clone(), ftext.clone()));
                pieces.push((plugin.clone(), format!("{}-footer", section.id), fid.clone()));
                Piece { id: fid, file: ffile, component: f.component.clone(), iface: scan(&ftext, &f.component) }
            });
            let card = text.contains(&format!("component {} inherits Card", section.ui.component));
            // A card takes its plugin's colour unless it says another group's (add_card_header's hue=)
            let own_group = section.ui.props.get("card-group").and_then(|g| g.as_str()).map(String::from);
            let hue = card.then(|| own_group.unwrap_or_else(|| categories.get(plugin).cloned().unwrap_or_default()));
            places.push(SectionPlace { piece: Piece { id, file, component: section.ui.component.clone(), iface: scan(&text, &section.ui.component) }, tab: t, footer, hue });
        }
    }
    let section_words = tabs.iter().map(|(_, _, mine)| mine.iter().filter(|&&i| !sections[i].1.keywords.is_empty())
        .map(|&i| (format!("s{i}"), sections[i].1.keywords.clone())).collect()).collect();
    let mut tab_sections = vec![Vec::new(); tabs.len()];
    for place in &places {
        if let Some((plugin, id, _)) = pieces.iter().find(|p| p.2 == place.piece.id) {
            tab_sections[place.tab].push((plugin.clone(), id.clone()));
        }
    }
    let text = doc::settings(tabs.len(), plugins_tab, &places);
    let def = crate::compile::compile("settings.slint", &text, files, "TelescopeSettings")?;
    // Under the main window, as Python's dialog has it as its parent
    let ui = crate::parent::make(&window.ui, || def.create()).map_err(|e| e.to_string())?;
    crate::keys::watch_scan_codes(&ui);
    let settings = Rc::new(Settings {
        ui,
        tabs: tabs.iter().map(|(id, n, _)| (id.clone(), n.clone())).collect(),
        plugins_tab,
        pieces,
        tab_sections,
        section_words,
        panel: RefCell::new(panel),
        entries: RefCell::new(Vec::new()),
        hits: RefCell::new(Vec::new()),
        asking: RefCell::new(None),
        notices: RefCell::new(VecDeque::new()),
        telling: Cell::new(false),
        window: Rc::downgrade(window),
        fitted: Cell::new(false),
        flash: slint::Timer::default(),
        watch: slint::Timer::default(),
        seen: RefCell::new(String::new()),
    });
    settings.wire(&places, &sections);
    settings.show_panel();
    settings.start_watching();
    settings.show_row(0);
    Ok(settings)
}

fn under(item: &ItemRc) -> Vec<ItemRc> {
    let mut items = Vec::new();
    item.visit_descendants(|i| {
        items.push(i.clone());
        ControlFlow::<()>::Continue(())
    });
    items
}

fn say(item: &ItemRc, what: AccessibleStringProperty) -> String {
    item.accessible_string_property(what).map(|s| s.to_string()).unwrap_or_default()
}

/// Whether something between the item and the page has `visible: false`, which Slint turns into a clip around it.
fn hidden(item: &ItemRc, page: &ItemRc) -> bool {
    let mut node = Some(item.clone());
    while let Some(i) = node.filter(|i| i != page) {
        if i.downcast::<Clip>().is_some_and(|c| c.as_pin_ref().is_visibility_clip() && c.as_pin_ref().clip()) {
            return true;
        }
        node = i.parent_item(ParentItemTraversalMode::StopAtPopups);
    }
    false
}

/// The words on the marker of this kind under a row or a card.
fn words_of(element: &ItemRc, kind: &str) -> String {
    let marker = under(element).into_iter().find(|e| say(e, AccessibleStringProperty::Id) == kind);
    marker.map(|m| say(&m, AccessibleStringProperty::Label)).unwrap_or_default()
}

/// What the controls in a row say (settings_search._row_texts' extra).
fn controls_of(element: &ItemRc, name: &str) -> String {
    let mut said: Vec<String> = under(element).iter()
        .filter(|e| say(e, AccessibleStringProperty::Id) == "control-words" || !matches!(e.accessible_role(), AccessibleRole::None | AccessibleRole::Text))
        .map(|e| say(e, AccessibleStringProperty::Label)).filter(|l| !l.trim().is_empty() && l != name).collect();
    said.dedup();
    said.join(" ")
}

fn source_of(plugin: &str, ui: &str, source: &SlintSource) -> (String, String) {
    match source {
        SlintSource::Text(text) => (format!("plugins/{plugin}/settings-{ui}.slint"), text.clone()),
        SlintSource::File(path) => (path.clone(), std::fs::read_to_string(path).unwrap_or_default()),
    }
}

impl Settings {
    fn wire(self: &Rc<Self>, places: &[SectionPlace], sections: &[(String, SettingsSection)]) {
        let ui = &self.ui;
        let rows: Vec<SValue> = self.tabs.iter().map(|(id, name)| {
            SValue::Struct(Struct::from_iter([
                ("id".to_string(), s(id)),
                ("name".to_string(), s(name)),
                ("icon".to_string(), SValue::Image(icon(tab_icon(id)))),
            ]))
        }).collect();
        let _ = ui.set_property("tabs", model(rows));
        if let Some(w) = self.window.upgrade() {
            let state = w.state.clone();
            let _ = ui.set_global_callback("Folds", "is-folded", move |args| {
                let key = match args.first() { Some(SValue::String(s)) => s.to_string(), _ => String::new() };
                SValue::Bool(state.borrow().collapse.is_folded(&key))
            });
        }
        // Each section's starting properties and its callbacks, which go to its plugin as UiEvents
        for place in places {
            for piece in std::iter::once(&place.piece).chain(place.footer.as_ref()) {
                let Some((plugin, section_id, _)) = self.pieces.iter().find(|p| p.2 == piece.id).cloned() else { continue };
                if let Some((_, section)) = sections.iter().find(|(p, s)| *p == plugin && (s.id == section_id || format!("{}-footer", s.id) == section_id)) {
                    let props = if section_id.ends_with("-footer") { section.footer.as_ref().map(|f| f.props.clone()) } else { Some(section.ui.props.clone()) };
                    if let Some(props) = props {
                        self.set_props(&plugin, &section_id, &props);
                    }
                }
                for cb in &piece.iface.callbacks {
                    let (plugin, section_id, callback, weak) = (plugin.clone(), section_id.clone(), cb.clone(), Rc::downgrade(self));
                    let _ = ui.set_callback(&format!("{}-{cb}", piece.id), move |args| {
                        if let Some(w) = weak.upgrade().and_then(|s| s.window.upgrade()) {
                            let event = UiEvent { ui: section_id.clone(), callback: callback.clone(), args: args.iter().map(to_json).collect() };
                            w.state.borrow_mut().ui_event(&plugin, event);
                            w.process();
                        }
                        SValue::Void
                    });
                }
            }
        }
        let cb = |s: &Rc<Self>, name: &str, f: fn(&Rc<Settings>, &[SValue])| {
            let weak = Rc::downgrade(s);
            let _ = s.ui.set_callback(name, move |args| {
                if let Some(s) = weak.upgrade() {
                    f(&s, args);
                }
                SValue::Void
            });
        };
        let weak = Rc::downgrade(self);
        let _ = ui.set_callback("key", move |args| {
            let Some(w) = weak.upgrade().and_then(|s| s.window.upgrade()) else { return SValue::Bool(false) };
            let (released, typing) = (args.get(1) == Some(&SValue::Bool(true)), args.get(2) == Some(&SValue::Bool(true)));
            let input = crate::keys::key_input(&args[0], released, typing, w.modal());
            SValue::Bool(w.key(&input))
        });
        let weak = Rc::downgrade(self);
        let _ = ui.set_global_callback("Reveal", "show", move |args| {
            if let (Some(st), [_, SValue::Number(y), _, SValue::Number(h)]) = (weak.upgrade(), args) {
                st.reveal(*y as f32, *h as f32);
            }
            SValue::Void
        });
        cb(self, "tab-picked", |s, a| if let Some(SValue::Number(i)) = a.first() { s.on_row(*i as usize) });
        cb(self, "query-changed", |s, a| if let Some(SValue::String(t)) = a.first() { s.on_query(t) });
        cb(self, "step", |s, a| if let Some(SValue::Number(d)) = a.first() { s.step(*d as i32) });
        cb(self, "open-result", |s, a| if let Some(SValue::Number(i)) = a.first() { s.open_hit(*i as usize) });
        cb(self, "open-current", |s, _| {
            let i = match s.ui.get_property("result") { Ok(SValue::Number(n)) => n.max(0.0) as usize, _ => 0 };
            s.open_hit(i);
        });
        cb(self, "plugins-filter-changed", |s, _| s.show_panel());
        cb(self, "show-all", |s, _| {
            let _ = s.ui.set_property("plugin-query", s_empty());
            let _ = s.ui.set_property("plugin-filter", SValue::Number(0.0));
            s.show_panel();
        });
        cb(self, "plugin-switched", |s, a| {
            if let (Some(SValue::String(name)), Some(SValue::Bool(on))) = (a.first(), a.get(1)) {
                s.request(Request::Switch { name: name.to_string(), on: *on });
            }
        });
        cb(self, "plugin-removed", |s, a| {
            if let Some(SValue::String(name)) = a.first() {
                s.request(Request::Remove { name: name.to_string() });
            }
        });
        cb(self, "remove-pack", |s, _| s.request(Request::RemovePack));
        cb(self, "update-switched", |s, a| {
            if let (Some(SValue::String(id)), Some(SValue::Bool(on))) = (a.first(), a.get(1)) {
                s.request(if id.as_str() == "auto" { Request::AutoUpdate(*on) } else { Request::CheckUpdates(*on) });
            }
        });
        cb(self, "answered", |s, a| s.answered(matches!(a.first(), Some(SValue::Bool(true)))));
        cb(self, "all-on", |s, _| s.request(Request::AllOn));
        cb(self, "restart", |s, _| {
            let streaming = s.window.upgrade().is_some_and(|w| w.is_streaming());
            s.request(Request::Restart { streaming });
        });
        cb(self, "open-docs", |_, _| open_url(plugins_tab::DOCS_URL));
        cb(self, "open-url", |_, a| if let Some(SValue::String(url)) = a.first() { open_url(url) });
        cb(self, "open-folder", |s, _| s.open_plugins_folder());
        cb(self, "install", |s, _| s.pick_zip());
    }

    pub fn set_props(&self, plugin: &str, ui: &str, props: &serde_json::Value) {
        let Some((_, _, id)) = self.pieces.iter().find(|p| p.0 == plugin && p.1 == ui) else { return };
        for (k, v) in props.as_object().into_iter().flatten() {
            let name = format!("{id}-{k}");
            let _ = self.ui.set_property(&name, to_property(&self.ui, &name, v));
        }
    }

    /// Scrolls the page so a control that just got keyboard focus shows, 40 px clear of the edge like ensureWidgetVisible.
    fn reveal(&self, y: f32, h: f32) {
        let t = match self.ui.get_property("current") { Ok(SValue::Number(n)) => n as usize, _ => 0 };
        let num = |n: &str| match self.ui.get_property(n) { Ok(SValue::Number(v)) => v as f32, _ => 0.0 };
        let (offset, height) = (num(&format!("page{t}-offset")), num(&format!("page{t}-height")));
        let (top, margin) = (53.0, 40.0);
        let to = if y < top { offset - (top - y) - margin } else if y + h > top + height { offset + (y + h - top - height) + margin } else { return };
        let _ = self.ui.set_property(&format!("page{t}-offset"), SValue::Number(to.max(0.0) as f64));
    }

    /// The tab ids shown, top to bottom.
    pub fn tabs(&self) -> Vec<String> {
        self.tabs.iter().map(|(id, _)| id.clone()).collect()
    }

    pub fn current_tab(&self) -> String {
        let i = match self.ui.get_property("current") { Ok(SValue::Number(n)) => n as usize, _ => 0 };
        self.tabs.get(i).map(|t| t.0.clone()).unwrap_or_default()
    }

    /// Runs a callback of a plugin's settings section as if it had been clicked (scene renders open its dialogs this way).
    pub fn call(&self, plugin: &str, ui: &str, callback: &str) -> bool {
        self.call_with(plugin, ui, callback, &[])
    }

    /// call with the callback's arguments.
    pub fn call_with(&self, plugin: &str, ui: &str, callback: &str, args: &[SValue]) -> bool {
        let Some((_, _, id)) = self.pieces.iter().find(|p| p.0 == plugin && p.1 == ui) else { return false };
        let done = self.ui.invoke(&format!("{id}-{callback}"), args).is_ok();
        if let Some(w) = self.window.upgrade() {
            w.process();
        }
        done
    }

    /// Go to the tab with this id; false if there isn't one.
    pub fn show_tab(&self, tab: &str) -> bool {
        let Some(i) = self.tabs.iter().position(|(id, _)| id == tab) else { return false };
        let moved = self.leave_search() || self.current_tab() != tab;
        let _ = self.ui.set_property("current", SValue::Number(i as f64));
        self.show_row(i);
        if moved && self.ui.window().is_visible() {
            self.tell_shown();
        }
        true
    }

    /// The first time it opens: _fit_to_screen, from the work area of the screen it came up on (once the system has made the
    /// window), and moved back in where it sticks out.
    fn keep_on_screen(&self) {
        if self.fitted.replace(true) {
            return;
        }
        crate::screen::when_made(&self.ui, |w| crate::screen::work_area(w).filter(|_| w.inner_size().width > 0), |ui, area| {
            let Some(settings) = SETTINGS.with(|s| s.borrow().clone()) else { return };
            let scale = ui.window().scale_factor();
            settings.fit_to_screen((area.width as f32 / scale, area.height as f32 / scale));
            crate::screen::keep_inside(ui, ((320.0 * scale) as u32, (240.0 * scale) as u32));
        });
    }

    /// Opens at its usual size unless the screen is smaller, keeping clear of its edges; a small screen also wins over
    /// the window's minimum. In logical pixels.
    pub fn fit_to_screen(&self, screen: (f32, f32)) {
        let (w, h) = (SIZE.0.min(screen.0 - SCREEN_MARGIN), SIZE.1.min(screen.1 - SCREEN_MARGIN));
        let _ = self.ui.set_property("least-width", SValue::Number(LEAST.0.min(w) as f64));
        let _ = self.ui.set_property("least-height", SValue::Number(LEAST.1.min(h) as f64));
        self.ui.window().set_size(slint::LogicalSize::new(w, h));
    }

    fn show_row(&self, row: usize) {
        if let Some((_, name)) = self.tabs.get(row) {
            let _ = self.ui.set_property("page-title", s(name));
        }
    }

    fn on_row(&self, row: usize) {
        self.leave_search();
        self.show_row(row);
        self.tell_shown();
    }

    /// The sections on the tab in front hear "shown", as a SettingsSection's on_show is called each time its tab comes
    /// on screen: they bring what they show up to date.
    pub fn tell_shown(&self) {
        let tab = match self.ui.get_property("current") { Ok(SValue::Number(n)) => n as usize, _ => 0 };
        let Some(w) = self.window.upgrade() else { return };
        for (plugin, id) in self.tab_sections.get(tab).into_iter().flatten() {
            w.state.borrow_mut().ui_event(plugin, UiEvent { ui: id.clone(), callback: "shown".into(), args: Vec::new() });
        }
        w.process();
    }

    /// Sets an image property on one of a plugin's sections.
    pub fn set_image(&self, plugin: &str, ui: &str, prop: &str, image: slint::Image) {
        let Some((_, _, id)) = self.pieces.iter().find(|p| p.0 == plugin && p.1 == ui) else { return };
        if let Err(e) = self.ui.set_property(&format!("{id}-{prop}"), SValue::Image(image)) {
            log::warn!("{plugin} set {prop} on its {ui}: {e:?}");
        }
    }

    /// Back on the tab; true if it was showing results.
    fn leave_search(&self) -> bool {
        let was = self.ui.get_property("searching") == Ok(SValue::Bool(true));
        let _ = self.ui.set_property("query", s_empty());
        let _ = self.ui.set_property("searching", SValue::Bool(false));
        self.entries.borrow_mut().clear();
        self.hits.borrow_mut().clear();
        let i = match self.ui.get_property("current") { Ok(SValue::Number(n)) => n as usize, _ => 0 };
        self.show_row(i);
        was
    }

    /// Every setting the search can find, read off the window as it is now (settings_search.collect). The items are
    /// walked by hand: Slint's ElementHandle leaves out whatever is scrolled off the page.
    pub fn entries(&self) -> Vec<Entry> {
        let root = ItemRc::new_root(WindowInner::from_pub(self.ui.window()).component());
        let items = under(&root);
        let mut out = Vec::new();
        for (t, (tab, name)) in self.tabs.iter().enumerate() {
            // The Plugins tab and each of its groups are also found by what people call plugins
            let words = if tab == "plugins" { plugins_tab::search_keywords() } else { String::new() };
            let base = Entry {
                tab: tab.clone(), tab_name: name.clone(), card: String::new(), name: name.clone(), detail: String::new(),
                keywords: String::new(), extra: String::new(), card_keywords: String::new(), tab_keywords: words.clone(),
                card_entry: true, tab_entry: true, target: (0.0, 0.0, 0.0, 0.0),
            };
            out.push(base.clone());
            let mark = items.iter().find(|i| say(i, AccessibleStringProperty::Label) == format!("page {t}") && say(i, AccessibleStringProperty::Id) == "setting-words");
            let Some(page) = mark.and_then(|m| m.parent_item(ParentItemTraversalMode::StopAtPopups)) else { continue };
            let page = &page;
            let origin = page.map_to_window(page.geometry().origin);
            let number = |name: &str| match self.ui.get_property(name) { Ok(SValue::Number(n)) => n as f32, _ => 0.0 };
            let offset = number(&format!("page{t}-offset"));
            let marked: Vec<(String, ItemRc)> = under(page).into_iter().filter(|e| !hidden(e, page))
                .map(|e| (say(&e, AccessibleStringProperty::Id), e)).filter(|(k, _)| k == "setting" || k == "card").collect();
            let rect = |e: &ItemRc| {
                let (g, p) = (e.geometry(), e.map_to_window(e.geometry().origin));
                (p.x - origin.x, p.y - origin.y + offset, g.width(), g.height())
            };
            let sections: Vec<(f32, f32, &String)> = self.section_words[t].iter()
                .map(|(id, words)| (number(&format!("{id}-top")), number(&format!("{id}-tall")), words)).collect();
            let cards: Vec<(String, Rect, String)> = marked.iter().filter(|(k, _)| k == "card")
                .map(|(_, e)| (say(e, AccessibleStringProperty::Label), rect(e), words_of(e, "card-words"))).collect();
            for (kind, e) in &marked {
                let label = say(e, AccessibleStringProperty::Label);
                if label.trim().is_empty() {
                    continue;
                }
                let r = rect(e);
                let is_card = kind == "card";
                let card = cards.iter().rfind(|c| !is_card && r.1 >= c.1.1 && r.1 < c.1.1 + c.1.3);
                let section = sections.iter().find(|s| r.1 >= s.0 && r.1 < s.0 + s.1).map_or("", |s| s.2.as_str());
                out.push(Entry {
                    card: card.map(|c| c.0.clone()).unwrap_or_default(),
                    detail: say(e, AccessibleStringProperty::Description),
                    extra: if is_card { String::new() } else { controls_of(e, &label) },
                    keywords: if is_card { format!("{} {words}", words_of(e, "card-words")) } else { words_of(e, "setting-words") },
                    card_keywords: format!("{} {words}", card.map_or("", |c| c.2.as_str())),
                    tab_keywords: section.to_string(),
                    name: label,
                    card_entry: is_card,
                    tab_entry: false,
                    target: r,
                    ..base.clone()
                });
            }
        }
        out
    }

    pub fn search_results(&self, query: &str) -> Vec<Entry> {
        search_ex(&self.entries(), query, 12).0
    }

    fn on_query(&self, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            if self.leave_search() {
                self.tell_shown();
            }
            return;
        }
        if self.ui.get_property("searching") != Ok(SValue::Bool(true)) {
            *self.entries.borrow_mut() = self.entries();
        }
        let (hits, partial) = search_ex(&self.entries.borrow(), text, 12);
        let rows: Vec<SValue> = hits.iter().map(|e| SValue::Struct(Struct::from_iter([("name".to_string(), s(&e.name)), ("path".to_string(), s(&e.path()))]))).collect();
        let _ = self.ui.set_property("results", model(rows));
        let _ = self.ui.set_property("result", SValue::Number(0.0));
        let _ = self.ui.set_property("results-note", s(&if partial && !hits.is_empty() { tr("Nothing matches every word. These match some of them.") } else { String::new() }));
        let _ = self.ui.set_property("results-empty", s(&if hits.is_empty() {
            fill(&tr("Nothing matches “{text}”. Try fewer words, or say what it does (resolution, microphone, startup)."), &[("text", text)])
        } else { String::new() }));
        let _ = self.ui.set_property("page-title", s(&tr("Search results")));
        let _ = self.ui.set_property("searching", SValue::Bool(true));
        *self.hits.borrow_mut() = hits;
    }

    fn step(&self, delta: i32) {
        let n = self.hits.borrow().len() as i32;
        if n > 0 {
            let at = match self.ui.get_property("result") { Ok(SValue::Number(v)) => v as i32, _ => 0 };
            let _ = self.ui.set_property("result", SValue::Number((at + delta).clamp(0, n - 1) as f64));
        }
    }

    fn open_hit(self: &Rc<Self>, i: usize) {
        let Some(entry) = self.hits.borrow().get(i).cloned() else { return };
        self.go_to(&entry);
    }

    /// Shows the tab the setting is on, scrolled to it and lit up for a moment.
    pub fn go_to(self: &Rc<Self>, entry: &Entry) {
        self.show_tab(&entry.tab);
        if entry.tab_entry {
            return;
        }
        let Some(t) = self.tabs.iter().position(|(id, _)| *id == entry.tab) else { return };
        let (x, y, w, h) = entry.target;
        let _ = self.ui.set_property(&format!("page{t}-offset"), SValue::Number((y - 40.0).max(0.0) as f64));
        let scrolled = match self.ui.get_property(&format!("page{t}-offset")) { Ok(SValue::Number(n)) => n as f32, _ => 0.0 };
        for (k, v) in [("flash-x", 260.0 + x), ("flash-y", 53.0 + y - scrolled), ("flash-w", w), ("flash-h", h)] {
            let _ = self.ui.set_property(k, SValue::Number(v as f64));
        }
        let _ = self.ui.set_property("flashing", SValue::Bool(true));
        let weak = Rc::downgrade(self);
        self.flash.start(slint::TimerMode::SingleShot, std::time::Duration::from_millis(FLASH_MS), move || {
            if let Some(s) = weak.upgrade() {
                let _ = s.ui.set_property("flashing", SValue::Bool(false));
            }
        });
    }

    /// The Plugins tab's tiles as the search and filter leave them.
    pub fn show_panel(&self) {
        let query = match self.ui.get_property("plugin-query") { Ok(SValue::String(q)) => q.to_string(), _ => String::new() };
        let filter = match self.ui.get_property("plugin-filter") { Ok(SValue::Number(1.0)) => Filter::On, Ok(SValue::Number(2.0)) => Filter::Off, _ => Filter::All };
        let panel = self.panel.borrow();
        let own = self.window.upgrade().is_none_or(|w| w.ui.get_global_property("Theme", "own-hues").map_or(true, |v| v == SValue::Bool(true)));
        let link = |l: &Option<plugins_tab::Link>| l.as_ref().map(|l| (l.text.clone(), l.url.clone())).unwrap_or_default();
        let groups: Vec<SValue> = panel.shown(&query, filter).into_iter().map(|(g, count)| {
            let tall = g.tiles.iter().map(tile_extra).max().unwrap_or(0);
            let tiles: Vec<SValue> = g.tiles.iter().map(|t| {
                let badges: Vec<SValue> = t.badges.iter().map(|b| SValue::Struct(Struct::from_iter([("text".to_string(), s(&b.text)), ("kind".to_string(), s(&b.kind))]))).collect();
                let (note_link, note_url) = link(&t.note_link);
                let (byline_link, byline_url) = link(&t.byline_link);
                SValue::Struct(Struct::from_iter([
                    ("name".to_string(), s(&t.name)), ("title".to_string(), s(&t.title)), ("about".to_string(), s(&t.about)),
                    ("icon".to_string(), SValue::Image(icon(&t.icon))), ("hue".to_string(), SValue::Brush(slint::Brush::SolidColor(hue(&t.group, own)))),
                    ("on".to_string(), SValue::Bool(t.on)), ("locked".to_string(), SValue::Bool(t.locked)), ("enabled".to_string(), SValue::Bool(t.enabled)),
                    ("tooltip".to_string(), s(&t.tooltip)), ("note".to_string(), s(&t.note)), ("note-kind".to_string(), s(&t.note_kind)),
                    ("note-link".to_string(), s(&note_link)), ("note-url".to_string(), s(&note_url)),
                    ("byline".to_string(), s(&t.byline)), ("byline-link".to_string(), s(&byline_link)), ("byline-url".to_string(), s(&byline_url)),
                    ("removable".to_string(), SValue::Bool(t.added)),
                    ("remove-label".to_string(), s(&if t.added { fill(&tr("Remove {plugin}"), &[("plugin", &t.title)]) } else { String::new() })),
                    ("badges".to_string(), model(badges)),
                ]))
            }).collect();
            SValue::Struct(Struct::from_iter([
                ("id".to_string(), s(&g.id)), ("title".to_string(), s(&g.title)), ("count".to_string(), s(&count)),
                ("icon".to_string(), SValue::Image(icon(group_icon(&g.id)))), ("hue".to_string(), SValue::Brush(slint::Brush::SolidColor(hue(&g.id, own)))),
                ("tiles".to_string(), model(tiles)), ("tile-height".to_string(), SValue::Number((94 + tall) as f64)),
                ("remove-pack".to_string(), SValue::Bool(g.remove_pack)), ("status".to_string(), s(&g.status)),
            ]))
        }).collect();
        let _ = self.ui.set_property("groups", model(groups));
        let _ = self.ui.set_property("narrowed", SValue::Bool(!query.trim().is_empty() || filter != Filter::All));
        let _ = self.ui.set_property("empty-note", s(&Panel::empty_note(&query, filter)));
        let _ = self.ui.set_property("all-on-enabled", SValue::Bool(panel.all_on_enabled));
        let _ = self.ui.set_property("footer-note", s(&panel.footer_note()));
        let _ = self.ui.set_property("footer-shown", SValue::Bool(!panel.pending.is_empty()));
        let _ = self.ui.set_property("has-added", SValue::Bool(!panel.installed.is_empty()));
        let rows: Vec<SValue> = panel.update_rows.iter().flat_map(|u| [
            update_row("check", tr("Check for updates"),
                       tr("Looks once a day for new versions of added plugins that publish releases on GitHub and mentions them here. Nothing gets downloaded."), "", false, u.check_on, true),
            update_row("auto", tr("Install updates without asking"), tr("Installs new versions of the plugins you switched on and switches them on again."),
                       &tr("Dangerous! Whoever controls a plugin's repo can then run new code on this computer every time they publish a release."), true, u.auto_on, u.auto_enabled),
        ]).collect();
        let _ = self.ui.set_property("update-rows", model(rows));
        let names: Vec<SValue> = ["All", "On", "Off"].iter().map(|w| s(&fill(&tr("Show: {which}"), &[("which", &telescope_core::i18n::tr_ctx("plugins", w))]))).collect();
        let _ = self.ui.set_property("filter-names", model(names));
        let _ = self.plugins_tab;
    }

    /// Asks for a change: a question goes on screen first when the change needs one.
    fn request(&self, request: Request) {
        let step = self.panel.borrow_mut().request(request);
        self.handle(step);
    }

    fn handle(&self, step: Step) {
        match step {
            Step::Done(outcome) => self.finish(outcome),
            Step::Ask(q, commit) => {
                self.show_panel();
                self.ask(&q, commit);
            }
        }
    }

    fn ask(&self, q: &Question, commit: Commit) {
        let _ = self.ui.set_property("ask-title", s(&q.title));
        let _ = self.ui.set_property("ask-text", s(&q.text));
        let _ = self.ui.set_property("ask-yes", s(&q.yes));
        let _ = self.ui.set_property("ask-danger", SValue::Bool(q.danger));
        let _ = self.ui.set_property("ask-info", SValue::Bool(false));
        let _ = self.ui.set_property("asking", SValue::Bool(true));
        *self.asking.borrow_mut() = Some(commit);
    }

    /// Shows the tiles as they are now, then what the person is told, then restarts if that's what they asked for.
    fn finish(&self, outcome: Outcome) {
        self.show_panel();
        self.notices.borrow_mut().extend(outcome.notices);
        self.next_notice();
        if outcome.restart {
            if let Some(w) = self.window.upgrade() {
                if !w.restart_app() {
                    self.notices.borrow_mut().push_back(crate::plugins_tab::restart_failed());
                    self.next_notice();
                }
            }
        }
    }

    /// The next message waits for the question or message before it to be dismissed.
    fn next_notice(&self) {
        if self.telling.get() || self.asking.borrow().is_some() {
            return;
        }
        let Some(n) = self.notices.borrow_mut().pop_front() else { return };
        let _ = self.ui.set_property("ask-title", s(&n.title));
        let _ = self.ui.set_property("ask-text", s(&n.text));
        let _ = self.ui.set_property("ask-yes", s(&tr("OK")));
        let _ = self.ui.set_property("ask-danger", SValue::Bool(n.kind == NoticeKind::Warning));
        let _ = self.ui.set_property("ask-info", SValue::Bool(true));
        let _ = self.ui.set_property("asking", SValue::Bool(true));
        self.telling.set(true);
    }

    fn answered(&self, yes: bool) {
        let _ = self.ui.set_property("asking", SValue::Bool(false));
        let commit = self.asking.borrow_mut().take();
        match commit {
            Some(commit) if yes => {
                let outcome = self.panel.borrow_mut().commit(commit);
                self.finish(outcome);
            }
            Some(_) => {
                // Cancelled: the switch goes back as it was
                self.panel.borrow_mut().refresh();
                self.show_panel();
                self.next_notice();
            }
            None => {
                self.telling.set(false);
                self.next_notice();
            }
        }
    }

    /// The Install button: pick a .zip (the file chooser runs on its own thread, so the window keeps going).
    fn pick_zip(&self) {
        use telescope_core::host::{FileDialog, FileDialogKind};
        let dialog = FileDialog {
            kind: FileDialogKind::Open,
            title: tr("Install a plugin"),
            start: Panel::start_folder().to_string_lossy().into_owned(),
            filters: vec![tr("Plugins (*.zip)")],
        };
        std::thread::spawn(move || {
            let (ok, _, data) = crate::file_dialog::ask(&dialog);
            let path = data.get("path").and_then(|p| p.as_str()).map(String::from).filter(|_| ok);
            if let Some(path) = path {
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(settings) = current() {
                        settings.install(path.into());
                    }
                });
            }
        });
    }

    /// Installs a .zip the person picked or dropped.
    pub fn install(&self, path: std::path::PathBuf) {
        self.request(Request::Install { path });
    }

    fn open_plugins_folder(&self) {
        let made = self.panel.borrow().make_plugins_dir();
        match made {
            Ok(folder) => open_url(&folder.to_string_lossy()),
            Err(notice) => {
                self.notices.borrow_mut().push_back(notice);
                self.next_notice();
            }
        }
    }

    /// Polls the vision download and the update check while the window lives, and shows what they've come to.
    fn start_watching(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.watch.start(slint::TimerMode::Repeated, std::time::Duration::from_millis(600), move || {
            let Some(s) = weak.upgrade() else { return };
            poll_updates(s.panel.borrow().admin.as_ref());
            let sign = format!("{} {}", s.panel.borrow().download_sign(), CHECKED.with(|c| c.get()));
            if *s.seen.borrow() != sign {
                *s.seen.borrow_mut() = sign;
                s.panel.borrow_mut().refresh();
                s.show_panel();
            }
        });
    }
}

/// How much taller than the plain tile this one has to be: its byline, its note, its link and its badges.
fn tile_extra(t: &plugins_tab::Tile) -> i32 {
    let note = if t.note.is_empty() { 0 } else { 6 + 16 * (t.note.chars().count() as i32 / 44 + 1) };
    (if t.badges.is_empty() { 0 } else { 24 }) + (if t.byline.is_empty() && t.byline_link.is_none() { 0 } else { 22 }) + note
        + (if t.note_link.is_some() { 22 } else { 0 })
}

fn update_row(id: &str, title: String, about: String, note: &str, danger: bool, on: bool, enabled: bool) -> SValue {
    SValue::Struct(Struct::from_iter([
        ("id".to_string(), s(id)), ("title".to_string(), s(&title)), ("about".to_string(), s(&about)), ("note".to_string(), s(note)),
        ("danger".to_string(), SValue::Bool(danger)), ("on".to_string(), SValue::Bool(on)), ("enabled".to_string(), SValue::Bool(enabled)),
    ]))
}

fn s_empty() -> SValue {
    SValue::String(Default::default())
}

fn open_url(url: &str) {
    telescope_core::launch::open_url(url);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use telescope_core::ui::UiSpec;

    fn section(id: &str, tab: &str, order: i32) -> (String, SettingsSection) {
        ("p".into(), SettingsSection {
            id: id.into(), tab: tab.into(), order, tab_title: String::new(), keywords: String::new(), footer: None,
            ui: UiSpec { source: SlintSource::Text(String::new()), component: "X".into(), props: json!({}) },
        })
    }

    #[test]
    fn only_tabs_with_something_in_them_show_in_the_fixed_order() {
        // Python's test window has no Plugins tab because the app is what adds it; here ordered_tabs does
        let tabs = ordered_tabs(&[section("a", "about", 0), section("b", "general", 0), section("c", "video", 0)], "");
        let ids: Vec<&str> = tabs.iter().map(|t| t.0.as_str()).collect();
        assert_eq!(ids, ["general", "video", "plugins", "about"]);
        assert_eq!(tabs[3].1, "Updates & About");
    }

    #[test]
    fn a_tab_a_plugin_makes_goes_before_plugins() {
        let mut mine = section("c", "mine", 0);
        mine.1.tab_title = "My thing".into();
        let tabs = ordered_tabs(&[section("a", "shortcuts", 0), mine, section("b", "general", 0)], "");
        let ids: Vec<&str> = tabs.iter().map(|t| t.0.as_str()).collect();
        assert_eq!(ids, ["general", "shortcuts", "mine", "plugins"]);
        assert_eq!(tabs[2].1, "My thing");
        let tabs = ordered_tabs(&[section("a", "on_air", 0), section("b", "about", 0)], "");
        let ids: Vec<&str> = tabs.iter().map(|t| t.0.as_str()).collect();
        assert_eq!(ids, ["on_air", "plugins", "about"]);
        assert_eq!(tabs[0].1, "On air");
    }

    #[test]
    fn a_tab_title_replaces_the_usual_name() {
        let mut s = section("a", "general", 0);
        s.1.tab_title = "Mine".into();
        assert_eq!(ordered_tabs(&[s], "")[0].1, "Mine");
        assert_eq!(ordered_tabs(&[], "Plugins (3 off)")[0].1, "Plugins (3 off)");
    }

    #[test]
    fn sections_sit_in_order_then_registration_order() {
        let tabs = ordered_tabs(&[section("late", "general", 5), section("first", "general", 0), section("second", "general", 0)], "");
        assert_eq!(tabs[0].2, vec![1, 2, 0]);
    }
}
