//! The editor for One picture, a window of the plugin's own: a live preview to drag, resize and crop
//! the streams on, and the controls for borders, corners, the background and saved layouts. Every change goes out at once. Its
//! window draws what editor_props says and reports the pointer in px of the picture.

use telescope_core::i18n::Fields;
use super::*;
use crate::composite::{self, crop_px, Tile, MAX_BORDER, MAX_RADIUS, MAX_TILES, MIN_SIZE, SNAP};
use telescope_core::frame::PixelFormat;
use telescope_core::host::{FileDialog, FileDialogKind, RequestId};
use telescope_core::ui::WindowSpec;

/// The window, as a piece of the plugin's UI.
pub(super) const WINDOW: &str = "editor";
const EDITOR_UI: &str = concat!(include_str!("../ui/parts.slint"), "\n", include_str!("../ui/composite_editor.slint"));
const T_PREVIEW: &str = "editor-preview";
const PICTURES: &str = n_("Pictures (*.png *.jpg *.jpeg *.webp *.bmp)");
/// How far from a handle a click still grabs it, in px
const HANDLE_PX: f64 = 9.0;
/// About 15 pictures a second is plenty to line things up by
const PREVIEW_MS: u64 = 66;
const PREVIEW_MAX_W: u32 = 1280;
/// Arrow keys move the picked stream this many pixels of the picture (Shift: ten times as far)
const NUDGE_PX: f64 = 1.0;
const ZOOM_STEP: f64 = 1.1;
/// The handles round a tile, as (name, across, down).
const HANDLES: [(&str, f64, f64); 8] =
    [("tl", 0.0, 0.0), ("t", 0.5, 0.0), ("tr", 1.0, 0.0), ("r", 1.0, 0.5), ("br", 1.0, 1.0), ("b", 0.5, 1.0), ("bl", 0.0, 1.0), ("l", 0.0, 0.5)];

#[derive(Clone, Debug, PartialEq)]
enum DragKind {
    Move,
    Resize(String),
    Pan,
}

struct Drag {
    kind: DragKind,
    /// Where it began, in fractions of the picture and in px, and the tile as it was.
    start: (f64, f64),
    pos: (f64, f64),
    /// The tile as it was drawn, at the zoom Max zoom allows.
    tile: Tile,
    /// The zoom the layout has for it, which can be past that: a drag that leaves the shown zoom alone leaves this alone.
    saved_zoom: f64,
}

/// The keys held during a pointer event.
#[derive(Clone, Copy, Default)]
struct Mods {
    ctrl: bool,
    shift: bool,
    alt: bool,
}

/// What a name typed into the prompt is for.
#[derive(Clone, Debug, PartialEq)]
enum NameFor {
    Save,
    Rename(String),
}

pub(super) struct Editor {
    selected: i64,
    /// The tile and handle under the pointer.
    hover: (i64, String),
    drag: Option<Drag>,
    /// What the drag going on does, shown in the corner.
    mode: String,
    /// Snap lines to draw while dragging, in fractions of the picture.
    guides: (Vec<f64>, Vec<f64>),
    /// The picture's size on screen in px, from the last pointer event or the window.
    area: (f64, f64),
    cursor: &'static str,
    /// The name prompt's purpose and what it starts with.
    asking: Option<(NameFor, String)>,
    /// "There's already a layout called…": what asked, and the tidied name.
    replacing: Option<(NameFor, String)>,
    /// Which colour the sliders under its swatch change: "border", "background" or "".
    colour: &'static str,
    picture_asked: Option<RequestId>,
    compositor: Compositor,
    prepared: Option<(Layout, (u32, u32))>,
    /// Tiles the eye took out of the layout while this window is open, with the layer they were on: the eye brings one back as it was.
    forgotten: Vec<(usize, Tile)>,
}

impl Editor {
    fn new(reader: PictureReader) -> Editor {
        Editor {
            selected: -1, hover: (-1, String::new()), drag: None, mode: String::new(), guides: (Vec::new(), Vec::new()), area: (960.0, 540.0),
            cursor: "", asking: None, replacing: None, colour: "", picture_asked: None, compositor: Compositor::new(reader), prepared: None, forgotten: Vec::new(),
        }
    }

    fn tile_rect(&self, tile: &Tile) -> (f64, f64, f64, f64) {
        (tile.x * self.area.0, tile.y * self.area.1, tile.w * self.area.0, tile.h * self.area.1)
    }

    /// (tile index, handle or "") under pos, the picked tile's handles first and then the frontmost tile.
    fn hit(&self, tiles: &[Tile], pos: (f64, f64)) -> (i64, String) {
        if let Some(tile) = usize::try_from(self.selected).ok().and_then(|i| tiles.get(i)).filter(|t| !t.locked) {
            let (x, y, w, h) = self.tile_rect(tile);
            for (name, across, down) in HANDLES {
                if (x + across * w - pos.0).abs() <= HANDLE_PX && (y + down * h - pos.1).abs() <= HANDLE_PX {
                    return (self.selected, name.to_string());
                }
            }
        }
        // a locked tile can't be moved by a click on the picture: the click goes through to what is behind it
        for (i, tile) in tiles.iter().enumerate().rev().filter(|(_, t)| !t.locked) {
            let (x, y, w, h) = self.tile_rect(tile);
            if pos.0 >= x && pos.0 < x + w && pos.1 >= y && pos.1 < y + h {
                return (i as i64, String::new());
            }
        }
        (-1, String::new())
    }
}

