//! Several streams in one picture (telescope/composite.py): the layout, the maths the editor moves tiles with, and keeping
//! layouts in settings. Drawing them is compositor.rs. Boxes are fractions of the picture (0..1) and borders and corners are
//! pixels of a 1080-line picture, so a layout looks the same at any canvas size.

use serde_json::{json, Map, Value};

/// As many as Telescope runs streams.
pub const MAX_TILES: usize = 4;
pub const MAX_LAYOUTS: usize = 30;
pub const MAX_NAME: usize = 40;
pub const REFERENCE_HEIGHT: f64 = 1080.0;
pub const MAX_BORDER: f64 = 40.0;
pub const MAX_RADIUS: f64 = 200.0;
pub const MAX_ZOOM: f64 = 4.0;
/// A box can't get smaller than this share of the picture.
pub const MIN_SIZE: f64 = 0.05;
/// Cropping always leaves at least this share of the stream's frame each way.
pub const MIN_SHOWN: f64 = 0.05;
/// A stream whose newest frame is older than this many seconds (it dropped) is left out until it's back.
pub const STALE_S: f64 = 3.0;
/// How close an edge has to come to another to snap to it, as a share of the picture.
pub const SNAP: f64 = 0.012;
pub const DEFAULT_BACKGROUND: &str = "#15151a";

/// One stream's place in the picture.
#[derive(Clone, Debug, PartialEq)]
pub struct Tile {
    pub source_id: String,
    /// Shown in the editor while that stream isn't running.
    pub name: String,
    /// The box, as fractions of the picture.
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    /// How far in, and where the middle of what's shown is in the cropped frame (0..1).
    pub zoom: f64,
    pub cx: f64,
    pub cy: f64,
    pub border: i64,
    pub border_colour: String,
    pub radius: i64,
    /// How much is cut off each side of the stream's frame, as fractions of it.
    pub crop_l: f64,
    pub crop_t: f64,
    pub crop_r: f64,
    pub crop_b: f64,
    /// Fill the box whatever its shape; otherwise the middle of the crop is cut to the box's shape.
    pub stretch: bool,
}

impl Tile {
    /// Tile(source_id) with every other field at its default.
    pub fn new(source_id: &str) -> Tile {
        Tile {
            source_id: source_id.into(), name: String::new(), x: 0.0, y: 0.0, w: 1.0, h: 1.0, zoom: 1.0, cx: 0.5, cy: 0.5, border: 0,
            border_colour: "#ffffff".into(), radius: 0, crop_l: 0.0, crop_t: 0.0, crop_r: 0.0, crop_b: 0.0, stretch: false,
        }
    }

    /// Tile(source_id, name, x, y, w, h).
    pub fn boxed(source_id: &str, name: &str, x: f64, y: f64, w: f64, h: f64) -> Tile {
        Tile { name: name.into(), x, y, w, h, ..Tile::new(source_id) }
    }

    fn with_box(&self, (x, y, w, h): (f64, f64, f64, f64)) -> Tile {
        Tile { x, y, w, h, ..self.clone() }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    pub name: String,
    /// Back to front.
    pub tiles: Vec<Tile>,
    pub background: String,
    /// A picture file behind the tiles instead of the colour.
    pub picture: String,
}

impl Default for Layout {
    fn default() -> Self {
        Layout { name: String::new(), tiles: Vec::new(), background: DEFAULT_BACKGROUND.into(), picture: String::new() }
    }
}

impl Layout {
    pub fn of(tiles: Vec<Tile>) -> Layout {
        Layout { tiles, ..Layout::default() }
    }

