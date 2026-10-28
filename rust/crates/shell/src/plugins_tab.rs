//! The Plugins tab of Settings (widgets/plugins_panel.py) as data: every built-in plugin as a tile with its switch,
//! grouped by what it's for, with the search and the All / On / Off filter. Added plugins come from the plugin host
//! once it lists them; until then the Added plugins box shows its empty note.

use crate::switches;
use crate::texts::{fill, trn};
use serde_json::Value;
use std::collections::BTreeSet;
use telescope_core::i18n::tr;
use telescope_core::plugin::{PluginInfo, PLUGIN_CATEGORIES};

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

#[derive(Clone, Debug, PartialEq)]
pub struct Tile {
    pub name: String,
    pub title: String,
    pub about: String,
    pub icon: String,
    pub group: String,
    pub on: bool,
    pub locked: bool,
    pub tooltip: String,
    pub note: String,
    pub note_kind: String,
    pub badges: Vec<Badge>,
    pub optional: bool,
    pub when_off: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    pub id: String,
    pub title: String,
    pub tiles: Vec<Tile>,
}

/// category_of: a plugin's group, Extras for one that names none of them.
pub fn category_of(value: &str) -> &str {
    if PLUGIN_CATEGORIES.iter().any(|(id, _)| *id == value) { value } else { "extras" }
}

pub struct Panel {
    pub builtins: Vec<PluginInfo>,
    /// The names of the plugins running now.
    pub running: BTreeSet<String>,
    pub groups: Vec<Group>,
    pub pending: BTreeSet<String>,
    pub all_on_enabled: bool,
}

impl Panel {
    pub fn new(builtins: Vec<PluginInfo>, running: BTreeSet<String>, cfg: &Value) -> Panel {
        let mut panel = Panel { builtins, running, groups: vec![], pending: BTreeSet::new(), all_on_enabled: false };
        panel.refresh(cfg);
        panel
    }

    /// Rebuilds the tiles from the switches as saved now.
    pub fn refresh(&mut self, cfg: &Value) {
        let (off, on) = (switches::builtin_off(cfg), switches::optional_on(cfg));
        self.pending.clear();
        self.all_on_enabled = self.builtins.iter().any(|p| off.contains(&p.name) && !p.required && !p.optional);
        let mut groups: Vec<Group> = PLUGIN_CATEGORIES.iter()
            .map(|(id, name)| Group { id: id.to_string(), title: tr(name), tiles: vec![] }).collect();
        for p in &self.builtins.clone() {
            let tile = if p.optional { self.optional_tile(p, on.contains(&p.name)) } else { self.builtin_tile(p, p.required || !off.contains(&p.name)) };
            let g = category_of(&p.category);
            if let Some(group) = groups.iter_mut().find(|gr| gr.id == g) {
                group.tiles.push(tile);
            }
        }
        for g in &mut groups {
            g.tiles.sort_by_key(|t| t.title.to_lowercase());
        }
        groups.retain(|g| !g.tiles.is_empty());
        self.groups = groups;
    }

    fn base(&self, p: &PluginInfo, on: bool) -> Tile {
        Tile {
            name: p.name.clone(),
            title: tr(if p.title.is_empty() { &p.name } else { &p.title }),
            about: tr(&p.about),
            icon: p.icon.clone(),
            group: category_of(&p.category).to_string(),
            on,
            locked: false,
            tooltip: String::new(),
            note: String::new(),
            note_kind: "dim".into(),
            badges: vec![],
            optional: p.optional,
            when_off: if p.when_off.is_empty() { String::new() } else { tr(&p.when_off) },
        }
    }

    fn waits(&mut self, tile: &mut Tile) {
        self.pending.insert(tile.name.clone());
        tile.badges.push(Badge { text: tr("Waits for a restart"), kind: "restart".into() });
    }