fn cursor_for(index: i64, handle: &str) -> &'static str {
    match handle {
        "tl" | "br" => "nwse",
        "tr" | "bl" => "nesw",
        "l" | "r" => "ew",
        "t" | "b" => "ns",
        _ if index >= 0 => "move",
        _ => "",
    }
}

/// tile with what it shows moved along with a right-drag of delta (px on screen), as if dragging the stream.
fn panned(tile: &Tile, from: &Tile, frame: Option<&Frame>, rect: (f64, f64, f64, f64), delta: (f64, f64)) -> Tile {
    let Some(frame) = frame else { return tile.clone() };
    let (fw, fh) = (frame.width as i64, frame.height as i64);
    let (x0, y0, x1, y1) = crop_px(tile, fw, fh, (rect.2.round() as i64).max(1), (rect.3.round() as i64).max(1));
    let rw = ((1.0 - tile.crop_l - tile.crop_r) * fw as f64).max(1.0);
    let rh = ((1.0 - tile.crop_t - tile.crop_b) * fh as f64).max(1.0);
    let cx = from.cx - delta.0 / rect.2.max(1.0) * (x1 - x0) as f64 / rw;
    let cy = from.cy - delta.1 / rect.3.max(1.0) * (y1 - y0) as f64 / rh;
    Tile { cx: cx.clamp(0.0, 1.0), cy: cy.clamp(0.0, 1.0), ..tile.clone() }
}

fn hex_channels(colour: &str) -> [u8; 3] {
    let byte = |i: usize| colour.get(i..i + 2).and_then(|h| u8::from_str_radix(h, 16).ok()).unwrap_or(0);
    [byte(1), byte(3), byte(5)]
}

impl CompositePlugin {
    /// Opens the editor, or brings it forward.
    pub fn open_editor(&mut self, host: &mut dyn Host) {
        if self.editor.is_none() {
            self.editor = Some(Editor::new(self.reader.clone()));
        }
        let ui = UiSpec { source: SlintSource::Text(EDITOR_UI.into()), component: "CompositeEditor".into(), props: self.editor_props() };
        host.open_window(WINDOW, &WindowSpec { ui, size: [1240.0, 760.0] });
        host.schedule(PREVIEW_MS, T_PREVIEW);
    }

    #[cfg(test)]
    pub(super) fn editor_open(&self) -> bool {
        self.editor.is_some()
    }

    fn frames_now(&self) -> Frames {
        let hub = self.shared.hub();
        self.shared.frames(&hub, None, monotonic_ns())
    }

    /// (source id, name, aspect) of the running streams that have a picture, in the order they started.
    pub fn streams(&self) -> Vec<composite::Stream> {
        let mut frames = self.frames_now();
        let aspect = |frame: &Frame| frame.width as f64 / frame.height.max(1) as f64;
        let mut out: Vec<composite::Stream> = Vec::new();
        for sf in self.shared.store().stream_frames() {
            if let Some((frame, _)) = frames.remove(&sf.source_id) {
                out.push((sf.source_id, sf.name, aspect(&frame)));
            }
        }
        // The main stream's picture with no frame of its own in the store comes last, as setdefault puts it
        out.extend(frames.iter().map(|(sid, (frame, _))| (sid.clone(), sid.clone(), aspect(frame))));
        out
    }

    fn canvas_aspect(&self) -> f64 {
        let (w, h) = self.size();
        w as f64 / h as f64
    }

    /// The stream's width over height, from its newest frame; None while it's off.
    fn source_aspect(&self, source_id: &str) -> Option<f64> {
        self.streams().into_iter().find(|s| s.0 == source_id).map(|s| s.2)
    }

    /// The selected tile as it is drawn, at the zoom Max zoom allows.
    fn picked(&self) -> Option<Tile> {
        let e = self.editor.as_ref()?;
        self.layout.tiles.get(usize::try_from(e.selected).ok()?).map(|t| self.capped(t))
    }

    /// The tiles as they are drawn: a zoom saved above Max zoom shows at Max zoom and is kept, to come back when Max zoom goes up.
    fn shown_tiles(&self) -> Vec<Tile> {
        self.layout.tiles.iter().map(|t| self.capped(t)).collect()
    }

    fn replace_tile(&mut self, host: &mut dyn Host, index: i64, mut tile: Tile) {
        let Some(at) = usize::try_from(index).ok().filter(|i| *i < self.layout.tiles.len()) else { return };
        let old = self.layout.tiles[at].clone();
        // a locked tile takes no change but its own lock
        if old.locked && (Tile { locked: old.locked, ..tile.clone() }) != old {
            return;
        }
        // an edit that leaves the shown zoom alone leaves the saved one alone too
        if tile.zoom == self.capped(&old).zoom {
            tile.zoom = old.zoom;
        }
        if old != tile {
            let mut layout = self.layout.clone();
            layout.tiles[at] = tile;
            self.set_layout(host, layout);
        }
    }

    pub(super) fn select(&mut self, index: i64) {
        if let Some(e) = self.editor.as_mut() {
            e.selected = index;
        }
    }

    // ── The picture ───────────────────────────────────────────────────

    fn pointer_down(&mut self, pos: (f64, f64), right: bool) {
        let tiles = self.shown_tiles();
        let Some(e) = self.editor.as_mut() else { return };
        let (index, handle) = e.hit(&tiles, pos);
        e.selected = index;
        if index < 0 {
            return;
        }
        let saved = self.layout.tiles.get(index as usize).map_or(1.0, |t| t.zoom);
        let Some(e) = self.editor.as_mut() else { return };
        let kind = if right { DragKind::Pan } else if handle.is_empty() { DragKind::Move } else { DragKind::Resize(handle) };
        let start = (pos.0 / e.area.0, pos.1 / e.area.1);
        e.drag = Some(Drag { kind, start, pos, tile: tiles[index as usize].clone(), saved_zoom: saved });
    }

