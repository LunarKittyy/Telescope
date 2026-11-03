//! The Plugins tab of Settings (widgets/plugins_panel.py) as data: every plugin, built in or added, as a tile with its switch, grouped by
//! what it's for, with the search and the All / On / Off filter, the Added plugins box, the update switches and the vision download.
//!
//! Nothing here draws. A change is asked for with `request`, which says whether the person has to answer a question first; `commit` then
//! does it and says what to tell them. `run` does both for the tests, which answer the questions themselves like the Python ones.

use telescope_core::i18n::Fields;
use crate::texts::{fill, trn};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use telescope_core::i18n::{tr, tr_ctx};
use telescope_core::plugin::{PluginInfo, PLUGIN_CATEGORIES};
use telescope_core::vision::VisionService;
use telescope_core::plugin_admin::{AddedPlugin, InstallFailure, PluginAdmin, AUTO_UPDATE, CHECK_UPDATES};

pub const DOCS_URL: &str = "https://github.com/LunarKittyy/Telescope/blob/master/docs/plugins.md";

/// Each group's icon on its heading (CATEGORY_LOOK); its colour is the group's hue.
pub fn group_icon(group: &str) -> &'static str {
    match group {
        "camera" => "camera",
        "look" => "colour",
        "vision" => "skeleton",
        "preview" => "eye",
        "on_screen" => "image",
        "sound" => "mic",
        "streaming" => "v_stream",
        _ => "plug",
    }
}

/// The words the settings search finds the Plugins tab and its groups by.
pub fn search_keywords() -> String {
    tr_ctx("search", "plugins extensions addons add ons modules features turn off on disable enable install zip")
}

/// The narrowest a tile gets before the grid drops to fewer columns.
pub const TILE_MIN: f32 = 230.0;
pub const COLUMNS_MAX: usize = 3;
pub const TILE_GAP: f32 = 10.0;