    fn builtin_tile(&mut self, p: &PluginInfo, on: bool) -> Tile {
        let mut tile = self.base(p, on);
        if p.required {
            tile.locked = true;
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

    /// An optional built-in plugin: off until switched on. The vision download itself is the vision session's.
    fn optional_tile(&mut self, p: &PluginInfo, on: bool) -> Tile {
        let mut tile = self.base(p, on);
        if p.needs_vision {
            tile.badges.insert(0, Badge { text: tr("Needs the vision download"), kind: "vision".into() });
        }
        if on != self.running.contains(&p.name) {
            self.waits(&mut tile);
        }
        tile
    }

    /// The tiles the search and filter leave, group by group (empty groups dropped).
    pub fn shown(&self, query: &str, filter: Filter) -> Vec<(Group, String)> {
        let words: Vec<String> = query.to_lowercase().split_whitespace().map(String::from).collect();
        self.groups.iter().filter_map(|g| {
            let tiles: Vec<Tile> = g.tiles.iter().filter(|t| {
                let hay = format!("{} {}", t.title, t.about).to_lowercase();
                words.iter().all(|w| hay.contains(w)) && (filter == Filter::All || t.on == (filter == Filter::On))
            }).cloned().collect();
            let on = g.tiles.iter().filter(|t| t.on).count();
            // Translators: next to a group's name on the Plugins tab, how many of its plugins are switched on
            let count = fill(&tr("{on} of {total} on"), &[("on", &on.to_string()), ("total", &g.tiles.len().to_string())]);
            (!tiles.is_empty()).then(|| (Group { tiles, ..g.clone() }, count))
        }).collect()
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
        if off == 0 { String::new() } else { tr("Plugins ({n} off)").replace("{n}", &off.to_string()) }
    }

    /// What switching a built-in plugin off asks first: (title, text, yes).
    pub fn ask_off(tile: &Tile) -> (String, String, String) {
        let mut text = String::new();
        if !tile.when_off.is_empty() {
            text.push_str(&tile.when_off);
            text.push_str("\n\n");
        }
        text.push_str(&tr("Its settings stay saved for when you switch it back on."));
        (fill(&tr("Switch off {plugin}?"), &[("plugin", &tile.title)]), text, tr("Switch off"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn info(name: &str, category: &str) -> PluginInfo {
        PluginInfo { name: name.into(), title: name.into(), category: category.into(), ..Default::default() }
    }

    fn panel(cfg: &Value, running: &[&str]) -> Panel {
        let mut connection = info("connection", "streaming");
        connection.required = true;
        let mut tracking = info("tracking", "vision");
        tracking.optional = true;
        tracking.needs_vision = true;
        let builtins = vec![info("notes", "on_screen"), info("camera_control", "camera"), connection, tracking, info("odd", "nonsense")];
        Panel::new(builtins, running.iter().map(|s| s.to_string()).collect(), cfg)
    }

    #[test]
    fn tiles_sit_in_their_groups_in_order() {
        let p = panel(&json!({}), &["notes", "camera_control", "connection", "odd"]);
        let ids: Vec<&str> = p.groups.iter().map(|g| g.id.as_str()).collect();
        assert_eq!(ids, ["camera", "vision", "on_screen", "streaming", "extras"]);
        assert!(p.pending.is_empty());
        let required = &p.groups[3].tiles[0];
        assert!(required.locked && required.badges[0].kind == "lock");
    }

    #[test]
    fn switching_off_waits_for_a_restart_and_says_how_many() {
        let mut cfg = json!({});
        switches::set_builtin_off(&mut cfg, "notes", true);
        let p = panel(&cfg, &["notes", "camera_control", "connection", "odd"]);
        assert_eq!(p.pending.iter().collect::<Vec<_>>(), ["notes"]);
        assert_eq!(p.footer_note(), "Restart to apply 1 change");
        assert!(p.all_on_enabled);
        let notes = &p.groups.iter().find(|g| g.id == "on_screen").unwrap().tiles[0];
        assert_eq!(notes.badges.last().unwrap().text, "Waits for a restart");
    }

    #[test]
    fn the_filter_and_search_narrow_the_tiles() {
        let p = panel(&json!({}), &["notes", "camera_control", "connection", "odd"]);
        let names = |q: &str, f: Filter| p.shown(q, f).iter().flat_map(|(g, _)| g.tiles.iter().map(|t| t.name.clone())).collect::<Vec<_>>();
        assert_eq!(names("", Filter::Off), ["tracking"]);
        assert_eq!(names("not", Filter::All), ["notes"]);
        assert!(names("zzz", Filter::All).is_empty());
        assert_eq!(Panel::empty_note("zzz", Filter::All), "No plugins match “zzz”.");
        assert_eq!(p.shown("", Filter::All)[0].1, "1 of 1 on");
    }

    #[test]
    fn the_tab_says_how_many_are_off() {
        assert_eq!(Panel::tab_title(16), "Plugins (16 off)");
        assert_eq!(Panel::tab_title(0), "");
    }

    #[test]
    fn the_grid_has_three_columns_when_there_is_room() {
        assert_eq!(columns(645.0), 2);
        assert_eq!(columns(740.0), 3);
        assert_eq!(columns(390.0), 1);
    }
}