    fn pointer_move(&mut self, host: &mut dyn Host, pos: (f64, f64), mods: Mods) {
        let tiles = self.shown_tiles();
        let Some(e) = self.editor.as_mut() else { return };
        let Some(d) = &e.drag else {
            let hover = e.hit(&tiles, pos);
            e.cursor = cursor_for(hover.0, &hover.1);
            e.hover = hover;
            return;
        };
        let (kind, tile, start, from, saved) = (d.kind.clone(), d.tile.clone(), d.start, d.pos, d.saved_zoom);
        let selected = e.selected;
        let (mut dx, mut dy) = (pos.0 / e.area.0 - start.0, pos.1 / e.area.1 - start.1);
        let snapping = !mods.ctrl; // Ctrl: no snapping, as in OBS
        let (xs, ys) = composite::snap_lines(&tiles, selected);
        let (mut guides_x, mut guides_y) = (Vec::new(), Vec::new());
        let (mode, new) = match &kind {
            DragKind::Move => {
                let (mut nx, mut ny) = (tile.x + dx, tile.y + dy);
                if snapping {
                    let (sx, gx) = composite::snap(&[nx, nx + tile.w / 2.0, nx + tile.w], &xs, SNAP);
                    let (sy, gy) = composite::snap(&[ny, ny + tile.h / 2.0, ny + tile.h], &ys, SNAP);
                    (nx, ny) = (nx + sx, ny + sy);
                    guides_x.extend(gx);
                    guides_y.extend(gy);
                }
                ("move", composite::moved(&tile, nx, ny))
            }
            DragKind::Resize(handle) => {
                // OBS's keys: Alt crops the side being dragged, Shift lets the shape go, a plain drag keeps it
                let mode = if mods.alt { "crop" } else if mods.shift { "stretch" } else { "scale" };
                if snapping {
                    let ex = if handle.contains('l') { Some(tile.x + dx) } else if handle.contains('r') { Some(tile.x + tile.w + dx) } else { None };
                    let ey = if handle.contains('t') { Some(tile.y + dy) } else if handle.contains('b') { Some(tile.y + tile.h + dy) } else { None };
                    if let Some(ex) = ex {
                        dx += composite::snap(&[ex], &xs, SNAP).0;
                    }
                    if let Some(ey) = ey {
                        dy += composite::snap(&[ey], &ys, SNAP).0;
                    }
                }
                let aspect = self.source_aspect(&tile.source_id);
                let new = composite::dragged(&tile, handle, dx, dy, mode, self.canvas_aspect(), aspect);
                if snapping {
                    guides_x = xs.iter().copied().filter(|g| [new.x, new.x + new.w].iter().any(|e| (g - e).abs() < 1e-6)).collect();
                    guides_y = ys.iter().copied().filter(|g| [new.y, new.y + new.h].iter().any(|e| (g - e).abs() < 1e-6)).collect();
                }
                (mode, new)
            }
            DragKind::Pan => {
                let frames = self.frames_now();
                let frame = frames.get(&tile.source_id).map(|f| &f.0);
                let now = tiles.get(selected as usize).cloned().unwrap_or_else(|| tile.clone());
                let e = self.editor.as_ref().unwrap();
                ("pan", panned(&now, &tile, frame, e.tile_rect(&now), (pos.0 - from.0, pos.1 - from.1)))
            }
        };
        let text = match mode {
            "move" => tr("Move"),
            "scale" => tr("Scale"),
            "stretch" => tr("Stretch"),
            "crop" => tr("Crop"),
            _ => tr("Pan"),
        };
        let e = self.editor.as_mut().unwrap();
        e.mode = if snapping || mode == "pan" { text } else { tr("{mode}, no snapping").field("mode", &text) };
        e.guides = (guides_x, guides_y);
        // switching between resize modes mid-drag works from the zoom the drag began with, not from one an earlier step of it wrote
        let new = if new.zoom == tile.zoom { Tile { zoom: saved, ..new } } else { new };
        self.replace_tile(host, selected, new);
    }

    fn pointer_up(&mut self) {
        if let Some(e) = self.editor.as_mut() {
            e.drag = None;
            e.mode.clear();
            e.guides = (Vec::new(), Vec::new());
        }
    }

    fn wheel(&mut self, host: &mut dyn Host, pos: (f64, f64), steps: f64) {
        let tiles = self.shown_tiles();
        let Some(e) = self.editor.as_mut() else { return };
        let (index, _) = e.hit(&tiles, pos);
        if index < 0 {
            return;
        }
        e.selected = index;
        let tile = &tiles[index as usize];
        let zoom = (tile.zoom * ZOOM_STEP.powf(steps)).clamp(1.0, self.max_zoom);
        self.replace_tile(host, index, Tile { zoom, ..tile.clone() });
    }