/// _TileGrid.columns: three when there's room, else two, one only when very narrow.
pub fn columns(width: f32) -> usize {
    let fit = ((width + TILE_GAP) / (TILE_MIN + TILE_GAP)).floor() as usize;
    let least = if width < TILE_MIN * 2.0 - 60.0 { 1 } else { 2 };
    least.max(COLUMNS_MAX.min(fit))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filter {
    All,
    On,
    Off,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Badge {
    pub text: String,
    /// "lock", "restart", "vision" or "added".
    pub kind: String,
}

/// A link in a tile's text: what it says and where it goes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Link {
    pub text: String,
    pub url: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Tile {
    pub name: String,
    pub title: String,
    pub about: String,
    pub icon: String,
    pub group: String,
    pub on: bool,
    pub locked: bool,
    /// The switch can be used (a locked one can't, nor one with no download to wait for).
    pub enabled: bool,
    pub tooltip: String,
    pub note: String,
    pub note_kind: String,
    /// A link that goes with the note (a newer release).
    pub note_link: Option<Link>,
    /// "By {author}." and the repo as a link, for an added plugin.
    pub byline: String,
    pub byline_link: Option<Link>,
    pub badges: Vec<Badge>,
    pub optional: bool,
    pub when_off: String,
    /// An added plugin: it has a remove button.
    pub added: bool,
    /// What the search looks through: name, description and for an added one its author.
    pub haystack: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    pub id: String,
    pub title: String,
    pub tiles: Vec<Tile>,
    /// The vision download's own button, on the group the plugins that use it are in.
    pub remove_pack: bool,
    pub status: String,
}

/// category_of: a plugin's group, Extras for one that names none of them.
pub fn category_of(value: &str) -> &str {
    if PLUGIN_CATEGORIES.iter().any(|(id, _)| *id == value) { value } else { "extras" }
}

/// What a question box asks (_ask): its title, the text, the button that agrees and whether it's a warning.
#[derive(Clone, Debug, PartialEq)]
pub struct Question {
    pub title: String,
    pub text: String,
    pub yes: String,
    pub danger: bool,
}

fn question(title: String, text: String, yes: String, danger: bool) -> Question {
    Question { title, text, yes, danger }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeKind {
    Info,
    Warning,
}

/// A message box with nothing to decide.
#[derive(Clone, Debug, PartialEq)]
pub struct Notice {
    pub kind: NoticeKind,
    pub title: String,
    pub text: String,
}

fn warning(title: &str, text: String) -> Notice {
    Notice { kind: NoticeKind::Warning, title: tr(title), text }
}

/// A new copy wouldn't start, so this one carries on.
pub fn restart_failed() -> Notice {
    warning("Couldn't restart", tr("Telescope couldn't start a new copy of itself. Close it and open it again to apply your changes."))
}

fn info(title: &str, text: String) -> Notice {
    Notice { kind: NoticeKind::Info, title: tr(title), text }
}

/// What the tab was asked to change.
#[derive(Clone, Debug, PartialEq)]
pub enum Request {
    /// A tile's switch (built in, optional or added).
    Switch { name: String, on: bool },
    AllOn,
    Remove { name: String },
    RemovePack,
    Install { path: PathBuf },
    CheckUpdates(bool),
    AutoUpdate(bool),
    /// Restart now; streaming is whether a stream is running.
    Restart { streaming: bool },
}

/// What happens once any question is agreed to.
#[derive(Clone, Debug, PartialEq)]
pub enum Commit {
    Builtin { name: String, on: bool },
    Optional { name: String, on: bool },
    DownloadThenOn { name: String },
    Added { name: String, on: bool },
    AllOn,
    Remove { name: String },
    RemovePack,
    Install { path: PathBuf, replace: bool },
    Updates { key: &'static str, on: bool },
    Restart,
}

/// What a request did: what to tell the person, and whether Telescope is to start again.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Outcome {
    pub notices: Vec<Notice>,
    pub restart: bool,
}

pub enum Step {
    Done(Outcome),
    Ask(Question, Commit),
}

/// The vision pack's download as the tab sees it (VisionDownload).
pub trait Download {
    fn ready(&self) -> bool;
    fn available(&self) -> bool;
    fn running(&self) -> bool;
    fn size_mb(&self) -> u64;
    /// The optional plugin that gets switched on once it's done.
    fn waiting(&self) -> String;
    /// What went wrong last time, for the plugin that was waiting.
    fn error(&self) -> String;
    fn failed_for(&self) -> String;
    /// Bytes so far and in all (0: not known).
    fn progress(&self) -> (u64, u64);
    fn start(&self, switch_on: &str);
    fn remove(&self) -> Result<(), String>;
}

/// The app's one download, from the vision service.
pub struct VisionDownload {
    service: Arc<dyn VisionService>,
    waiting: Mutex<String>,
}

impl VisionDownload {
    pub fn new(service: Arc<dyn VisionService>) -> VisionDownload {
        VisionDownload { service, waiting: Mutex::new(String::new()) }
    }
}

impl Download for VisionDownload {
    fn ready(&self) -> bool {
        self.service.pack().ready
    }
    fn available(&self) -> bool {
        self.service.pack().available
    }
    fn running(&self) -> bool {
        self.service.pack().downloading
    }
    fn size_mb(&self) -> u64 {
        self.service.pack().size_mb.max(1)
    }
    fn waiting(&self) -> String {
        if self.running() { self.waiting.lock().unwrap_or_else(|e| e.into_inner()).clone() } else { String::new() }
    }
    fn error(&self) -> String {
        self.service.pack().error
    }
    fn failed_for(&self) -> String {
        self.service.pack().failed_for
    }
    fn progress(&self) -> (u64, u64) {
        let pack = self.service.pack();
        (pack.done, pack.size)
    }
    fn start(&self, switch_on: &str) {
        if !switch_on.is_empty() {
            *self.waiting.lock().unwrap_or_else(|e| e.into_inner()) = switch_on.to_string();
        }
        self.service.download(switch_on);
    }
    fn remove(&self) -> Result<(), String> {
        self.service.remove_pack()
    }
}

/// No vision download in this build: nothing to fetch and nothing there.
pub struct NoVision;

impl Download for NoVision {
    fn ready(&self) -> bool { false }
    fn available(&self) -> bool { false }
    fn running(&self) -> bool { false }
    fn size_mb(&self) -> u64 { 1 }
    fn waiting(&self) -> String { String::new() }
    fn error(&self) -> String { String::new() }
    fn failed_for(&self) -> String { String::new() }
    fn progress(&self) -> (u64, u64) { (0, 0) }
    fn start(&self, _: &str) {}
    fn remove(&self) -> Result<(), String> {
        Ok(())
    }
}

/// The Check for updates and Install updates without asking switches under the Added plugins.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UpdateRows {
    pub check_on: bool,
    pub auto_on: bool,
    /// The red switch can't go on without the check.
    pub auto_enabled: bool,
}

pub struct Panel {
    pub builtins: Vec<PluginInfo>,
    reserved: BTreeSet<String>,
    /// The names of the plugins running now.
    running: BTreeSet<String>,
    /// Why each added plugin that's on didn't start this time.
    problems: BTreeMap<String, String>,
    pub admin: Arc<dyn PluginAdmin>,
    download: Box<dyn Download>,
    pub installed: Vec<AddedPlugin>,
    pub groups: Vec<Group>,
    pub pending: BTreeSet<String>,
    pub all_on_enabled: bool,
    /// Under the groups: the update switches, when something added can be updated.
    pub update_rows: Option<UpdateRows>,
    vision_group: String,
}

/// The text with its <a href> tags taken out, and the first link in it.
pub fn split_link(text: &str) -> (String, Option<Link>) {
    let Some(open) = text.find("<a href=\"") else { return (text.to_string(), None) };
    let rest = &text[open + 9..];
    let Some(quote) = rest.find("\">") else { return (text.to_string(), None) };
    let url = rest[..quote].to_string();
    let words = &rest[quote + 2..];
    let Some(end) = words.find("</a>") else { return (text.to_string(), None) };
    let link = Link { text: words[..end].to_string(), url };
    (format!("{}{}{}", &text[..open], link.text, &words[end + 4..]), Some(link))
}

impl Panel {
    pub fn new(builtins: Vec<PluginInfo>, running: BTreeSet<String>, problems: BTreeMap<String, String>, admin: Arc<dyn PluginAdmin>,
               download: Box<dyn Download>) -> Panel {
        let reserved = builtins.iter().map(|p| p.name.clone()).collect();
        let mut panel = Panel {
            builtins, reserved, running, problems, admin, download, installed: vec![], groups: vec![], pending: BTreeSet::new(),
            all_on_enabled: false, update_rows: None, vision_group: String::new(),
        };
        panel.refresh();
        panel
    }

    /// Changes whenever the download moves on, so the window knows when to show the tiles again.
    pub fn download_sign(&self) -> String {
        let dl = &self.download;
        let (done, size) = dl.progress();
        format!("{} {} {} {} {}", dl.ready(), dl.running(), (done * 100).checked_div(size).unwrap_or(0), dl.error(), dl.failed_for())
    }

    /// How many switches, installs and removals wait for Telescope to start again.
    pub fn pending_changes(&self) -> usize {
        self.pending.len()
    }

    /// Rebuilds the tiles from the switches as saved now.
    pub fn refresh(&mut self) {
        let (off, on) = (self.admin.builtin_off(), self.admin.optional_on());
        self.pending.clear();
        self.installed = self.admin.find_installed();
        self.all_on_enabled = self.builtins.iter().any(|p| !p.required && if p.optional { on.contains(&p.name) } else { off.contains(&p.name) });
        let mut tiles: Vec<(String, Vec<Tile>)> = PLUGIN_CATEGORIES.iter().map(|(id, _)| (id.to_string(), vec![])).collect();
        let add = |tiles: &mut Vec<(String, Vec<Tile>)>, tile: Tile| {
            if let Some((_, list)) = tiles.iter_mut().find(|(id, _)| *id == tile.group) {
                list.push(tile);
            }
        };
        for p in &self.builtins.clone() {
            let tile = if p.optional { self.optional_tile(p, on.contains(&p.name)) } else { self.builtin_tile(p, p.required || !off.contains(&p.name)) };
            add(&mut tiles, tile);
        }
        for item in &self.installed.clone() {
            let tile = if item.dev { self.dev_tile(item) } else { self.added_tile(item) };
            add(&mut tiles, tile);
        }
        let vision: Vec<&str> = self.builtins.iter().filter(|p| p.optional && p.needs_vision).map(|p| category_of(&p.category)).collect();
        self.vision_group = if vision.contains(&"vision") { "vision".into() } else { vision.first().map(|s| s.to_string()).unwrap_or_default() };
        let remove_pack = self.remove_pack_shown(&on);
        let status = self.vision_status();
        self.groups = PLUGIN_CATEGORIES.iter().filter_map(|(id, name)| {
            let mut list = tiles.iter().find(|(g, _)| g == id).map(|(_, t)| t.clone()).unwrap_or_default();
            list.sort_by_key(|t| t.title.to_lowercase());
            let mine = *id == self.vision_group;
            (!list.is_empty()).then(|| Group { id: id.to_string(), title: tr(name), tiles: list, remove_pack: mine && remove_pack, status: if mine { status.clone() } else { String::new() } })
        }).collect();
        self.update_rows = self.installed.iter().any(|i| !i.dev).then(|| {
            let check_on = self.admin.switch(CHECK_UPDATES, true);
            let auto_on = check_on && self.admin.switch(AUTO_UPDATE, false);
            UpdateRows { check_on, auto_on, auto_enabled: check_on || auto_on }
        });
        let there: BTreeSet<&str> = self.installed.iter().map(|i| i.name.as_str()).collect();
        let removed: Vec<String> = self.running.iter().filter(|n| !self.reserved.contains(*n) && !there.contains(n.as_str())).cloned().collect();
        self.pending.extend(removed);
    }

    fn base(&self, title: String, about: String, icon: &str, group: &str, on: bool) -> Tile {
        Tile {
            name: String::new(), haystack: format!("{title} {about}").to_lowercase(), title, about, icon: icon.into(), group: group.into(), on,
            locked: false, enabled: true, tooltip: String::new(), note: String::new(), note_kind: "dim".into(), note_link: None,
            byline: String::new(), byline_link: None, badges: vec![], optional: false, when_off: String::new(), added: false,
        }
    }

    fn of_builtin(&self, p: &PluginInfo, on: bool) -> Tile {
        let mut tile = self.base(tr(if p.title.is_empty() { &p.name } else { &p.title }), tr(&p.about), &p.icon, category_of(&p.category), on);
        tile.name = p.name.clone();
        tile.optional = p.optional;
        tile.when_off = if p.when_off.is_empty() { String::new() } else { tr(&p.when_off) };
        tile
    }

    /// _tile's pending: the plugin waits on a restart.
    fn waits(&mut self, tile: &mut Tile) {
        self.pending.insert(tile.name.clone());
        tile.badges.push(Badge { text: tr("Restart required"), kind: "restart".into() });
    }

    fn builtin_tile(&mut self, p: &PluginInfo, on: bool) -> Tile {
        let mut tile = self.of_builtin(p, on);
        if p.required {
            tile.on = true;
            tile.locked = true;
            tile.enabled = false;
            tile.tooltip = tr("Telescope can't work without this one, so it can't be switched off");
            tile.badges.push(Badge { text: tr("Required"), kind: "lock".into() });
            return tile;
        }
        if on && !tile.when_off.is_empty() {
            tile.tooltip = tile.when_off.clone();
        }
        if on != self.running.contains(&p.name) {
            self.waits(&mut tile);
        }
        tile
    }

    /// An optional built-in plugin: off until switched on, which first downloads what it needs.
    fn optional_tile(&mut self, p: &PluginInfo, on: bool) -> Tile {
        let mut tile = self.of_builtin(p, on);
        let dl = &self.download;
        let needs = p.needs_vision && !dl.ready();
        let downloading = p.needs_vision && dl.running() && dl.waiting() == p.name;
        let (mut note, mut kind) = (String::new(), "dim");
        if downloading {
            let (done, size) = dl.progress();
            note = (done * 100).checked_div(size).map_or_else(|| tr("Downloading…"), |percent| fill(&tr("Downloading… {percent}%"), &[("percent", &percent.min(99).to_string())]));
            kind = "status_warn";
        } else if dl.failed_for() == p.name && !dl.error().is_empty() {
            note = fill(&tr("The download didn't work. {error}"), &[("error", &dl.error())]);
            kind = "status_err";
        } else if needs && !dl.available() {
            note = tr("There's no download of what it needs for this computer yet, so it can't be switched on.");
            kind = "status_err";
        } else if on && needs {
            note = fill(&tr("Its download (about {mb} MB) isn't there anymore. Switch it off and on again to get it."), &[("mb", &dl.size_mb().to_string())]);
            kind = "status_warn";
        }
        tile.enabled = !(needs && !dl.available()) && !(dl.running() && dl.waiting() == p.name);
        tile.note = note;
        tile.note_kind = kind.into();
        if p.needs_vision {
            tile.badges.push(Badge { text: tr("Needs the vision download"), kind: "vision".into() });
        }
        if on != self.running.contains(&p.name) && !needs {
            self.waits(&mut tile);
        }
        tile
    }

    fn remove_pack_shown(&self, on: &BTreeSet<String>) -> bool {
        let dl = &self.download;
        dl.ready() && !dl.running() && !self.builtins.iter().any(|p| p.optional && p.needs_vision && (on.contains(&p.name) || self.running.contains(&p.name)))
    }

    fn vision_status(&self) -> String {
        let dl = &self.download;
        if dl.ready() || dl.running() {
            String::new()
        } else if !dl.available() {
            tr("There's no vision download for this computer yet.")
        } else {
            fill(&tr("Switching one of these on downloads about {mb} MB first, once for all of them."), &[("mb", &dl.size_mb().to_string())])
        }
    }

    fn added_tile(&mut self, item: &AddedPlugin) -> Tile {
        let title = format!("{} {}", item.title, item.version).trim().to_string();
        let mut tile = self.base(title, item.description.clone(), "plug", category_of(&item.category), item.enabled);
        tile.name = item.name.clone();
        tile.added = true;
        tile.haystack = format!("{} {} {}", tile.title, tile.about, item.author).to_lowercase();
        if !item.author.is_empty() {
            tile.byline = fill(&tr("By {author}."), &[("author", &item.author)]);
        }
        if !item.repo.is_empty() {
            // The address without https://, as a link
            let shown = item.repo.split_once("://").map(|(_, rest)| rest).unwrap_or(&item.repo).trim_end_matches('/').to_string();
            tile.byline_link = Some(Link { text: shown, url: item.repo.clone() });
        }
        let release = if self.admin.switch(CHECK_UPDATES, true) { self.admin.newer_release(&item.name) } else { None };
        let (mut pending, mut note, mut kind) = (false, String::new(), "dim");
        if let (Some(version), true, false) = (self.admin.auto_updated(&item.name), item.enabled, item.changed) {
            note = fill(&tr("Updated to {version} automatically. The new version runs after Telescope restarts."), &[("version", &version)]);
            pending = true;
        } else if !item.problem.is_empty() {
            note = item.problem.clone();
            kind = "status_err";
        } else if item.changed {
            note = tr("Its files changed since you switched it on, so it's off for now. Switch it on again if you trust this version.");
            kind = "status_warn";
        } else if item.enabled && self.problems.contains_key(&item.name) {
            note = fill(&tr("It didn't start ({problem})."), &[("problem", self.problems[&item.name].trim_end_matches('.'))]);
            kind = "status_err";
            pending = true; // a restart tries it again
        } else if item.enabled != self.running.contains(&item.name) {
            pending = true;
        }
        if note.is_empty() {
            if let Some(release) = release {
                // Translators: keep the <a href="{url}"> link
                let text = fill(&tr("Version {version} is out. <a href=\"{url}\">Download it from its release page</a> and install it here to replace this one."),
                                &[("version", &release.tag), ("url", &release.page)]);
                let (plain, link) = split_link(&text);
                note = plain;
                tile.note_link = link;
                kind = "status_warn";
            }
        }
        tile.note = note;
        tile.note_kind = kind.into();
        tile.badges.push(Badge { text: tr("Added"), kind: "added".into() });
        // One that's on can always be switched off
        tile.enabled = item.problem.is_empty() || item.enabled;
        if pending {
            self.waits(&mut tile);
        }
        tile
    }

    /// A plugin telescope-rs dev --plugin loads from where it's written: always on, nothing to switch or remove.
    fn dev_tile(&self, item: &AddedPlugin) -> Tile {
        let title = format!("{} {}", item.title, item.version).trim().to_string();
        let mut tile = self.base(title, item.description.clone(), "plug", category_of(&item.category), true);
        tile.name = item.name.clone();
        tile.locked = true;
        tile.enabled = false;
        tile.added = false;
        tile.tooltip = tr("telescope-rs dev --plugin loads this one, so it's always on");
        tile.badges.push(Badge { text: tr("Being written"), kind: "added".into() });
        if !item.problem.is_empty() || self.problems.contains_key(&item.name) {
            let why = if item.problem.is_empty() { &self.problems[&item.name] } else { &item.problem };
            tile.note = fill(&tr("It didn't start ({problem})."), &[("problem", why.trim_end_matches('.'))]);
            tile.note_kind = "status_err".into();
        } else {
            tile.note = fill(&tr("Loaded from {folder} by telescope-rs dev. Telescope restarts by itself when you save one of its files."), &[("folder", &item.folder.to_string_lossy())]);
        }
        tile
    }

    /// The tiles the search and filter leave, group by group (empty groups dropped), each with its "n of m on".
    pub fn shown(&self, query: &str, filter: Filter) -> Vec<(Group, String)> {
        let words: Vec<String> = query.to_lowercase().split_whitespace().map(String::from).collect();
        self.groups.iter().filter_map(|g| {
            let tiles: Vec<Tile> = g.tiles.iter().filter(|t| {
                words.iter().all(|w| t.haystack.contains(w)) && (filter == Filter::All || t.on == (filter == Filter::On))
            }).cloned().collect();
            let on = g.tiles.iter().filter(|t| t.on).count();
            // Translators: next to a group's name on the Plugins tab, how many of its plugins are switched on
            let count = fill(&tr("{on} of {total} on"), &[("on", &on.to_string()), ("total", &g.tiles.len().to_string())]);
            (!tiles.is_empty()).then(|| (Group { tiles, ..g.clone() }, count))
        }).collect()
    }

    /// The names of the tiles the search and the filter leave, group by group, in order.
    pub fn shown_tiles(&self, query: &str, filter: Filter) -> Vec<String> {
        self.shown(query, filter).into_iter().flat_map(|(g, _)| g.tiles.into_iter().map(|t| t.title)).collect()
    }

    /// What the empty list says.
    pub fn empty_note(query: &str, filter: Filter) -> String {
        if !query.trim().is_empty() {
            fill(&tr("No plugins match “{text}”."), &[("text", query.trim())])
        } else if filter == Filter::Off {
            tr("Every plugin is switched on.")
        } else {
            tr("No plugin is switched on.")
        }
    }

    pub fn footer_note(&self) -> String {
        trn("Restart to apply {n} change", "Restart to apply {n} changes", self.pending.len())
    }

    /// The tab's name, with how many built-in plugins are off this run.
    pub fn tab_title(off: usize) -> String {
        if off == 0 { String::new() } else { tr("Plugins ({n} off)").field("n", off.to_string()) }
    }

    fn tile(&self, name: &str) -> Option<Tile> {
        self.groups.iter().flat_map(|g| g.tiles.iter()).find(|t| t.name == name).cloned()
    }

    fn item(&self, name: &str) -> Option<AddedPlugin> {
        self.installed.iter().find(|i| i.name == name).cloned()
    }

    // ── Changing things ───────────────────────────────────────────────────────

    /// What a change asks first, or does it at once when there's nothing to ask.
    pub fn request(&mut self, request: Request) -> Step {
        match request {
            Request::Switch { name, on } => self.switch(&name, on),
            Request::AllOn => Step::Done(self.commit(Commit::AllOn)),
            Request::Remove { name } => {
                let Some(item) = self.item(&name) else { return Step::Done(Outcome::default()) };
                let folder = item.folder.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                Step::Ask(question(
                    fill(&tr("Remove {plugin}?"), &[("plugin", &item.title)]),
                    fill(&tr("Its folder ({folder}) is deleted from the plugins folder. To use it again you'll have to add it again."), &[("folder", &folder)]),
                    tr("Remove"), true), Commit::Remove { name })
            }
            Request::RemovePack => Step::Ask(question(
                tr("Remove the download?"),
                fill(&tr("It frees about {mb} MB. Switching on one of the optional plugins downloads it again."), &[("mb", &self.download.size_mb().to_string())]),
                tr("Remove"), false), Commit::RemovePack),
            Request::Install { path } => self.install(path, false),
            Request::CheckUpdates(on) => Step::Done(self.commit(Commit::Updates { key: CHECK_UPDATES, on })),
            Request::AutoUpdate(on) => {
                if !on {
                    return Step::Done(self.commit(Commit::Updates { key: AUTO_UPDATE, on }));
                }
                Step::Ask(question(
                    tr("Install plugin updates without asking?"),
                    tr("Telescope will install new versions of the plugins you switched on and switch them on too, without showing them to you first.\n\nWhoever controls a plugin's repo on GitHub (its author, or anyone who gets into their account) can then run new code on this computer every time they publish a release. Only switch this on if you trust all of them with that."),
                    tr("Switch on"), true), Commit::Updates { key: AUTO_UPDATE, on })
            }
            Request::Restart { streaming } => {
                if streaming {
                    Step::Ask(question(tr("Restart Telescope now?"), tr("Streaming stops while Telescope restarts. It takes a few seconds."), tr("Restart"), false), Commit::Restart)
                } else {
                    Step::Done(self.commit(Commit::Restart))
                }
            }
        }
    }

    fn switch(&mut self, name: &str, on: bool) -> Step {
        let Some(tile) = self.tile(name) else { return Step::Done(Outcome::default()) };
        if tile.added {
            let item = self.item(name);
            let title = item.map(|i| i.title).unwrap_or_else(|| tile.title.clone());
            return Step::Ask(if on {
                question(fill(&tr("Switch on {plugin}?"), &[("plugin", &title)]),
                         tr("A plugin can do anything on your computer. Only switch it on if you trust its source."), tr("Switch on"), true)
            } else {
                question(fill(&tr("Switch off {plugin}?"), &[("plugin", &title)]),
                         tr("Without it whatever it added to Telescope is gone. Its settings stay saved for when you switch it back on."), tr("Switch off"), false)
            }, Commit::Added { name: name.into(), on });
        }
        if tile.optional {
            let needs = self.builtins.iter().any(|p| p.name == name && p.needs_vision) && !self.download.ready();
            if on && needs {
                return Step::Ask(question(
                    fill(&tr("Download about {mb} MB?"), &[("mb", &self.download.size_mb().to_string())]),
                    fill(&tr("{plugin} uses MediaPipe to find people in the picture. It comes as a separate download from Telescope's releases on GitHub, which the other optional plugins share."), &[("plugin", &tile.title)]),
                    tr("Download and switch on"), false), Commit::DownloadThenOn { name: name.into() });
            }
            return Step::Done(self.commit(Commit::Optional { name: name.into(), on }));
        }
        if !on {
            let mut text = String::new();
            if !tile.when_off.is_empty() {
                text.push_str(&tile.when_off);
                text.push_str("\n\n");
            }
            text.push_str(&tr("Its settings stay saved for when you switch it back on."));
            return Step::Ask(question(fill(&tr("Switch off {plugin}?"), &[("plugin", &tile.title)]), text, tr("Switch off"), false), Commit::Builtin { name: name.into(), on });
        }
        Step::Done(self.commit(Commit::Builtin { name: name.into(), on }))
    }

    fn install(&mut self, path: PathBuf, replace: bool) -> Step {
        let result = self.admin.install_zip(&path, replace);
        let mut outcome = Outcome::default();
        let item = match result {
            Err(InstallFailure::Exists(name)) => {
                let old = self.installed.iter().find(|i| i.name == name && !i.dev);
                let title = old.map(|o| format!("{} {}", o.title, o.version).trim().to_string()).unwrap_or(name);
                self.refresh();
                return Step::Ask(question(
                    fill(&tr("Replace {plugin}?"), &[("plugin", &title)]),
                    tr("The version in this .zip takes its place with the same settings. Like any plugin you add it starts out switched off."),
                    tr("Replace"), false), Commit::Install { path, replace: true });
            }
            Err(InstallFailure::Failed(why)) => {
                self.refresh();
                outcome.notices.push(warning("Couldn't install it", why));
                return Step::Done(outcome);
            }
            Ok(item) => item,
        };
        self.refresh();
        outcome.notices.push(if !item.problem.is_empty() {
            warning("Installed, but it won't load", fill(&tr("{plugin} was installed, but it can't run yet. {problem}"), &[("plugin", &item.title), ("problem", &item.problem)]))
        } else if replace {
            info("Replaced", fill(&tr("{plugin} {version} is in place but switched off for now. To use it, switch it on again and restart Telescope."), &[("plugin", &item.title), ("version", &item.version)]))
        } else {
            info("Installed", fill(&tr("{plugin} is installed but switched off for now. To use it, switch it on and restart Telescope."), &[("plugin", &item.title)]))
        });
        Step::Done(outcome)
    }

    /// Saves a change and shows the tiles as they are now; a failure says so (_save).
    fn save(&mut self, change: impl FnOnce(&dyn PluginAdmin) -> std::io::Result<()>) -> Outcome {
        let mut outcome = Outcome::default();
        if let Err(e) = change(self.admin.as_ref()) {
            log::error!("Couldn't save a plugin switch: {e}");
            outcome.notices.push(warning("Couldn't save that", fill(&tr("Telescope couldn't save the change: {error}"), &[("error", &e.to_string())])));
        }
        self.refresh();
        outcome
    }

    /// Does what was asked, after any question was agreed to.
    pub fn commit(&mut self, commit: Commit) -> Outcome {
        match commit {
            Commit::Builtin { name, on } => self.save(|a| a.set_builtin_off(&name, !on)),
            Commit::Optional { name, on } => self.save(|a| a.set_optional_on(&name, on)),
            Commit::DownloadThenOn { name } => {
                self.download.start(&name);
                self.refresh();
                Outcome::default()
            }
            Commit::Added { name, on } => {
                self.save(|a| a.set_enabled(&name, on))
            }
            Commit::AllOn => self.save(|a| a.reset_to_defaults()),
            Commit::Remove { name } => {
                let mut outcome = Outcome::default();
                if let Err(e) = self.admin.remove(&name) {
                    log::error!("Couldn't remove plugin {name}: {e}");
                    outcome.notices.push(warning("Couldn't remove it", fill(&tr("Telescope couldn't delete its folder: {error}"), &[("error", &e)])));
                }
                self.refresh();
                outcome
            }
            Commit::RemovePack => {
                let mut outcome = Outcome::default();
                if let Err(e) = self.download.remove() {
                    outcome.notices.push(warning("Couldn't remove it", fill(&tr("Telescope couldn't delete it: {error}"), &[("error", &e)])));
                }
                self.refresh();
                outcome
            }
            Commit::Install { path, replace } => match self.install(path, replace) {
                Step::Done(outcome) => outcome,
                Step::Ask(..) => Outcome::default(),
            },
            Commit::Updates { key, on } => {
                let outcome = self.save(|a| a.set_switch(key, on));
                if on {
                    self.admin.maybe_check_updates();
                }
                outcome
            }
            Commit::Restart => Outcome { notices: vec![], restart: true },
        }
    }

    /// request() and commit() in one, answering any question with `reply` (what the tests do).
    pub fn run(&mut self, request: Request, reply: &mut dyn FnMut(&Question) -> bool) -> Outcome {
        match self.request(request) {
            Step::Done(outcome) => outcome,
            Step::Ask(q, then) => {
                if !reply(&q) {
                    self.refresh();
                    return Outcome::default();
                }
                self.commit(then)
            }
        }
    }

    /// The folder for the "Open plugins folder" button: made when it isn't there yet.
    pub fn make_plugins_dir(&self) -> Result<PathBuf, Notice> {
        let folder = self.admin.plugins_dir();
        std::fs::create_dir_all(&folder).map(|_| folder).map_err(|e| warning("Couldn't open it", fill(&tr("Telescope couldn't make its plugins folder: {error}"), &[("error", &e.to_string())])))
    }

    /// The folder the Downloads of this computer is in, where picking a .zip starts (Downloads if there is one, else home).
    pub fn start_folder() -> PathBuf {
        let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from).unwrap_or_default();
        let downloads = home.join("Downloads");
        if downloads.is_dir() { downloads } else { home }
    }

    /// Whether a path is the one .zip file a drop must be.
    pub fn dropped_zip(paths: &[&Path]) -> Option<PathBuf> {
        match paths {
            [one] if one.to_string_lossy().to_lowercase().ends_with(".zip") => Some(one.to_path_buf()),
            _ => None,
        }
    }
}


/// The files of one drop. winit reports each file as an event of its own, so they're gathered until the window has seen them all.
#[derive(Default)]
pub struct DropBatch {
    paths: std::cell::RefCell<Vec<PathBuf>>,
}

impl DropBatch {
    /// Adds a file. True for the first of a drop, which is the one that has to schedule take().
    pub fn add(&self, path: PathBuf) -> bool {
        let mut paths = self.paths.borrow_mut();
        paths.push(path);
        paths.len() == 1
    }

    /// The .zip the drop was, if it was exactly one; the batch is empty afterwards either way.
    pub fn take(&self) -> Option<PathBuf> {
        let paths = std::mem::take(&mut *self.paths.borrow_mut());
        Panel::dropped_zip(&paths.iter().map(PathBuf::as_path).collect::<Vec<_>>())
    }
}