    pub fn named(name: &str, tiles: Vec<Tile>) -> Layout {
        Layout { name: name.into(), tiles, ..Layout::default() }
    }
}

// ── Saving and loading ────────────────────────────────────────────────────

/// A JSON number Python's isinstance(value, (int, float)) takes (not a bool), clamped; the default otherwise.
fn number(value: Option<&Value>, default: f64, lo: f64, hi: f64) -> f64 {
    match value.and_then(Value::as_f64) {
        Some(v) if v.is_finite() => hi.min(lo.max(v)),
        _ => default,
    }
}

const HEX: &str = "0123456789abcdef";

fn colour(value: Option<&Value>, default: &str) -> String {
    match value.and_then(Value::as_str) {
        Some(s) if s.chars().count() == 7 && s.starts_with('#') && s[1..].to_lowercase().chars().all(|c| HEX.contains(c)) => s.to_lowercase(),
        _ => default.into(),
    }
}

/// Format and private-use characters, which Python's isprintable() leaves out with the controls and every space but ' '.
fn printable(c: char) -> bool {
    if c == ' ' {
        return true;
    }
    !(c.is_whitespace() || c.is_control() || matches!(c as u32, 0xAD | 0x600..=0x605 | 0x61C | 0x6DD | 0x70F | 0x8E2 | 0x180E
        | 0x200B..=0x200F | 0x2028 | 0x2029 | 0x202A..=0x202E | 0x2060..=0x2064 | 0x2066..=0x206F | 0xFEFF | 0xFFF9..=0xFFFB
        | 0x110BD | 0x110CD | 0x13430..=0x1343F | 0x1BCA0..=0x1BCA3 | 0x1D173..=0x1D17A | 0xE0001 | 0xE0020..=0xE007F
        | 0xE000..=0xF8FF | 0xF0000..=0x10FFFF | 0xD800..=0xDFFF))
}

fn text(value: Option<&Value>, limit: usize) -> String {
    let Some(s) = value.and_then(Value::as_str) else { return String::new() };
    let kept: String = s.chars().filter(|&c| printable(c) && c != '<' && c != '>').take(limit).collect();
    kept.trim_matches(' ').to_string()
}

/// A layout's name as it's saved: printable, no < or >, at most MAX_NAME long.
pub fn clean_name(name: &str) -> String {
    text(Some(&Value::String(name.into())), MAX_NAME)
}

pub fn clean_tile(raw: &Value) -> Option<Tile> {
    let source_id = raw.as_object()?.get("source_id")?.as_str()?;
    let get = |k: &str| raw.get(k);
    let w = number(get("w"), 1.0, MIN_SIZE, 1.0);
    let h = number(get("h"), 1.0, MIN_SIZE, 1.0);
    let crop_l = number(get("crop_l"), 0.0, 0.0, 1.0 - MIN_SHOWN);
    let crop_t = number(get("crop_t"), 0.0, 0.0, 1.0 - MIN_SHOWN);
    Some(Tile {
        source_id: source_id.chars().take(200).collect(),
        name: text(get("name"), MAX_NAME),
        x: number(get("x"), 0.0, 0.0, 1.0 - w),
        y: number(get("y"), 0.0, 0.0, 1.0 - h),
        w,
        h,
        zoom: number(get("zoom"), 1.0, 1.0, MAX_ZOOM),
        cx: number(get("cx"), 0.5, 0.0, 1.0),
        cy: number(get("cy"), 0.5, 0.0, 1.0),
        border: number(get("border"), 0.0, 0.0, MAX_BORDER) as i64,
        border_colour: colour(get("border_colour"), "#ffffff"),
        radius: number(get("radius"), 0.0, 0.0, MAX_RADIUS) as i64,
        crop_l,
        crop_t,
        crop_r: number(get("crop_r"), 0.0, 0.0, 1.0 - MIN_SHOWN - crop_l),
        crop_b: number(get("crop_b"), 0.0, 0.0, 1.0 - MIN_SHOWN - crop_t),
        stretch: get("stretch") == Some(&Value::Bool(true)),
    })
}

/// A layout from saved settings, with anything broken or out of range fixed; None for something that isn't one.
pub fn clean_layout(raw: &Value) -> Option<Layout> {
    let obj = raw.as_object()?;
    let mut tiles: Vec<Tile> = Vec::new();
    for entry in obj.get("tiles").and_then(Value::as_array).into_iter().flatten() {
        if let Some(tile) = clean_tile(entry) {
            if !tiles.iter().any(|t| t.source_id == tile.source_id) && tiles.len() < MAX_TILES {
                tiles.push(tile);
            }
        }
    }
    let picture = obj.get("picture").and_then(Value::as_str).filter(|p| p.chars().count() < 4096).unwrap_or("");
    Some(Layout {
        name: text(obj.get("name"), MAX_NAME),
        tiles,
        background: colour(obj.get("background"), DEFAULT_BACKGROUND),
        picture: picture.into(),
    })
}

/// Saved layouts with a name each, the first of each name kept, at most MAX_LAYOUTS.
pub fn clean_layouts(raw: &Value) -> Vec<Layout> {
    let mut out: Vec<Layout> = Vec::new();
    for entry in raw.as_array().into_iter().flatten() {
        if let Some(layout) = clean_layout(entry) {
            if !layout.name.is_empty() && !out.iter().any(|l| l.name == layout.name) && out.len() < MAX_LAYOUTS {
                out.push(layout);
            }
        }
    }
    out
}

pub fn tile_dict(t: &Tile) -> Value {
    let mut m = Map::new();
    m.insert("source_id".into(), json!(t.source_id));
    m.insert("name".into(), json!(t.name));
    for (k, v) in [("x", t.x), ("y", t.y), ("w", t.w), ("h", t.h), ("zoom", t.zoom), ("cx", t.cx), ("cy", t.cy)] {
        m.insert(k.into(), json!(v));
    }
    m.insert("border".into(), json!(t.border));
    m.insert("border_colour".into(), json!(t.border_colour));
    m.insert("radius".into(), json!(t.radius));
    for (k, v) in [("crop_l", t.crop_l), ("crop_t", t.crop_t), ("crop_r", t.crop_r), ("crop_b", t.crop_b)] {
        m.insert(k.into(), json!(v));
    }
    m.insert("stretch".into(), json!(t.stretch));
    Value::Object(m)
}

pub fn layout_dict(layout: &Layout) -> Value {
    json!({"name": layout.name, "background": layout.background, "picture": layout.picture,
           "tiles": layout.tiles.iter().map(tile_dict).collect::<Vec<_>>()})
}

/// Whether two layouts look the same, names aside.
pub fn same_arrangement(a: &Layout, b: &Layout) -> bool {
    a.tiles == b.tiles && a.background == b.background && a.picture == b.picture
}

// ── Where things go ───────────────────────────────────────────────────────

/// Python's round(): halves go to the even neighbour.
pub fn pround(v: f64) -> i64 {
    v.round_ties_even() as i64
}

/// (x0, y0, x1, y1) of the tile's box in a width x height picture, at least a pixel each way.
pub fn box_px(tile: &Tile, width: i64, height: i64) -> (i64, i64, i64, i64) {
    let (x0, y0) = (pround(tile.x * width as f64), pround(tile.y * height as f64));
    let (x1, y1) = (pround((tile.x + tile.w) * width as f64), pround((tile.y + tile.h) * height as f64));
    let (x0, y0) = (x0.max(0).min(width - 1), y0.max(0).min(height - 1));
    (x0, y0, width.min((x0 + 1).max(x1)), height.min((y0 + 1).max(y1)))
}

/// (x0, y0, x1, y1) of the part of a src_w x src_h frame that fills a box_w x box_h box, unrounded. The crop is cut off
/// first; then, unless the tile stretches, the biggest piece of what's left with the box's shape; the zoom makes that
/// smaller around the tile's centre without leaving the crop.
pub fn shown(tile: &Tile, src_w: f64, src_h: f64, box_w: f64, box_h: f64) -> (f64, f64, f64, f64) {
    let (rx, ry) = (tile.crop_l * src_w, tile.crop_t * src_h);
    let rw = 1e-6f64.max((1.0 - tile.crop_l - tile.crop_r) * src_w);
    let rh = 1e-6f64.max((1.0 - tile.crop_t - tile.crop_b) * src_h);
    let (w, h) = if tile.stretch {
        (rw, rh)
    } else {
        let aspect = box_w / 1e-6f64.max(box_h);
        if rw / rh > aspect { (rh * aspect, rh) } else { (rw, rw / aspect) }
    };
    let (w, h) = (w / tile.zoom, h / tile.zoom);
    let x0 = rx + (0.0f64.max(tile.cx * rw - w / 2.0)).min(rw - w);
    let y0 = ry + (0.0f64.max(tile.cy * rh - h / 2.0)).min(rh - h);
    (x0, y0, x0 + w, y0 + h)
}

/// (x0, y0, x1, y1) of shown() in whole pixels, at least one each way and inside the frame.
pub fn crop_px(tile: &Tile, src_w: i64, src_h: i64, box_w: i64, box_h: i64) -> (i64, i64, i64, i64) {
    let (x0, y0, x1, y1) = shown(tile, src_w as f64, src_h as f64, box_w as f64, box_h as f64);
    let (w, h) = (1.max(pround(x1 - x0)), 1.max(pround(y1 - y0)));
    let (x0, y0) = (0.max(pround(x0)).min(src_w - 1), 0.max(pround(y0)).min(src_h - 1));
    (x0, y0, src_w.min(x0 + w), src_h.min(y0 + h))
}

/// A border or corner size given for a 1080-line picture, in pixels of one height lines tall.
pub fn scaled_size(reference: i64, height: i64) -> i64 {
    if reference == 0 { 0 } else { pround(reference as f64 * height as f64 / REFERENCE_HEIGHT) }
}

/// (x, y, w, h) of the biggest box with a stream's shape (aspect, width over height in pixels) centred in cell, where the
/// picture is canvas_aspect wide for each unit tall.
pub fn fit_box(aspect: f64, cell: (f64, f64, f64, f64), canvas_aspect: f64) -> (f64, f64, f64, f64) {
    let (cx, cy, cw, ch) = cell;
    let mut w = cw;
    let mut h = w * canvas_aspect / aspect;
    if h > ch {
        h = ch;
        w = h * aspect / canvas_aspect;
    }
    (cx + (cw - w) / 2.0, cy + (ch - h) / 2.0, w, h)
}

/// A running stream as the starting layouts see it: (source id, name, aspect).
pub type Stream = (String, String, f64);

/// Tiles for streams next to each other, each as big as its shape allows.
pub fn side_by_side(streams: &[Stream], canvas_aspect: f64) -> Vec<Tile> {
    side_by_side_gap(streams, canvas_aspect, 0.02)
}

pub fn side_by_side_gap(streams: &[Stream], canvas_aspect: f64, gap: f64) -> Vec<Tile> {
    let n = streams.len();
    if n == 0 {
        return Vec::new();
    }
    let cell_w = (1.0 - gap * (n as f64 + 1.0)) / n as f64;
    streams.iter().take(MAX_TILES).enumerate().map(|(i, (sid, name, aspect))| {
        let b = fit_box(*aspect, (gap + i as f64 * (cell_w + gap), gap, cell_w, 1.0 - 2.0 * gap), canvas_aspect);
        Tile { radius: 16, ..Tile::boxed(sid, name, b.0, b.1, b.2, b.3) }
    }).collect()
}

/// The first stream filling the picture and the others small in the bottom corner, the newest on top. Upright phones in a
/// short picture are made smaller so the stack still fits.
pub fn big_and_small(streams: &[Stream], canvas_aspect: f64) -> Vec<Tile> {
    let Some((sid, name, _)) = streams.first() else { return Vec::new() };
    let mut tiles = vec![Tile::boxed(sid, name, 0.0, 0.0, 1.0, 1.0)];
    let (margin, width) = (0.03, 0.26);
    // the same gap up and down as across, in fractions of the height
    let gap = margin * canvas_aspect;
    let rest: Vec<&Stream> = streams.iter().skip(1).take(MAX_TILES - 1).collect();
    let sizes: Vec<f64> = rest.iter().map(|(_, _, aspect)| {
        let h = width * canvas_aspect / aspect;
        if h > 0.45 { 0.45 } else { h }
    }).collect();
    let room = 1.0 - gap * (sizes.len() as f64 + 1.0);
    let shrink = if sizes.is_empty() { 1.0 } else { 1.0f64.min(room / sizes.iter().sum::<f64>()) };
    let mut bottom = 1.0 - gap;
    for ((sid, name, aspect), h) in rest.into_iter().zip(sizes) {
        let h = h * shrink;
        let w = h * aspect / canvas_aspect;
        let y = bottom - h;
        tiles.push(Tile { border: 4, radius: 24, ..Tile::boxed(sid, name, 1.0 - margin - w, 0.0f64.max(y), w, h) });
        bottom = y - gap;
    }
    tiles
}

/// Two by two (or two side by side for two streams), each as big as its shape allows in its cell.
pub fn grid(streams: &[Stream], canvas_aspect: f64) -> Vec<Tile> {
    if streams.len() <= 2 {
        return side_by_side(streams, canvas_aspect);
    }
    let gap = 0.015;
    let (cell_w, cell_h) = ((1.0 - 3.0 * gap) / 2.0, (1.0 - 3.0 * gap) / 2.0);
    streams.iter().take(MAX_TILES).enumerate().map(|(i, (sid, name, aspect))| {
        let cell = (gap + (i % 2) as f64 * (cell_w + gap), gap + (i / 2) as f64 * (cell_h + gap), cell_w, cell_h);
        let b = fit_box(*aspect, cell, canvas_aspect);
        Tile { radius: 12, ..Tile::boxed(sid, name, b.0, b.1, b.2, b.3) }
    }).collect()
}

/// tile with its box's top left at (x, y), kept inside the picture.
pub fn moved(tile: &Tile, x: f64, y: f64) -> Tile {
    Tile { x: 0.0f64.max(x).min(1.0 - tile.w), y: 0.0f64.max(y).min(1.0 - tile.h), ..tile.clone() }
}

/// box (x, y, w, h) with the edges handle names ("l", "r", "t", "b" or two of them, like "tl") dragged by dx, dy, kept inside
/// the picture and no smaller than MIN_SIZE. aspect: keep w / h at that (the picture's own units).
pub fn resized(bx: (f64, f64, f64, f64), handle: &str, dx: f64, dy: f64, aspect: Option<f64>) -> (f64, f64, f64, f64) {
    let (mut x, mut y, w, h) = bx;
    let (mut x1, mut y1) = (x + w, y + h);
    if handle.contains('l') {
        x = 0.0f64.max(x + dx).min(x1 - MIN_SIZE);
    }
    if handle.contains('r') {
        x1 = 1.0f64.min(x1 + dx).max(x + MIN_SIZE);
    }
    if handle.contains('t') {
        y = 0.0f64.max(y + dy).min(y1 - MIN_SIZE);
    }
    if handle.contains('b') {
        y1 = 1.0f64.min(y1 + dy).max(y + MIN_SIZE);
    }
    let (mut w, mut h) = (x1 - x, y1 - y);
    if let Some(aspect) = aspect.filter(|a| *a != 0.0) {
        if handle == "t" || handle == "b" || (handle != "l" && handle != "r" && h * aspect > w) {
            // a corner follows whichever way it was dragged further
            w = h * aspect;
        } else {
            h = w / aspect;
        }
        // what doesn't fit shrinks both ways, from the corner that stays put
        let (right, bottom) = (!handle.contains('l'), !handle.contains('t'));
        let room_w = if right { 1.0 - bx.0 } else { bx.0 + bx.2 };
        let room_h = if bottom { 1.0 - bx.1 } else { bx.1 + bx.3 };
        let shrink = 1.0f64.min(room_w / w).min(room_h / h);
        (w, h) = (w * shrink, h * shrink);
        x = if right { bx.0 } else { bx.0 + bx.2 - w };
        y = if bottom { bx.1 } else { bx.1 + bx.3 - h };
    }
    (x, y, w, h)
}

/// box (x, y, w, h) made bigger or smaller by dragging handle, keeping its shape. A corner keeps the opposite corner where it
/// is; a side keeps the opposite side and stays centred the other way.
pub fn scaled(bx: (f64, f64, f64, f64), handle: &str, dx: f64, dy: f64) -> (f64, f64, f64, f64) {
    let (x, y, w, h) = bx;
    if handle.chars().count() == 2 {
        return resized(bx, handle, dx, dy, Some(w / h));
    }
    let (_nx, _ny, nw, nh) = resized(bx, handle, dx, dy, None);
    let across = handle == "l" || handle == "r";
    let s = if across { nw / w } else { nh / h };
    // the side that follows along
    let other = if across { h } else { w };
    let s = s.max(MIN_SIZE / w.min(h)).min(1.0 / other);
    let (nw, nh) = (w * s, h * s);
    if across {
        let nx = if handle == "r" { x } else { x + w - nw };
        let nx = 0.0f64.max(nx).min(1.0 - nw);
        return (nx, 0.0f64.max(y + (h - nh) / 2.0).min(1.0 - nh), nw, nh);
    }
    let ny = if handle == "b" { y } else { y + h - nh };
    let ny = 0.0f64.max(ny).min(1.0 - nh);
    (0.0f64.max(x + (w - nw) / 2.0).min(1.0 - nw), ny, nw, nh)
}

/// tile with what it shows right now written into its crop, at zoom 1: it looks the same, and its crop can then be moved
/// side by side or stretched. src_aspect is the stream's width over height, canvas_aspect the picture's.
pub fn pinned(tile: &Tile, src_aspect: f64, canvas_aspect: f64) -> Tile {
    let (x0, y0, x1, y1) = shown(tile, src_aspect, 1.0, tile.w * canvas_aspect, tile.h);
    Tile { crop_l: x0 / src_aspect, crop_t: y0, crop_r: 1.0 - x1 / src_aspect, crop_b: 1.0 - y1, zoom: 1.0, cx: 0.5, cy: 0.5, ..tile.clone() }
}

/// tile with the sides handle names dragged by dx, dy, cutting off (or bringing back) that side of the stream while the
/// rest of it stays where it is on the picture. The tile has to be pinned() first.
pub fn cropped(tile: &Tile, handle: &str, dx: f64, dy: f64) -> Tile {
    let (x, y, w, h) = (tile.x, tile.y, tile.w, tile.h);
    let (x1, y1) = (x + w, y + h);
    let (mut nx, mut ny, nw, nh) = resized((x, y, w, h), handle, dx, dy, None);
    let (mut nx1, mut ny1) = (nx + nw, ny + nh);
    let (mut l, mut t, mut r, mut b) = (tile.crop_l, tile.crop_t, tile.crop_r, tile.crop_b);
    // picture per share of the stream's width
    let per_u = w / 1e-6f64.max(1.0 - l - r);
    let per_v = h / 1e-6f64.max(1.0 - t - b);
    if handle.contains('l') {
        let l2 = 0.0f64.max(l + (nx - x) / per_u).min(1.0 - r - MIN_SHOWN);
        (nx, l) = (x + (l2 - l) * per_u, l2);
    }
    if handle.contains('r') {
        let r2 = 0.0f64.max(r - (nx1 - x1) / per_u).min(1.0 - l - MIN_SHOWN);
        (nx1, r) = (x1 - (r2 - r) * per_u, r2);
    }
    if handle.contains('t') {
        let t2 = 0.0f64.max(t + (ny - y) / per_v).min(1.0 - b - MIN_SHOWN);
        (ny, t) = (y + (t2 - t) * per_v, t2);
    }
    if handle.contains('b') {
        let b2 = 0.0f64.max(b - (ny1 - y1) / per_v).min(1.0 - t - MIN_SHOWN);
        (ny1, b) = (y1 - (b2 - b) * per_v, b2);
    }
    Tile { x: nx, y: ny, w: nx1 - nx, h: ny1 - ny, crop_l: l, crop_t: t, crop_r: r, crop_b: b, ..tile.clone() }
}

/// tile after dragging handle by dx, dy, the way OBS does it. how is "scale" (keeps the shape), "stretch" (the box takes any
/// shape and the stream is squashed to fill it) or "crop" (cuts off the side being dragged). Stretching and cropping need the
/// stream's shape (src_aspect); without one they only change the box, as for a stream that's off.
pub fn dragged(tile: &Tile, handle: &str, dx: f64, dy: f64, how: &str, canvas_aspect: f64, src_aspect: Option<f64>) -> Tile {
    let bx = (tile.x, tile.y, tile.w, tile.h);
    if how == "scale" {
        return tile.with_box(scaled(bx, handle, dx, dy));
    }
    let Some(src_aspect) = src_aspect else { return tile.with_box(resized(bx, handle, dx, dy, None)) };
    let tile = pinned(tile, src_aspect, canvas_aspect);
    if how == "crop" {
        return cropped(&tile, handle, dx, dy);
    }
    Tile { stretch: true, ..tile.with_box(resized(bx, handle, dx, dy, None)) }
}

/// tile with its box the stream's own shape inside the old one, and no crop, stretch or zoom.
pub fn fitted(tile: &Tile, src_aspect: f64, canvas_aspect: f64) -> Tile {
    let b = fit_box(src_aspect, (tile.x, tile.y, tile.w, tile.h), canvas_aspect);
    Tile { zoom: 1.0, cx: 0.5, cy: 0.5, crop_l: 0.0, crop_t: 0.0, crop_r: 0.0, crop_b: 0.0, stretch: false, ..tile.with_box(b) }
}

/// ([x], [y]) an edge of the tile at index skip snaps to: the picture's edges and centre and the other tiles'.
pub fn snap_lines(tiles: &[Tile], skip: i64) -> (Vec<f64>, Vec<f64>) {
    let (mut xs, mut ys) = (vec![0.0, 0.5, 1.0], vec![0.0, 0.5, 1.0]);
    for (i, t) in tiles.iter().enumerate() {
        if i as i64 != skip {
            xs.extend([t.x, t.x + t.w / 2.0, t.x + t.w]);
            ys.extend([t.y, t.y + t.h / 2.0, t.y + t.h]);
        }
    }
    (xs, ys)
}

/// (how far to move, the line it snapped to or None) for the closest of values to come within reach of a line.
pub fn snap(values: &[f64], lines: &[f64], reach: f64) -> (f64, Option<f64>) {
    let mut best: Option<(f64, f64)> = None;
    for v in values {
        for line in lines {
            let d = line - v;
            if d.abs() <= reach && best.is_none_or(|b| d.abs() < b.0.abs()) {
                best = Some((d, *line));
            }
        }
    }
    best.map_or((0.0, None), |(d, line)| (d, Some(line)))
}

/// tiles with the one at index moved step places towards the front (negative: towards the back).
pub fn raised(tiles: &[Tile], index: i64, step: i64) -> Vec<Tile> {
    if !(0..tiles.len() as i64).contains(&index) {
        return tiles.to_vec();
    }
    let to = 0.max(index + step).min(tiles.len() as i64 - 1) as usize;
    let mut items = tiles.to_vec();
    let item = items.remove(index as usize);
    items.insert(to, item);
    items
}

/// A colour as (B, G, R), black for one that isn't "#rrggbb".
pub fn bgr(c: &str) -> [u8; 3] {
    let c = colour(Some(&Value::String(c.into())), "#000000");
    let byte = |i: usize| u8::from_str_radix(&c[i..i + 2], 16).unwrap_or(0);
    [byte(5), byte(3), byte(1)]
}

#[cfg(test)]
#[path = "composite_tests.rs"]
mod tests;