    /// One of the Left, Top, Width and Height boxes: to `px` (canvas pixels), or by that many (step), kept on the picture.
    fn set_box(&mut self, host: &mut dyn Host, field: &str, px: f64, by: bool) {
        let Some(tile) = self.picked() else { return };
        let (cw, ch) = self.size();
        let (cw, ch) = (cw as f64, ch as f64);
        let mut new = tile.clone();
        match field {
            "x" => new.x = (if by { tile.x + px / cw } else { px / cw }).clamp(0.0, (1.0 - tile.w).max(0.0)),
            "y" => new.y = (if by { tile.y + px / ch } else { px / ch }).clamp(0.0, (1.0 - tile.h).max(0.0)),
            "w" | "h" => {
                // a stream that keeps its shape when dragged keeps it when typed, as well: the other side follows
                let ratio = (tile.w * cw) / (tile.h * ch).max(1e-9);
                let wanted = if field == "w" { if by { tile.w * cw + px } else { px } } else { (if by { tile.h * ch + px } else { px }) * ratio };
                let mut w = (wanted / cw).max(MIN_SIZE);
                if !tile.stretch {
                    // as big as the picture allows from where the box starts, in the same shape
                    let most = ((1.0 - tile.x).min((1.0 - tile.y) * ch * ratio / cw)).max(MIN_SIZE);
                    w = w.min(most);
                    new.h = (w * cw / ratio / ch).max(MIN_SIZE);
                } else if field == "h" {
                    new.h = (if by { tile.h + px / ch } else { px / ch }).clamp(MIN_SIZE, (1.0 - tile.y).max(MIN_SIZE));
                    w = tile.w;
                } else {
                    w = w.min((1.0 - tile.x).max(MIN_SIZE));
                }
                new.w = w;
            }
            _ => return,
        }
        let index = self.editor.as_ref().map_or(-1, |e| e.selected);
        self.replace_tile(host, index, new);
    }

    /// One of the crop boxes: the side ("l", "t", "r", "b") to `pct` percent of the stream, or by that many (step). "reset" shows all of it.
    fn set_crop(&mut self, host: &mut dyn Host, side: &str, pct: f64, by: bool) {
        let Some(tile) = self.picked() else { return };
        let pinned = match self.source_aspect(&tile.source_id) {
            Some(aspect) => composite::pinned(&tile, aspect, self.canvas_aspect()),
            None => tile.clone(),
        };
        let new = if side == "reset" {
            let mut t = pinned;
            for s in ["l", "t", "r", "b"] {
                t = composite::crop_side(&t, s, 0.0);
            }
            t
        } else {
            let now = match side {
                "l" => pinned.crop_l,
                "t" => pinned.crop_t,
                "r" => pinned.crop_r,
                _ => pinned.crop_b,
            };
            composite::crop_side(&pinned, side, if by { now + pct / 100.0 } else { pct / 100.0 })
        };
        let index = self.editor.as_ref().map_or(-1, |e| e.selected);
        self.replace_tile(host, index, new);
    }

    /// The picked tile and another trade places and sizes; `other` counts the tiles but the picked one.
    fn swap_picked(&mut self, host: &mut dyn Host, other: i64) {
        let picked = self.editor.as_ref().map_or(-1, |e| e.selected);
        let others: Vec<usize> = (0..self.layout.tiles.len()).filter(|i| *i as i64 != picked).collect();
        let (Some(&there), true) = (usize::try_from(other).ok().and_then(|o| others.get(o)), picked >= 0) else { return };
        if self.layout.tiles[picked as usize].locked || self.layout.tiles[there].locked {
            return;
        }
        let mut tiles = composite::swapped(&self.layout.tiles, picked as usize, there);
        // each is cut to the shape of its new box, so a wide picture isn't squeezed into a tall box
        let canvas = self.canvas_aspect();
        for i in [picked as usize, there] {
            if let Some(aspect) = self.source_aspect(&tiles[i].source_id) {
                tiles[i] = composite::refit(&tiles[i], aspect, canvas);
            }
        }
        // the picked stream is now where the other was in the layers
        self.select(there as i64);
        self.set_layout(host, Layout { tiles, ..self.layout.clone() });
    }

    fn nudge(&mut self, host: &mut dyn Host, key: &str, shift: bool) {
        let Some(tile) = self.picked() else { return };
        let (mx, my) = match key {
            "left" => (-1.0, 0.0),
            "right" => (1.0, 0.0),
            "up" => (0.0, -1.0),
            "down" => (0.0, 1.0),
            _ => return,
        };
        let (w, h) = self.size();
        let step = NUDGE_PX * if shift { 10.0 } else { 1.0 };
        let index = self.editor.as_ref().map_or(-1, |e| e.selected);
        self.replace_tile(host, index, composite::moved(&tile, tile.x + mx * step / w as f64, tile.y + my * step / h as f64));
    }

    fn forget_hidden(&mut self) {
        if let Some(e) = self.editor.as_mut() {
            e.forgotten.clear();
        }
    }

    fn start_from(&mut self, host: &mut dyn Host, which: i64) {
        self.forget_hidden();
        let streams = self.streams();
        let aspect = self.canvas_aspect();
        let tiles = match which {
            0 => composite::side_by_side(&streams, aspect),
            1 => composite::big_and_small(&streams, aspect),
            _ => composite::grid(&streams, aspect),
        };
        self.select(-1);
        self.set_layout(host, Layout { tiles, ..self.layout.clone() });
    }

    // ── Saved layouts ─────────────────────────────────────────────────

    fn unused_name(&self) -> String {
        let names = self.names();
        let mut n = names.len() + 1;
        loop {
            let name = tr("Layout {n}").field("n", n.to_string());
            if !names.contains(&name) {
                return name;
            }
            n += 1;
        }
    }

    fn ask_save(&mut self) {
        let current = self.layout.name.clone();
        let start = if current.is_empty() { self.unused_name() } else { current };
        if let Some(e) = self.editor.as_mut() {
            e.asking = Some((NameFor::Save, start));
        }
    }

    fn ask_rename(&mut self) {
        let old = Some(self.saved_match()).filter(|m| !m.is_empty()).unwrap_or_else(|| self.layout.name.clone());
        if self.names().contains(&old) {
            if let Some(e) = self.editor.as_mut() {
                e.asking = Some((NameFor::Rename(old.clone()), old));
            }
        }
    }

