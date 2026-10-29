//! The QR codes plugin (plugins/qr_reader.py): a code held up to the camera goes to the clipboard, the Wi-Fi password for a Wi-Fi
//! one. The picture is looked at on a thread of its own a few times a second; a code only counts once it's been read several
//! looks in a row at about the same place, and is outlined on the preview while it does.

use crate::shared::Paired;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use telescope_core::actions;
use telescope_core::event::Event;
use telescope_core::frame::{CpuStep, Frame, FrameContext, FrameStage, FrameStep, PixelFormat};
use telescope_core::host::{Host, Poster, PreviewLayer, Shape};
use telescope_core::i18n::{n_, tr};
use telescope_core::plugin::{ConfigRejected, Plugin, PluginInfo};
use telescope_core::time::monotonic_ns;
use telescope_core::ui::{ActionSpec, CardSpec, SlintSource, UiEvent, UiSpec};

pub const NAME: &str = "qr_reader";
const UI: &str = concat!(include_str!("../ui/parts.slint"), "\n", include_str!("../ui/qr_reader.slint"));

/// A frame is looked at this often...
const EVERY: Duration = Duration::from_millis(300);
/// ...and this often while a code just read waits to be confirmed.
const PENDING: Duration = Duration::from_millis(100);
/// A code counts once it's read this many looks in a row at about the same place.
pub const STEADY: usize = 3;
/// A confirmed code stays through this many looks that miss it.
pub const GRACE: usize = 1;
/// ...shrunk to this width first, and tried again at half that (a big code close up reads better small).
const WIDTH: f64 = 1280.0;
/// The same code is copied again only after it's been out of view this long.
const AGAIN_S: f64 = 3.0;
/// The outline goes if no look confirms the code for this long (frames stopped coming).
const OUTLINE_MS: u64 = 1000;
/// The clipboard is read back this long after a copy (Wayland ignores one from a window in the background).
const CHECK_MS: u64 = 400;

const T_OUTLINE: &str = "outline";
const T_CHECK: &str = "check";
const POST_FOUND: &str = "found";

const SWITCH_TIP: &str = n_("Look for QR codes in the picture");
const COPY_TIP: &str = n_("Put the last code on the clipboard again");
const HOLD_UP: &str = n_("Hold a QR code up to the camera.");

/// A code's four corners as fractions of the frame, in the order the detector gave them.
pub type Corners = [[f64; 2]; 4];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wifi {
    pub ssid: String,
    pub password: String,
    /// "WPA", "WEP", "nopass" or "" when the code doesn't say.
    pub security: String,
    pub hidden: bool,
}

/// KEY:value pairs of a WIFI: code body, split on unescaped ; (a backslash escapes \ ; , : and "). A key seen twice keeps its first.
fn fields(body: &str) -> Vec<(String, String)> {
    let mut fields: Vec<(String, String)> = Vec::new();
    let mut add = |key: &str, value: &str| {
        let key = key.to_uppercase();
        if !fields.iter().any(|(k, _)| *k == key) {
            fields.push((key, value.to_string()));
        }
    };
    let (mut key, mut value, mut escaped, mut in_key) = (String::new(), String::new(), false, true);
    for ch in body.chars() {
        if escaped {
            if in_key { key.push(ch) } else { value.push(ch) }
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == ':' && in_key {
            in_key = false;
        } else if ch == ';' {
            if !key.is_empty() {
                add(&key, &value);
            }
            (key, value, in_key) = (String::new(), String::new(), true);
        } else if in_key {
            key.push(ch);
        } else {
            value.push(ch);
        }
    }
    if !key.is_empty() && !in_key {
        add(&key, &value);
    }
    fields
}

/// The network in a WIFI:T:WPA;S:name;P:password;; code, or None when text isn't one.
pub fn parse_wifi(text: &str) -> Option<Wifi> {
    if text.chars().take(5).collect::<String>().to_uppercase() != "WIFI:" {
        return None;
    }
    let body: String = text.chars().skip(5).collect();
    let fields = fields(&body);
    let get = |key: &str| fields.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());
    let ssid = get("S").filter(|s| !s.is_empty())?;
    Some(Wifi { ssid, password: get("P").unwrap_or_default(), security: get("T").unwrap_or_default(),
                hidden: get("H").unwrap_or_default().to_lowercase() == "true" })
}