    /// What was typed into the prompt: as it'll be saved, so a name that only matches once tidied up still asks.
    fn name_entered(&mut self, host: &mut dyn Host, typed: &str) {
        let Some((purpose, _)) = self.editor.as_mut().and_then(|e| e.asking.take()) else { return };
        let name = clean_name(typed);
        if name.is_empty() {
            return;
        }
        let same = match &purpose {
            NameFor::Save => name == self.layout.name,
            NameFor::Rename(old) => {
                if name == *old {
                    return;
                }
                false
            }
        };
        if !same && self.names().contains(&name) {
            if let Some(e) = self.editor.as_mut() {
                e.replacing = Some((purpose, name));
            }
            return;
        }
        self.apply_name(host, purpose, &name);
    }

    fn apply_name(&mut self, host: &mut dyn Host, purpose: NameFor, name: &str) {
        match purpose {
            NameFor::Save => {
                self.save_layout(host, name);
            }
            NameFor::Rename(old) => self.rename_layout(host, &old, name),
        }
    }

    fn delete_shown(&mut self, host: &mut dyn Host) {
        let name = Some(self.saved_match()).filter(|m| !m.is_empty()).unwrap_or_else(|| self.layout.name.clone());
        if self.names().contains(&name) {
            self.delete_layout(host, &name);
        }
    }

    // ── Which streams are in it ───────────────────────────────────────

    /// (source id, name, running) of everything the Streams list shows.
    fn stream_entries(&self) -> Vec<(String, String, bool)> {
        let running = self.streams();
        let mut entries: Vec<(String, String, bool)> = Vec::new();
        // the tiles of the layout first, the front one on top
        for tile in self.layout.tiles.iter().rev() {
            let live = running.iter().find(|r| r.0 == tile.source_id);
            let name = live.map(|r| r.1.clone()).unwrap_or_else(|| if tile.name.is_empty() { tile.source_id.clone() } else { tile.name.clone() });
            entries.push((tile.source_id.clone(), name, live.is_some()));
        }
        for (sid, name, _) in running {
            if !entries.iter().any(|e| e.0 == sid) {
                entries.push((sid, name, true));
            }
        }
        entries
    }

    fn put(&mut self, host: &mut dyn Host, source_id: &str, name: &str, on: bool) {
        let at = self.layout.tiles.iter().position(|t| t.source_id == source_id);
        let mut tiles: Vec<Tile> = self.layout.tiles.iter().filter(|t| t.source_id != source_id).cloned().collect();
        let mut picked = tiles.len() as i64 - 1;
        if !on {
            // the eye off: the stream leaves the picture, and its tile is remembered for as long as the window is open
            if let (Some(i), Some(e)) = (at, self.editor.as_mut()) {
                e.forgotten.retain(|(_, t)| t.source_id != source_id);
                e.forgotten.push((i, self.layout.tiles[i].clone()));
            }
        } else if tiles.len() < MAX_TILES {
            let back = self.editor.as_mut().and_then(|e| e.forgotten.iter().position(|(_, t)| t.source_id == source_id).map(|p| e.forgotten.remove(p)));
            if let Some((layer, tile)) = back {
                let layer = layer.min(tiles.len());
                tiles.insert(layer, tile);
                picked = layer as i64;
            } else {
                let aspect = self.source_aspect(source_id).unwrap_or_else(|| self.canvas_aspect());
                let b = composite::fit_box(aspect, (0.3, 0.3, 0.4, 0.4), self.canvas_aspect());
                tiles.push(Tile { radius: 16, ..Tile::boxed(source_id, name, b.0, b.1, b.2, b.3) });
                picked = tiles.len() as i64 - 1;
            }
        }
        self.select(if on { picked } else { -1 });
        self.set_layout(host, Layout { tiles, ..self.layout.clone() });
    }

    // ── The picked stream ─────────────────────────────────────────────

    fn edit_picked(&mut self, host: &mut dyn Host, change: impl FnOnce(&mut Tile)) {
        let Some(mut tile) = self.picked() else { return };
        change(&mut tile);
        let index = self.editor.as_ref().map_or(-1, |e| e.selected);
        self.replace_tile(host, index, tile);
    }

    fn fit_shape(&mut self, host: &mut dyn Host) {
        let Some(tile) = self.picked() else { return };
        let Some(aspect) = self.source_aspect(&tile.source_id) else { return };
        let fitted = composite::fitted(&tile, aspect, self.canvas_aspect());
        self.edit_picked(host, |t| *t = fitted);
    }

    /// The stream on row `from` of the Streams list (the front layer first) dragged to row `to`.
    fn move_layer(&mut self, host: &mut dyn Host, from: usize, to: usize) {
        let n = self.layout.tiles.len();
        if from >= n || to >= n || from == to {
            return;
        }
        let (index, step) = ((n - 1 - from) as i64, from as i64 - to as i64);
        let picked = self.editor.as_ref().map_or(-1, |e| e.selected);
        let tiles = composite::raised(&self.layout.tiles, index, step);
        // whoever was picked stays picked, wherever its layer went
        let id = usize::try_from(picked).ok().and_then(|i| self.layout.tiles.get(i)).map(|t| t.source_id.clone());
        let now = id.and_then(|id| tiles.iter().position(|t| t.source_id == id)).map_or(-1, |i| i as i64);
        self.select(now);
        self.set_layout(host, Layout { tiles, ..self.layout.clone() });
    }

    fn restack(&mut self, host: &mut dyn Host, step: i64) {
        let index = self.editor.as_ref().map_or(-1, |e| e.selected);
        let tiles = composite::raised(&self.layout.tiles, index, step);
        let to = (index + step).max(0).min(tiles.len() as i64 - 1);
        self.select(to);
        self.set_layout(host, Layout { tiles, ..self.layout.clone() });
    }

    // ── Background ────────────────────────────────────────────────────

    fn ask_picture(&mut self, host: &mut dyn Host) {
        let dialog = FileDialog { kind: FileDialogKind::Open, title: tr("Background picture"), start: String::new(), filters: vec![tr(PICTURES)] };
        let asked = host.pick_file(&dialog);
        if let Some(e) = self.editor.as_mut() {
            e.picture_asked = asked;
        }
    }

    /// A file picked for the background; true when it was the editor that asked.
    pub(super) fn editor_picked(&mut self, host: &mut dyn Host, request: RequestId, ok: bool, data: &Value) -> bool {
        if self.editor.as_ref().and_then(|e| e.picture_asked) != Some(request) {
            return false;
        }
        if let Some(e) = self.editor.as_mut() {
            e.picture_asked = None;
        }
        if let (true, Some(path)) = (ok, data.get("path").and_then(Value::as_str)) {
            let picture = path.to_string();
            self.set_layout(host, Layout { picture, ..self.layout.clone() });
        }
        true
    }