pub fn is_link(text: &str) -> bool {
    let lower = text.to_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

/// Whether a quad in pixels could be a code: four corners, convex, not tiny and not a sliver.
pub fn plausible(points: &[[f64; 2]]) -> bool {
    let Ok(p): Result<[[f64; 2]; 4], _> = points.try_into() else { return false };
    if p.iter().flatten().any(|v| !v.is_finite()) {
        return false;
    }
    let edge = |i: usize| [p[(i + 1) % 4][0] - p[i][0], p[(i + 1) % 4][1] - p[i][1]];
    let edges: Vec<[f64; 2]> = (0..4).map(edge).collect();
    let turns: Vec<f64> = (0..4).map(|i| edges[i][0] * edges[(i + 1) % 4][1] - edges[i][1] * edges[(i + 1) % 4][0]).collect();
    if !(turns.iter().all(|t| *t > 0.0) || turns.iter().all(|t| *t < 0.0)) {
        return false;
    }
    let sides: Vec<f64> = edges.iter().map(|e| e[0].hypot(e[1])).collect();
    let (shortest, longest) = (sides.iter().cloned().fold(f64::INFINITY, f64::min), sides.iter().cloned().fold(0.0, f64::max));
    let area = ((0..4).map(|i| p[i][0] * p[(i + 1) % 4][1]).sum::<f64>() - (0..4).map(|i| p[i][1] * p[(i + 1) % 4][0]).sum::<f64>()).abs() / 2.0;
    shortest >= 16.0 && shortest / longest >= 0.3 && area >= 0.3 * longest * longest
}

// ── Reading a picture ────────────────────────────────────────────────────────

/// cv2.resize(.., INTER_AREA) of a grey picture: each new pixel is the average of the old ones it covers, the ones on its edge by
/// the share of them it covers.
pub fn area_resize(src: &[u8], w: usize, h: usize, dw: usize, dh: usize) -> Vec<u8> {
    if (w, h) == (dw, dh) {
        return src.to_vec();
    }
    // For each new column or row: the old ones it covers and by how much of each
    let spans = |from: usize, to: usize| -> Vec<Vec<(usize, f32)>> {
        let scale = from as f64 / to as f64;
        (0..to).map(|i| {
            let (start, end) = (i as f64 * scale, ((i + 1) as f64 * scale).min(from as f64));
            let (first, last) = (start.floor() as usize, (end.ceil() as usize).min(from));
            (first..last).map(|k| (k, ((end.min(k as f64 + 1.0) - start.max(k as f64)) / scale) as f32)).filter(|(_, wt)| *wt > 0.0).collect()
        }).collect()
    };
    let (cols, rows) = (spans(w, dw), spans(h, dh));
    // Across, then down
    let mut across = vec![0f32; dw * h];
    for y in 0..h {
        for (x, span) in cols.iter().enumerate() {
            across[y * dw + x] = span.iter().map(|(k, wt)| src[y * w + k] as f32 * wt).sum();
        }
    }
    let mut out = vec![0u8; dw * dh];
    for (y, span) in rows.iter().enumerate() {
        for x in 0..dw {
            let v: f32 = span.iter().map(|(k, wt)| across[k * dw + x] * wt).sum();
            out[y * dw + x] = v.round().clamp(0.0, 255.0) as u8;
        }
    }
    out
}

/// The frame as a grey picture the way a viewer sees it (cvtColor BGR2GRAY of what the decoder made): the video's luma stretched to
/// the full range, or the channels weighted as OpenCV weighs them.
pub fn grey_of(frame: &Frame) -> Vec<u8> {
    let (w, h) = (frame.width as usize, frame.height as usize);
    match frame.format {
        PixelFormat::I420 | PixelFormat::Nv12 => frame.data[..w * h].iter().map(|y| ((*y as f32 - 16.0) * 1.164_383).round().clamp(0.0, 255.0) as u8).collect(),
        PixelFormat::Rgba => frame.data.chunks_exact(4).map(|p| ((4899 * p[0] as u32 + 9617 * p[1] as u32 + 1868 * p[2] as u32 + 8192) >> 14) as u8).collect(),
        PixelFormat::Bgra => frame.data.chunks_exact(4).map(|p| ((4899 * p[2] as u32 + 9617 * p[1] as u32 + 1868 * p[0] as u32 + 8192) >> 14) as u8).collect(),
        // A format added later that this can't read: nothing to look at
        _ => Vec::new(),
    }
}

/// (text, corners 0..1) of a QR code in a grey picture, "" for one seen but not read; None for no code. It looks at the picture as it
/// is and at half its size, where a big code close up reads better.
pub fn read_code(grey: &[u8], w: usize, h: usize) -> Option<(String, Corners)> {
    if w == 0 || h == 0 || grey.len() < w * h {
        return None;
    }
    let mut seen = None;
    let half = area_resize(grey, w, h, (w / 2).max(1), (h / 2).max(1));
    for (image, iw, ih) in [(grey, w, h), (half.as_slice(), (w / 2).max(1), (h / 2).max(1))] {
        let mut prepared = rqrr::PreparedImage::prepare_from_greyscale(iw, ih, |x, y| image[y * iw + x]);
        for grid in prepared.detect_grids() {
            // rqrr maps the far corners one module past the code's edge; OpenCV puts all four on it
            let size = rqrr::BitGrid::size(&grid.grid) as f64;
            let first = grid.bounds[0];
            let points: Vec<[f64; 2]> = grid.bounds.iter().enumerate().map(|(i, p)| {
                let pull = if i == 0 { 1.0 } else { size / (size + 1.0) };
                [first.x as f64 + (p.x - first.x) as f64 * pull, first.y as f64 + (p.y - first.y) as f64 * pull]
            }).collect();
            if !plausible(&points) {
                continue;
            }
            let corners: Corners = [0, 1, 2, 3].map(|i| [points[i][0] / iw as f64, points[i][1] / ih as f64]);
            match grid.decode() {
                Ok((_, text)) if !text.is_empty() => return Some((text, corners)),
                _ => seen = seen.or(Some((String::new(), corners))),
            }
        }
    }
    seen
}

/// Whether two quads (0..1) are about the same place: the centre moved less than the code's size.
fn near(a: &Corners, b: &Corners) -> bool {
    let span = |q: &Corners, axis: usize| q.iter().map(|p| p[axis]).fold(f64::NEG_INFINITY, f64::max) - q.iter().map(|p| p[axis]).fold(f64::INFINITY, f64::min);
    let size = span(a, 0).hypot(span(a, 1));
    let mean = |q: &Corners, axis: usize| q.iter().map(|p| p[axis]).sum::<f64>() / 4.0;
    (mean(a, 0) - mean(b, 0)).hypot(mean(a, 1) - mean(b, 1)) < size
}

/// Lets through only a code read several looks in a row at about the same place, so stray quads never show.
#[derive(Clone, Debug)]
pub struct Steady {
    pub looks: usize,
    pub grace: usize,
    text: String,
    corners: Option<Corners>,
    count: usize,
    misses: usize,
}

impl Default for Steady {
    fn default() -> Steady {
        Steady::new(STEADY, GRACE)
    }
}

impl Steady {
    pub fn new(looks: usize, grace: usize) -> Steady {
        Steady { looks, grace, text: String::new(), corners: None, count: 0, misses: 0 }
    }

    pub fn confirmed(&self) -> bool {
        self.count >= self.looks
    }

    pub fn pending(&self) -> bool {
        self.count > 0 && self.count < self.looks
    }

    fn held(&self) -> Option<(String, Corners)> {
        if self.confirmed() { self.corners.map(|c| (self.text.clone(), c)) } else { None }
    }

    /// Takes one look's result ("" or no corners for a miss); (text, corners) while a code is confirmed.
    pub fn see(&mut self, text: &str, corners: Option<Corners>) -> Option<(String, Corners)> {
        let Some(corners) = corners.filter(|_| !text.is_empty()) else {
            self.misses += 1;
            if !self.confirmed() || self.misses > self.grace {
                (self.text, self.corners, self.count) = (String::new(), None, 0);
            }
            return self.held();
        };
        // Once confirmed the text alone says it's the same code, so one moved fast stays
        if text == self.text && (self.confirmed() || self.corners.as_ref().is_some_and(|c| near(&corners, c))) {
            self.count += 1;
        } else {
            (self.text, self.count) = (text.to_string(), 1);
        }
        (self.corners, self.misses) = (Some(corners), 0);
        self.held()
    }
}

// ── The plugin ───────────────────────────────────────────────────────────────

/// What the frame steps on the stream's thread and the plugin on the window's thread both look at.
#[derive(Default)]
struct Shared {
    on: AtomicBool,
    /// A code was just read and waits to be confirmed, so the next look comes sooner.
    pending: AtomicBool,
    /// A picture is being looked at, so the next one isn't queued behind it.
    busy: AtomicBool,
}

/// The frame step: now and then hands a picture to a thread that looks for a code in it. The frame itself goes on untouched.
struct Reader {
    shared: Arc<Shared>,
    poster: Arc<dyn Poster>,
    looked_at: Option<Instant>,
}

impl CpuStep for Reader {
    fn process(&mut self, frame: &mut Frame, _ctx: &dyn FrameContext) {
        if !self.shared.on.load(Ordering::SeqCst) || self.shared.busy.load(Ordering::SeqCst) {
            return;
        }
        let every = if self.shared.pending.load(Ordering::SeqCst) { PENDING } else { EVERY };
        let now = Instant::now();
        if self.looked_at.is_some_and(|at| now.duration_since(at) < every) {
            return;
        }
        self.looked_at = Some(now);
        self.shared.busy.store(true, Ordering::SeqCst);
        let (shared, poster, frame) = (self.shared.clone(), self.poster.clone(), frame.clone());
        let spawned = std::thread::Builder::new().name("qr-reader".into()).spawn(move || {
            let found = look(&frame);
            // A miss too, so a code that's gone stops being outlined
            let data = match found {
                Some((text, corners)) => json!({"text": text, "corners": corners}),
                None => json!({"text": "", "corners": null}),
            };
            poster.post(POST_FOUND, data);
            shared.busy.store(false, Ordering::SeqCst);
        });
        if spawned.is_err() {
            self.shared.busy.store(false, Ordering::SeqCst);
        }
    }
}

/// The picture shrunk to `WIDTH` if it's wider, as a grey one, and what's in it.
fn look(frame: &Frame) -> Option<(String, Corners)> {
    let (w, h) = (frame.width as usize, frame.height as usize);
    let grey = grey_of(frame);
    let scale = (WIDTH / w as f64).min(1.0);
    if scale < 1.0 {
        let (dw, dh) = ((w as f64 * scale).round() as usize, (h as f64 * scale).round() as usize);
        read_code(&area_resize(&grey, w, h, dw.max(1), dh.max(1)), dw.max(1), dh.max(1))
    } else {
        read_code(&grey, w, h)
    }
}

pub struct QrReaderPlugin {
    shared: Arc<Shared>,
    on: bool,
    notify: bool,
    steady: Steady,
    paired: Paired,
    poster: Option<Arc<dyn Poster>>,
    last_text: String,
    last_seen: f64,
    /// What the last code put (or tried to put) on the clipboard, what the card says about it and what the notification does.
    copied: String,
    copied_line: String,
    note: String,
    copy_shown: bool,
    status: (String, &'static str),
    held: Option<bool>,
    /// The outline on the preview now.
    outline: Option<Corners>,
    /// set_config has no host: the card hears the settings and the layer is added with the next hook that has one.
    pending: bool,
    clock: Box<dyn Fn() -> f64 + Send>,
}

impl Default for QrReaderPlugin {
    fn default() -> Self {
        QrReaderPlugin::new()
    }
}

impl QrReaderPlugin {
    pub fn new() -> QrReaderPlugin {
        QrReaderPlugin {
            shared: Arc::default(),
            on: false,
            notify: true,
            steady: Steady::default(),
            paired: Paired::default(),
            poster: None,
            last_text: String::new(),
            last_seen: 0.0,
            copied: String::new(),
            copied_line: String::new(),
            note: String::new(),
            copy_shown: false,
            status: (tr(HOLD_UP), "status_dim"),
            held: None,
            outline: None,
            pending: false,
            clock: Box::new(|| monotonic_ns() as f64 / 1e9),
        }
    }

    #[cfg(test)]
    fn with_clock(clock: impl Fn() -> f64 + Send + 'static) -> QrReaderPlugin {
        QrReaderPlugin { clock: Box::new(clock), ..QrReaderPlugin::new() }
    }

    /// (what goes to the clipboard, what the card says, what the notification says) for a code's text.
    pub fn copy(text: &str) -> (String, String, String) {
        if let Some(wifi) = parse_wifi(text) {
            if !wifi.password.is_empty() {
                let line = tr("Copied the password for {network}").replace("{network}", &wifi.ssid);
                return (wifi.password, line.clone(), line);
            }
            let line = tr("Copied the name of {network}, which has no password").replace("{network}", &wifi.ssid);
            return (wifi.ssid, line.clone(), line);
        }
        if is_link(text) {
            return (text.to_string(), tr("Copied {link}").replace("{link}", text), text.to_string());
        }
        (text.to_string(), tr("Copied the code's text"), text.to_string())
    }

    fn fixed_props() -> Value {
        json!({"switch-name": tr("QR codes"), "switch-help": tr(SWITCH_TIP), "notify-name": tr("Notify me when a code is read"),
               "copy-text": tr("Copy"), "copy-tip": tr(COPY_TIP)})
    }

    fn state_props(&self) -> Value {
        json!({"on": self.on, "notify": self.notify, "active": self.paired.on(), "copy-shown": self.copy_shown,
               "status": self.status.0, "status-kind": self.status.1})
    }

    fn all_props(&self) -> Value {
        let mut all = Self::fixed_props();
        if let (Value::Object(a), Value::Object(s)) = (&mut all, self.state_props()) {
            a.extend(s);
        }
        all
    }

    fn show(&mut self, host: &mut dyn Host, text: &str, kind: &'static str) {
        self.status = (text.to_string(), if kind.is_empty() { "status_dim" } else { kind });
        host.set_ui_props("card", &json!({"status": self.status.0, "status-kind": self.status.1}));
    }

    fn now(&self) -> f64 {
        (self.clock)()
    }

    fn catch_up(&mut self, host: &mut dyn Host) {
        if self.pending {
            self.pending = false;
            host.set_ui_props("card", &self.all_props());
            // Loading the settings goes through the switch once, whether or not it moved
            self.switch(host, self.on);
        }
    }

    fn layer(&self) -> PreviewLayer {
        let shapes = self.outline.map(|c| {
            let points: Vec<[f32; 2]> = c.iter().map(|p| [p[0] as f32, p[1] as f32]).collect();
            // The dark edge first, so the green one reads on any picture
            vec![Shape::Polygon { points: points.clone(), stroke: "#0000008c".into(), width: 5.0, fill: None },
                 Shape::Polygon { points, stroke: "#5fc98c".into(), width: 3.0, fill: None }]
        });
        PreviewLayer { shapes: shapes.unwrap_or_default() }
    }

    /// The outline on the preview is this now (None: gone).
    fn outline_to(&mut self, host: &mut dyn Host, corners: Option<Corners>) {
        self.outline = corners;
        host.add_preview_layer(NAME, self.layer());
        host.refresh_preview();
    }

    /// _on_switch
    fn switch(&mut self, host: &mut dyn Host, on: bool) {
        self.on = on;
        self.shared.on.store(on, Ordering::SeqCst);
        self.steady = Steady::default();
        self.shared.pending.store(false, Ordering::SeqCst);
        self.outline = None;
        host.cancel_schedule(T_OUTLINE);
        if on {
            host.add_preview_layer(NAME, self.layer());
        } else {
            host.remove_preview_layer(NAME);
        }
        let (text, kind) = if on { (tr(HOLD_UP), "") } else { (String::new(), "") };
        self.show(host, &text, kind);
        host.schedule_save();
    }

    fn switched_by_hand(&mut self, host: &mut dyn Host, on: bool) {
        host.set_ui_props("card", &json!({"on": on}));
        self.switch(host, on);
    }

    /// A look's result: a code's text ("" for none read) and its corners (None for none).
    fn on_found(&mut self, host: &mut dyn Host, text: &str, corners: Option<Corners>) {
        if !self.on {
            return; // a result that comes in after switching off is dropped
        }
        let now = self.now();
        let steady = self.steady.see(text, corners);
        self.shared.pending.store(self.steady.pending(), Ordering::SeqCst);
        let Some((_, confirmed)) = steady else {
            if self.outline.is_some() {
                host.cancel_schedule(T_OUTLINE);
                self.outline_to(host, None);
            }
            return;
        };
        self.outline_to(host, Some(confirmed));
        host.schedule(OUTLINE_MS, T_OUTLINE);
        if text.is_empty() {
            return; // kept through a missed look, but not read this time
        }
        let again = text == self.last_text && now - self.last_seen < AGAIN_S;
        self.last_text = text.to_string();
        self.last_seen = now;
        if again {
            return;
        }
        let (copied, shown, note) = Self::copy(text);
        self.copied = copied.clone();
        self.copied_line = shown;
        self.copy_shown = true;
        host.set_ui_props("card", &json!({"copy-shown": true}));
        host.copy_text(&copied);
        self.note = note;
        host.schedule(CHECK_MS, T_CHECK);
    }

    /// Whether the copy reached the clipboard: a window in the background can't put anything there on Wayland. A host that can't
    /// read the clipboard back can't tell, so it counts as reached.
    fn check(&mut self, host: &mut dyn Host) {
        let (copied, shown, note) = (self.copied.clone(), self.copied_line.clone(), self.note.clone());
        let reached = host.clipboard_text().is_none_or(|text| text == copied);
        if reached {
            self.show(host, &shown, "status_ok");
        } else {
            self.show(host, &tr("Found a code. Click Copy to put it on the clipboard."), "status_warn");
        }
        if self.notify {
            if reached {
                host.send_notification(&tr("QR code copied"), &note, false);
            } else {
                host.send_notification(&tr("QR code found"), &tr("Click Copy on the QR codes card to put it on the clipboard."), false);
            }
        }
    }

    fn copy_again(&mut self, host: &mut dyn Host) {
        host.copy_text(&self.copied);
        let line = self.copied_line.clone();
        self.show(host, &line, "status_ok");
    }
}

impl Plugin for QrReaderPlugin {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: NAME.into(),
            title: n_("QR code reader").into(),
            about: n_("Copies a QR code you hold up to the camera, or the password from a Wi-Fi code.").into(),
            when_off: n_("Without it there's no QR code card and codes in the picture are left alone.").into(),
            category: "preview".into(),
            icon: "qr".into(),
            frame_stage: FrameStage::Reading,
            optional: true,
            ..Default::default()
        }
    }

    fn setup(&mut self, host: &mut dyn Host) {
        self.poster = host.poster();
        host.set_ui_props("card", &self.all_props());
    }

    fn shutdown(&mut self, host: &mut dyn Host) {
        self.on = false;
        self.shared.on.store(false, Ordering::SeqCst);
        host.cancel_schedule(T_OUTLINE);
        host.cancel_schedule(T_CHECK);
        self.poster = None;
    }

    fn on_event(&mut self, host: &mut dyn Host, event: &Event) {
        self.catch_up(host);
        if let Some(on) = self.paired.hear(event) {
            host.set_ui_props("card", &json!({"active": on}));
        }
        match event {
            Event::Posted { owner, token, data } if owner == NAME && token == POST_FOUND => {
                let text = data.get("text").and_then(Value::as_str).unwrap_or("");
                let corners: Option<Corners> = data.get("corners").and_then(|c| serde_json::from_value(c.clone()).ok());
                self.on_found(host, text, corners);
            }
            Event::TimerFired { owner, token } if owner == NAME => match token.as_str() {
                T_OUTLINE => {
                    if self.outline.is_some() {
                        self.outline_to(host, None);
                    }
                }
                T_CHECK => self.check(host),
                _ => {}
            },
            _ => {}
        }
    }

    fn card(&mut self) -> Option<CardSpec> {
        Some(CardSpec { ui: UiSpec { source: SlintSource::Text(UI.into()), component: "QrReaderCard".into(), props: self.all_props() }, region: "left".into() })
    }

    fn on_ui(&mut self, host: &mut dyn Host, event: &UiEvent) {
        if event.ui != "card" {
            return;
        }
        self.catch_up(host);
        let on = event.args.first().and_then(Value::as_bool).unwrap_or(false);
        match event.callback.as_str() {
            "switch-toggled-by-hand" => self.switched_by_hand(host, on),
            "notify-toggled" => {
                self.notify = on;
                host.set_ui_props("card", &json!({"notify": on}));
                host.schedule_save();
            }
            "copy-clicked" => self.copy_again(host),
            _ => {}
        }
    }

    fn actions(&mut self) -> Vec<ActionSpec> {
        vec![actions::toggle_action("qr_reader.on", &tr("Read QR codes"), &tr("QR code reader"), "", &[])]
    }

    fn run_action(&mut self, host: &mut dyn Host, id: &str, params: &Value) -> Option<String> {
        self.catch_up(host);
        if id != "qr_reader.on" {
            return None;
        }
        let want = actions::toggle_wanted(params, self.on, &mut self.held);
        if want != self.on {
            self.switched_by_hand(host, want);
        }
        Some(actions::toggle_text(&tr("Read QR codes"), want))
    }

    fn release_action(&mut self, host: &mut dyn Host, id: &str, params: &Value) {
        if id != "qr_reader.on" {
            return;
        }
        if let Some(before) = actions::hold_released(params, &mut self.held) {
            if before != self.on {
                self.switched_by_hand(host, before);
            }
        }
    }

    fn frame_step(&mut self) -> Option<FrameStep> {
        let poster = self.poster.clone()?;
        Some(FrameStep::Cpu(Box::new(Reader { shared: self.shared.clone(), poster, looked_at: None })))
    }

    fn get_config(&self) -> Value {
        json!({"on": self.on, "notify": self.notify})
    }

    fn set_config(&mut self, cfg: &Value) -> Result<(), ConfigRejected> {
        self.notify = cfg.get("notify") != Some(&Value::Bool(false));
        self.on = cfg.get("on") == Some(&Value::Bool(true));
        self.shared.on.store(self.on, Ordering::SeqCst);
        self.pending = true;
        Ok(())
    }

    fn diagnostics(&self) -> Vec<(String, String)> {
        vec![("QR code reader".into(), if self.on { "on" } else { "off" }.into())]
    }
}

#[cfg(test)]
#[path = "qr_reader_tests.rs"]
mod tests;