    fn colour_changed(&mut self, host: &mut dyn Host, channel: usize, value: u8) {
        let target = self.editor.as_ref().map_or("", |e| e.colour);
        let current = if target == "border" { self.picked().map(|t| t.border_colour.clone()) } else { Some(self.layout.background.clone()) };
        let Some(mut rgb) = current.map(|c| hex_channels(&c)) else { return };
        rgb[channel.min(2)] = value;
        let colour = format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2]);
        if target == "border" {
            self.edit_picked(host, |t| t.border_colour = colour);
        } else if target == "background" {
            self.set_layout(host, Layout { background: colour, ..self.layout.clone() });
        }
    }

    // ── The window ────────────────────────────────────────────────────

    /// Everything the window shows, as it is now (a preset or a shortcut may have changed the picture).
    pub(super) fn editor_props(&self) -> Value {
        let Some(e) = &self.editor else { return json!({}) };
        let layout = &Layout { tiles: self.shown_tiles(), ..self.layout.clone() };
        let names = self.names();
        let matched = self.saved_match();
        let current = names.iter().position(|n| *n == matched).map_or(-1, |i| i as i64);
        let streams = self.streams();
        let live: Vec<&str> = streams.iter().map(|s| s.0.as_str()).collect();
        let tiles: Vec<Value> = layout.tiles.iter().enumerate().map(|(i, t)| {
            let label = if t.name.is_empty() { t.source_id.clone() } else { t.name.clone() };
            json!({"x": t.x, "y": t.y, "w": t.w, "h": t.h, "missing": !live.contains(&t.source_id.as_str()), "name": label,
                   "gone": tr("{name} isn't streaming").field("name", &label),
                   "hover": e.hover.0 == i as i64 && e.selected != i as i64 && e.drag.is_none()})
        }).collect();
        let picked = self.picked();
        let picked = picked.as_ref();
        let handles: Vec<Value> = picked.map(|t| HANDLES.iter().map(|(name, across, down)| {
            let dragged = e.drag.as_ref().is_some_and(|d| d.kind == DragKind::Resize(name.to_string()));
            json!({"x": t.x + across * t.w, "y": t.y + down * t.h, "corner": name.len() == 2, "wide": *name == "t" || *name == "b",
                   "hot": dragged || (e.hover.0 == e.selected && e.hover.1 == *name)})
        }).collect()).unwrap_or_default();
        let rows: Vec<Value> = self.stream_entries().into_iter().map(|(sid, name, running)| {
            let status = if running { tr("Streaming") } else { tr("Not streaming") };
            let at = layout.tiles.iter().position(|t| t.source_id == sid);
            json!({"text": name, "status": status, "streaming": running, "dim": !running, "on": at.is_some(), "layer": at.is_some(), "locked": at.is_some_and(|i| layout.tiles[i].locked),
                   "picked": at.is_some_and(|i| i as i64 == e.selected)})
        }).collect();
        let picture_name = if layout.picture.is_empty() {
            tr("None")
        } else {
            std::path::Path::new(&layout.picture).file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned())
        };
        let colour_now = match e.colour {
            "border" => picked.map_or_else(|| "#000000".to_string(), |t| t.border_colour.clone()),
            _ => layout.background.clone(),
        };
        let index = e.selected;
        let (w, h) = self.size();
        let mut props = json!({
            "canvas-width": w,
            "canvas-height": h,
            "layout-items": names,
            "layout-current": current,
            "layout-enabled": !names.is_empty(),
            "saved": !matched.is_empty(),
            "starts-enabled": !streams.is_empty(),
            "tiles": tiles,
            "selected": index,
            "selected-tile": picked.map_or_else(|| json!({"x": 0.0, "y": 0.0, "w": 0.0, "h": 0.0, "missing": false, "name": "", "gone": "", "hover": false}), |t| json!({
                "x": t.x, "y": t.y, "w": t.w, "h": t.h, "missing": false, "gone": "", "hover": false,
                "name": if t.name.is_empty() { t.source_id.clone() } else { t.name.clone() }})),
            "handles": handles,
            "guide-xs": e.guides.0,
            "guide-ys": e.guides.1,
            "mode": e.mode,
            "cursor": e.cursor,
        });
        let side = json!({
            "streams": rows,
            "few-streams": streams.len() < 2,
            "tile-shown": picked.is_some(),
            "zoom": picked.map_or(100.0, |t| (t.zoom * 100.0).round()),
            "zoom-max": self.max_zoom * 100.0,
            "zoom-text": picked.map_or_else(String::new, |t| format!("{} %", (t.zoom * 100.0).round())),
            "border": picked.map_or(0, |t| t.border),
            "border-max": MAX_BORDER,
            "border-text": picked.map_or_else(String::new, |t| format!("{} px", t.border)),
            "border-colour": picked.map_or_else(|| "#000000".to_string(), |t| t.border_colour.clone()),
            "border-swatch": picked.map_or_else(|| "#000000".to_string(), |t| t.border_colour.clone()),
            "radius": picked.map_or(0, |t| t.radius),
            "radius-max": MAX_RADIUS,
            "radius-text": picked.map_or_else(String::new, |t| format!("{} px", t.radius)),
            "fit-enabled": picked.is_some_and(|t| live.contains(&t.source_id.as_str())),
            "pos-x-text": picked.map_or_else(String::new, |t| format!("{}", (t.x * w as f64).round() as i64)),
            "pos-y-text": picked.map_or_else(String::new, |t| format!("{}", (t.y * h as f64).round() as i64)),
            "size-w-text": picked.map_or_else(String::new, |t| format!("{}", (t.w * w as f64).round() as i64)),
            "crop-l-text": picked.map_or_else(String::new, |t| format!("{} %", (t.crop_l * 100.0).round() as i64)),
            "crop-t-text": picked.map_or_else(String::new, |t| format!("{} %", (t.crop_t * 100.0).round() as i64)),
            "crop-r-text": picked.map_or_else(String::new, |t| format!("{} %", (t.crop_r * 100.0).round() as i64)),
            "crop-b-text": picked.map_or_else(String::new, |t| format!("{} %", (t.crop_b * 100.0).round() as i64)),
            "crop-any": picked.is_some_and(|t| t.crop_l + t.crop_t + t.crop_r + t.crop_b > 0.0),
            "size-h-text": picked.map_or_else(String::new, |t| format!("{}", (t.h * h as f64).round() as i64)),
            "swap-items": layout.tiles.iter().enumerate().filter(|(i, _)| *i as i64 != index).map(|(_, t)| if t.name.is_empty() { t.source_id.clone() } else { t.name.clone() }).collect::<Vec<_>>(),
            "tile-locked": picked.is_some_and(|t| t.locked),
            "layer-count": layout.tiles.len(),
            "swap-enabled": picked.is_some() && layout.tiles.len() >= 2,
            "back-enabled": index > 0,
            "front-enabled": index >= 0 && index < layout.tiles.len() as i64 - 1,
            "background-code": layout.background,
            "background-swatch": layout.background,
            "picture-name": picture_name,
            "has-picture": !layout.picture.is_empty(),
            "colour-target": e.colour,
            "channels": hex_channels(&colour_now),
            "asking-name": e.asking.is_some(),
            "ask-title": match e.asking.as_ref().map(|a| &a.0) { Some(NameFor::Rename(_)) => tr("Rename layout"), _ => tr("Save layout") },
            "ask-start": e.asking.as_ref().map_or("", |a| a.1.as_str()),
            "confirming": e.replacing.is_some(),
            "confirm-text": e.replacing.as_ref().map_or_else(String::new, |r| tr("There's already a layout called \"{name}\". Replace it?").field("name", &r.1)),
        });
        if let (Value::Object(all), Value::Object(side)) = (&mut props, side) {
            all.extend(side);
            // outside the json! above, which is at its recursion limit
            all.insert("tile-cut-out".into(), json!(picked.is_some_and(|t| t.cut_out)));
        }
        props
    }

    pub(super) fn show_editor(&self, host: &mut dyn Host) {
        if self.editor.is_some() {
            host.set_ui_props(WINDOW, &self.editor_props());
        }
    }

    /// A callback of the editor's window.
    pub(super) fn editor_ui(&mut self, host: &mut dyn Host, event: &UiEvent) {
        let number = |i: usize| event.args.get(i).and_then(Value::as_f64).unwrap_or(0.0);
        let flag = |i: usize| event.args.get(i).and_then(Value::as_bool).unwrap_or(false);
        let text = |i: usize| event.args.get(i).and_then(Value::as_str).unwrap_or_default();
        match event.callback.as_str() {
            "closed" => {
                self.editor = None;
                host.cancel_schedule(T_PREVIEW);
                return;
            }
            "close-clicked" => return host.close_window(WINDOW),
            // kind, x, y, width, height, right button, ctrl, shift, alt
            "pointer" => {
                if let Some(e) = self.editor.as_mut() {
                    e.area = (number(3).max(1.0), number(4).max(1.0));
                }
                let pos = (number(1), number(2));
                match text(0) {
                    "down" => self.pointer_down(pos, flag(5)),
                    "move" => self.pointer_move(host, pos, Mods { ctrl: flag(6), shift: flag(7), alt: flag(8) }),
                    "up" => self.pointer_up(),
                    _ => {
                        if let Some(e) = self.editor.as_mut() {
                            e.hover = (-1, String::new());
                            e.cursor = "";
                        }
                    }
                }
            }
            "wheel" => {
                if let Some(e) = self.editor.as_mut() {
                    e.area = (number(2).max(1.0), number(3).max(1.0));
                }
                self.wheel(host, (number(0), number(1)), number(4) / 120.0);
            }
            "key" => self.nudge(host, text(0), flag(1)),
            "box-committed" => {
                if let Some(px) = text(1).trim().trim_end_matches("px").trim().parse::<f64>().ok().filter(|v| v.is_finite()) {
                    self.set_box(host, text(0), px, false);
                }
            }
            "box-step" => self.set_box(host, text(0), number(1), true),
            "crop-committed" => {
                if let Some(pct) = text(1).trim().trim_end_matches('%').trim().parse::<f64>().ok().filter(|v| v.is_finite()) {
                    self.set_crop(host, text(0), pct, false);
                }
            }
            "crop-step" => self.set_crop(host, text(0), number(1), true),
            "crop-reset" => self.set_crop(host, "reset", 0.0, false),
            "swap-picked" => self.swap_picked(host, number(0) as i64),
            "stream-picked" | "stream-locked" => {
                let Some((sid, ..)) = self.stream_entries().get(number(0) as usize).cloned() else { return };
                let Some(at) = self.layout.tiles.iter().position(|t| t.source_id == sid) else { return };
                if event.callback == "stream-picked" {
                    self.select(at as i64);
                } else {
                    self.replace_tile(host, at as i64, Tile { locked: flag(1), ..self.layout.tiles[at].clone() });
                }
            }
            "stream-step" => {
                // The pick moves to the next stream that has a tile, up or down the list
                let rows: Vec<usize> = self.stream_entries().into_iter().filter_map(|(sid, ..)| self.layout.tiles.iter().position(|t| t.source_id == sid)).collect();
                let now = rows.iter().position(|at| *at as i64 == self.editor.as_ref().map_or(-1, |e| e.selected));
                let to = match (now, number(0) < 0.0) {
                    (Some(i), true) => i.checked_sub(1),
                    (Some(i), false) => Some(i + 1),
                    (None, true) => rows.len().checked_sub(1),
                    (None, false) => Some(0),
                };
                if let Some(at) = to.and_then(|i| rows.get(i)) {
                    self.select(*at as i64);
                }
            }
            "stream-moved" => self.move_layer(host, number(0) as usize, number(1) as usize),
            "start-from" => self.start_from(host, number(0) as i64),
            "layout-picked" => {
                if let Some(name) = self.names().get(number(0) as usize).cloned() {
                    self.forget_hidden();
                    self.select(-1);
                    self.pick(host, &name);
                }
            }
            "save-clicked" => self.ask_save(),
            "rename-clicked" => self.ask_rename(),
            "delete-clicked" => self.delete_shown(host),
            "name-entered" => self.name_entered(host, text(0)),
            "name-cancelled" => {
                if let Some(e) = self.editor.as_mut() {
                    e.asking = None;
                }
            }
            "replace-answered" => {
                let replacing = self.editor.as_mut().and_then(|e| e.replacing.take());
                if let (Some((purpose, name)), true) = (replacing, flag(0)) {
                    self.apply_name(host, purpose, &name);
                }
            }
            "stream-toggled" => {
                if let Some((sid, name, _)) = self.stream_entries().get(number(0) as usize).cloned() {
                    self.put(host, &sid, &name, flag(1));
                }
            }
            "zoom-moved" => {
                let zoom = (number(0) / 100.0).min(self.max_zoom);
                self.edit_picked(host, |t| t.zoom = zoom);
            }
            "border-moved" => {
                let border = number(0).round() as i64;
                self.edit_picked(host, |t| t.border = border);
            }
            "radius-moved" => {
                let radius = number(0).round() as i64;
                self.edit_picked(host, |t| t.radius = radius);
            }
            "cut-out-toggled" => {
                let cut_out = flag(0);
                self.edit_picked(host, |t| t.cut_out = cut_out);
            }
            "fit-clicked" => self.fit_shape(host),
            "restack" => self.restack(host, number(0) as i64),
            "colour-clicked" => {
                let target = if text(0) == "border" { "border" } else { "background" };
                if let Some(e) = self.editor.as_mut() {
                    e.colour = if e.colour == target { "" } else { target };
                }
            }
            "channel-moved" => self.colour_changed(host, number(0) as usize, number(1).round().clamp(0.0, 255.0) as u8),
            "picture-clicked" => self.ask_picture(host),
            "no-picture" => self.set_layout(host, Layout { picture: String::new(), ..self.layout.clone() }),
            _ => return,
        }
        if self.editor.as_ref().is_some_and(|e| usize::try_from(e.selected).is_ok_and(|i| i >= self.layout.tiles.len())) {
            self.select(-1);
        }
        self.show_editor(host);
    }

    /// The live picture, about 15 times a second while the window is open.
    pub(super) fn editor_timer(&mut self, host: &mut dyn Host) {
        if self.editor.is_none() {
            return;
        }
        let frames = self.frames_now();
        let (full_w, full_h) = self.size();
        let layout = self.shown_layout();
        let Some(e) = self.editor.as_mut() else { return };
        let width = (e.area.0.round() as u32).clamp(16, PREVIEW_MAX_W);
        let height = (((width as f64) * full_h as f64 / full_w as f64).round() as u32).max(9);
        let size = (width, height);
        if e.prepared.as_ref().is_none_or(|p| p.0 != layout || p.1 != size) {
            e.compositor.prepare(&layout, size);
            e.prepared = Some((layout, size));
        }
        if let Some(picture) = e.compositor.render(&frames, PixelFormat::Rgba) {
            host.set_ui_image(WINDOW, "picture", &picture);
        }
        host.schedule(PREVIEW_MS, T_PREVIEW);
    }

    pub(super) const EDITOR_TIMER: &'static str = T_PREVIEW;
}
