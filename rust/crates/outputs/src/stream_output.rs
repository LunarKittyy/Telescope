//! The Stream output plugin (plugins/stream_output.py): the resolution, frame rate and format of the stream, and the
//! JPEG quality or bitrate the phone sends at. It tells the phone through host.phone_control while a stream runs, and
//! the window reads its format and bitrate from its settings when a stream starts.

use telescope_core::i18n::Fields;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use telescope_core::event::Event;
use telescope_core::host::{Host, OutputChange};
use telescope_core::i18n::{n_, tr};
use telescope_core::plugin::{ConfigRejected, Plugin, PluginInfo, StreamInfo};
use telescope_core::ui::{ActionSpec, CardSpec, Issue, IssueAction, IssueDoes, SlintSource, UiEvent, UiSpec};

pub const NAME: &str = "stream_output";
const UI: &str = include_str!("../ui/stream_output.slint");

/// The recommended spot, marked on the slider.
const DEFAULT_QUALITY: i64 = 85;
/// Past about 92 a JPEG roughly doubles in size for no difference anyone sees.
const MAX_QUALITY: i64 = 95;
const DEFAULT_FPS: u32 = 30;
const FPS_CHOICES: [u32; 6] = [15, 24, 25, 30, 48, 60];
/// The phone clamps to the same range, and to what its encoder takes; 0 is Auto.
const MAX_BITRATE_MBPS: i64 = 100;
/// One past the top of the slider: Dynamic, as much as the connection carries. -1 in the config and to the phone.
const DYNAMIC_POS: i64 = MAX_BITRATE_MBPS + 1;
const DYNAMIC: i64 = -1;
const BITRATE_TIP: &str = n_("All the way right is Dynamic, which adjusts to what your connection can handle.");
const NO_DYNAMIC_TIP: &str = n_("This phone's app is too old for Dynamic and uses Auto until it's updated.");
const LIGHT_TIP: &str = n_("H.264 from the phone's hardware encoder, like video calls use.");
const HEAVY_TIP: &str = n_("MJPEG: every frame a full JPEG. Several times Light's data, sharper in fast motion.");

pub const FORMAT_MJPEG: &str = "mjpeg";
pub const FORMAT_H264: &str = "h264";

const COMMON_ASPECT_RATIOS: [(u32, u32); 2] = [(16, 9), (4, 3)];
const PREFERRED_DEFAULTS: [(u32, u32); 2] = [(1920, 1080), (1280, 720)];
/// At or under this the signal, not the network, is what holds a stream back.
const WEAK_WIFI_DBM: i64 = -70;
/// How long after a stream starts its settings go to the phone.
const INITIAL_PUSH_MS: u64 = 1500;

// Timer tokens and banner button ids
const T_INITIAL: &str = "initial";
const T_STOP: &str = "stop_encoder";
const A_HEAVY: &str = "switch_heavy";
const A_LIGHT: &str = "switch_light";
const A_DYNAMIC: &str = "switch_dynamic";

/// Why the phone went back from H.264, in the current language. A phone sends a code and the size next to its English
/// text; one that sends neither, or a code this app doesn't know, gets the English as it is.
pub fn codec_error_text(state: &Value) -> String {
    let code = state.get("codec_error_code").and_then(Value::as_str);
    let dim = |k: &str| state.get(k).and_then(Value::as_i64).filter(|v| *v > 0);
    let size = match (dim("codec_error_width"), dim("codec_error_height")) {
        (Some(w), Some(h)) => format!("{w}x{h}"),
        _ => String::new(),
    };
    match code {
        Some("encoder_stopped") => return tr("The phone's H.264 encoder stopped"),
        Some("h264_unavailable") if !size.is_empty() => return tr("H.264 isn't available at {size} on this phone").field("size", &size),
        Some("camera_cant_feed") if !size.is_empty() => return tr("The camera can't feed H.264 at {size} on this phone").field("size", &size),
        Some("camera_stopped") if !size.is_empty() => return tr("H.264 at {size} stopped the camera on this phone").field("size", &size),
        _ => {}
    }
    match state.get("codec_error") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => "None".into(),
        Some(other) => other.to_string(),
    }
}

pub fn size_label(w: u32, h: u32) -> String {
    format!("{w} × {h}")
}

/// (w, h) from a size_label (or one saved with " x " before the × came in), or None.
pub fn parse_size_label(text: &str) -> Option<(u32, u32)> {
    let text = text.replace(" x ", " × ");
    let (w, h) = text.split_once(" × ")?;
    let (w, h): (i64, i64) = (w.trim().parse().ok()?, h.trim().parse().ok()?);
    (w > 0 && h > 0).then_some((w as u32, h as u32))
}

fn gcd(a: u32, b: u32) -> u32 {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// Exact reduction, snapped to a common ratio if within rounding distance (854x480 is 16:9).
pub fn aspect_ratio(w: u32, h: u32) -> (u32, u32) {
    let ratio = w as f64 / h as f64;
    for common in COMMON_ASPECT_RATIOS {
        if (ratio - common.0 as f64 / common.1 as f64).abs() < 0.02 {
            return common;
        }
    }
    let g = gcd(w, h).max(1);
    (w / g, h / g)
}

/// The size nearest wh: its aspect ratio first, then the nearest height, the smaller one on a tie.
pub fn closest_size(wh: (u32, u32), sizes: impl IntoIterator<Item = (u32, u32)>) -> Option<(u32, u32)> {
    let ratio = aspect_ratio(wh.0, wh.1);
    sizes.into_iter().min_by_key(|s| (aspect_ratio(s.0, s.1) != ratio, (s.1 as i64 - wh.1 as i64).abs(), s.0 as u64 * s.1 as u64))
}

/// The two drop-downs that pick the size: shapes, then the sizes of the shape picked.
#[derive(Default)]
struct Sizes {
    ratios: Vec<(u32, u32)>,
    by_ratio: HashMap<(u32, u32), Vec<(u32, u32)>>,
    ar: usize,
    res: Vec<(u32, u32)>,
    res_current: usize,
    enabled: bool,
}

impl Sizes {
    fn listed(&self) -> Vec<(u32, u32)> {
        self.ratios.iter().flat_map(|r| self.by_ratio[r].iter().copied()).collect()
    }
    fn current(&self) -> Option<(u32, u32)> {
        self.res.get(self.res_current).copied()
    }
    fn current_text(&self) -> String {
        self.current().map_or("-".into(), |(w, h)| size_label(w, h))
    }
    fn find_by_label(&self, label: &str) -> Option<(u32, u32)> {
        self.by_ratio.values().flatten().copied().find(|(w, h)| size_label(*w, *h) == label)
    }
    /// Group sizes by aspect ratio, narrowest ratio first.
    fn rebuild(&mut self, sizes: &[(u32, u32)]) {
        let mut by_ratio: HashMap<(u32, u32), Vec<(u32, u32)>> = HashMap::new();
        let mut order = Vec::new();
        for &(w, h) in sizes {
            let r = aspect_ratio(w, h);
            if !by_ratio.contains_key(&r) {
                order.push(r);
            }
            by_ratio.entry(r).or_default().push((w, h));
        }
        order.sort_by(|a, b| (a.0 as f64 / a.1 as f64).total_cmp(&(b.0 as f64 / b.1 as f64)));
        self.ratios = order;
        self.by_ratio = by_ratio;
    }
    /// The sizes of one ratio, largest first, the first one picked.
    fn fill_res(&mut self, ratio: (u32, u32)) {
        let mut sizes = self.by_ratio[&ratio].clone();
        sizes.sort_by_key(|s| std::cmp::Reverse(s.0 as u64 * s.1 as u64));
        self.res = sizes;
        self.res_current = 0;
    }
    /// Point both at wh (the nearest listed size if it isn't listed, the top entry if None).
    fn select(&mut self, wh: Option<(u32, u32)>) {
        if self.ratios.is_empty() {
            return;
        }
        let listed = self.listed();
        let mut wh = wh;
        if let Some(w) = wh {
            if !listed.is_empty() && !listed.contains(&w) {
                wh = closest_size(w, listed.iter().copied());
            }
        }
        let mut ratio = wh.map(|(w, h)| aspect_ratio(w, h));
        if ratio.is_some_and(|r| !self.by_ratio.contains_key(&r)) {
            ratio = None;
            wh = None;
        }
        self.ar = ratio.and_then(|r| self.ratios.iter().position(|x| *x == r)).unwrap_or(0);
        self.fill_res(self.ratios[self.ar]);
        if let Some(w) = wh {
            if let Some(i) = self.res.iter().position(|s| *s == w) {
                self.res_current = i;
            }
        }
    }
    fn clear(&mut self) {
        *self = Sizes::default();
    }
}

pub struct StreamOutputPlugin {
    h264_available: bool,
    /// The stream the panels show while one runs (Python's ctrl).
    stream: Option<String>,
    current_camera_id: Option<Value>,
    sizes: Sizes,
    pending_resolution_text: Option<String>,
    had_saved_resolution: bool,
    saved_resolution_text: Option<String>,
    format: &'static str,
    phone_codecs: Vec<String>,
    phone_dynamic: Option<bool>,
    h264_profile: Option<String>,
    intra_refresh: bool,
    wifi: (Option<String>, Option<i64>, Option<i64>),
    over_usb: bool,
    phone_picked: bool,
    /// Phones paired, as PhonesChanged last said.
    phones: u32,
    h264_sizes: Option<HashSet<(u32, u32)>>,
    good_size: Option<(u32, u32)>,
    wanted_fps: u32,
    camera_max_fps: u32,
    fps_current: usize,
    quality: i64,
    bitrate: i64,
    stop_for: Option<Option<String>>,
}

impl StreamOutputPlugin {
    /// h264_available: whether this computer can decode H.264 (h264_reader.available()).
    pub fn new(h264_available: bool) -> StreamOutputPlugin {
        let mut p = StreamOutputPlugin {
            h264_available, stream: None, current_camera_id: None, sizes: Sizes::default(), pending_resolution_text: None,
            had_saved_resolution: false, saved_resolution_text: None, format: FORMAT_H264, phone_codecs: Vec::new(),
            phone_dynamic: None, h264_profile: None, intra_refresh: false, wifi: (None, None, None), over_usb: false,
            phone_picked: true, phones: 0, h264_sizes: None, good_size: None, wanted_fps: DEFAULT_FPS, camera_max_fps: 0,
            fps_current: 3, quality: DEFAULT_QUALITY, bitrate: 0, stop_for: None,
        };
        p.show_fps();
        p
    }

    /// The route to stream from: H.264 only when chosen and this computer can decode it.
    pub fn stream_format(&self) -> &'static str {
        if self.format == FORMAT_H264 && self.h264_available { FORMAT_H264 } else { FORMAT_MJPEG }
    }

    fn fps(&self) -> u32 {
        FPS_CHOICES[self.fps_current]
    }

    fn dynamic(&self) -> bool {
        self.bitrate == DYNAMIC_POS
    }

    fn bitrate_bps(&self) -> i64 {
        if self.dynamic() { DYNAMIC } else { self.bitrate * 1_000_000 }
    }

    /// Until the phone has reported, assume it can: nearly every phone has an encoder, and one without falls back.
    fn h264_offered(&self) -> bool {
        self.h264_available && (self.phone_codecs.is_empty() || self.phone_codecs.iter().any(|c| c == FORMAT_H264))
    }

    /// (width or None, height or None, fps) for the stream worker: the phone picks the size.
    pub fn stream_params(&self) -> (Option<u32>, Option<u32>, u32) {
        (None, None, self.fps())
    }

    /// What the phone should open at when a stream starts: the size picked here (if known) and the fps.
    pub fn opening(&self) -> Value {
        let mut out = json!({"fps": self.fps()});
        let text = self.pending_resolution_text.clone().or_else(|| self.saved_resolution_text.clone()).unwrap_or_default();
        let mut wh = parse_size_label(&text);
        if let (Some(w), Some(sizes)) = (wh, self.h264_sizes.as_ref()) {
            if self.stream_format() == FORMAT_H264 && !sizes.is_empty() && !sizes.contains(&w) {
                wh = closest_size(w, sizes.iter().copied()); // picked for Heavy: Light opens at the nearest it can do
            }
        }
        if let Some((w, h)) = wh {
            out["width"] = json!(w);
            out["height"] = json!(h);
        }
        out
    }

    fn send(&self, host: &mut dyn Host, body: Value) {
        if let Some(sid) = &self.stream {
            host.phone_control(Some(sid), &body);
        }
    }

    fn push_initial_settings(&self, host: &mut dyn Host) {
        if self.stream.is_some() {
            self.send(host, json!({"action": "jpeg_quality", "value": self.quality}));
            self.send(host, json!({"action": "bitrate", "value": self.bitrate_bps()}));
            self.send(host, json!({"action": "fps_target", "value": self.fps()}));
        }
    }

    // ── What the card shows ───────────────────────────────────────────────

    fn quality_tip(&self) -> String {
        let label = quality_label(self.quality);
        tr("{quality}. The dot ({default}%) is recommended: higher costs a lot more data for little you'd see. On slow Wi-Fi, lower it or the FPS.")
            .field("quality", &label).field("default", DEFAULT_QUALITY.to_string())
    }

    fn bitrate_text(&self) -> String {
        if self.dynamic() { tr("Dynamic") } else if self.bitrate > 0 { format!("{} Mbps", self.bitrate) } else { tr("Auto") }
    }

    fn light_tip(&self) -> String {
        if !self.h264_available {
            tr("Light needs FFmpeg but it's missing from this copy of Telescope. Reinstalling brings it back.")
        } else if !self.phone_codecs.is_empty() && !self.phone_codecs.iter().any(|c| c == FORMAT_H264) {
            tr("H.264. This phone has no H.264 encoder.")
        } else {
            tr(LIGHT_TIP)
        }
    }

    pub fn props(&self) -> Value {
        let (res_items, ar_items) = if self.sizes.ratios.is_empty() {
            (vec!["-".to_string()], vec!["-".to_string()])
        } else {
            (self.sizes.res.iter().map(|(w, h)| size_label(*w, *h)).collect(),
             self.sizes.ratios.iter().map(|(a, b)| format!("{a}:{b}")).collect())
        };
        let mut bitrate_tip = tr(BITRATE_TIP);
        if self.phone_dynamic == Some(false) {
            bitrate_tip = format!("{bitrate_tip} {}", tr(NO_DYNAMIC_TIP));
        }
        json!({
            "active": self.phones > 0 || !self.phone_picked, "phone-only-hidden": !self.phone_picked,
            "res-items": res_items, "res-current": self.sizes.res_current, "res-enabled": self.sizes.enabled,
            "ar-items": ar_items, "ar-current": self.sizes.ar, "ar-enabled": self.sizes.enabled,
            "fps-items": FPS_CHOICES.iter().map(|f| format!("{f} fps")).collect::<Vec<_>>(), "fps-current": self.fps_current,
            "fps-gone": FPS_CHOICES.iter().map(|f| self.camera_max_fps != 0 && *f > self.camera_max_fps).collect::<Vec<_>>(),
            "fps-tip": tr("The phone's capture rate and the virtual camera's. Lower saves data and battery."),
            "h264": self.stream_format() == FORMAT_H264, "h264-enabled": self.h264_offered(),
            "light-tip": self.light_tip(), "heavy-tip": tr(HEAVY_TIP),
            "quality": self.quality, "quality-readout": format!("{}%", self.quality), "quality-tip": self.quality_tip(),
            "bitrate": self.bitrate, "bitrate-readout": self.bitrate_text(), "bitrate-tip": bitrate_tip,
        })
    }

    fn render(&self, host: &mut dyn Host) {
        host.set_ui_props("card", &self.props());
    }

    // ── Sizes ─────────────────────────────────────────────────────────────

    fn forget_sizes(&mut self) {
        self.h264_sizes = None;
        self.good_size = None;
        self.camera_max_fps = 0;
        self.show_fps();
        self.sizes.clear();
    }

    fn light_sizes(cam: &Value) -> Option<HashSet<(u32, u32)>> {
        let listed = cam.get("h264Sizes")?.as_array()?;
        Some(listed.iter().filter_map(size_of).collect())
    }

    fn default_resolution(&self) -> Option<(u32, u32)> {
        let sixteen_nine = self.sizes.by_ratio.get(&(16, 9));
        for wh in PREFERRED_DEFAULTS {
            if sixteen_nine.is_some_and(|s| s.contains(&wh)) {
                return Some(wh);
            }
        }
        for ratio in [(16, 9), (4, 3)] {
            if let Some(sizes) = self.sizes.by_ratio.get(&ratio).filter(|s| !s.is_empty()) {
                return sizes.iter().copied().max_by_key(|s| s.0 as u64 * s.1 as u64);
            }
        }
        None
    }

    fn apply_camera(&mut self, host: &mut dyn Host, cam: &Value, live: (Option<u32>, Option<u32>)) {
        let mut sizes: Vec<(u32, u32)> = cam.get("supportedSizes").and_then(Value::as_array).map(|a| a.iter().filter_map(size_of).collect()).unwrap_or_default();
        self.h264_sizes = Self::light_sizes(cam);
        if self.stream_format() == FORMAT_H264 {
            if let Some(light) = self.h264_sizes.as_ref().filter(|s| !s.is_empty()) {
                let can: Vec<(u32, u32)> = sizes.iter().copied().filter(|s| light.contains(s)).collect();
                if !can.is_empty() {
                    sizes = can; // leave out what Light can't do
                }
            }
        }
        if sizes.is_empty() {
            return;
        }
        let id = cam.get("id").cloned();
        if id != self.current_camera_id {
            // Only rebuild on an actual camera change, so it doesn't fight what's picked
            let is_first_bind = self.current_camera_id.is_none();
            self.current_camera_id = id;
            self.sizes.rebuild(&sizes);
            let target_wh = self.pending_resolution_text.as_deref().and_then(|t| self.sizes.find_by_label(t));
            // A brand-new device with nothing saved gets our own default rather than whatever the phone happened to be at
            let fresh_device = is_first_bind && target_wh.is_none() && !self.had_saved_resolution;
            let mut target = target_wh;
            if !fresh_device && target.is_none() {
                if let (Some(w), Some(h)) = live {
                    target = Some((w, h));
                }
            }
            let force_default = target.is_none();
            if force_default {
                target = self.default_resolution();
            }
            self.sizes.select(target);
            let final_wh = self.sizes.current();
            // Push it whenever it isn't what the phone runs at, or the box would show a size the phone never got
            let live_known = live.0.is_some() && live.1.is_some();
            if final_wh.is_some() && final_wh.map(|(w, h)| (Some(w), Some(h))) != Some(live) && (force_default || live_known) {
                self.on_resolution(host);
            }
            self.sizes.enabled = true;
            self.pending_resolution_text = None;
        } else if let (Some(w), Some(h)) = live {
            if self.sizes.current_text() != size_label(w, h) {
                self.sizes.select(Some((w, h)));
            }
        }
    }

    fn on_resolution(&mut self, host: &mut dyn Host) {
        let Some((w, h)) = self.sizes.current() else { return };
        if self.stream.is_none() {
            // Picked while stopped: the next start opens at it
            self.saved_resolution_text = Some(self.sizes.current_text());
            self.pending_resolution_text = self.saved_resolution_text.clone();
            host.schedule_save();
            return;
        }
        self.send(host, json!({"action": "resolution", "width": w, "height": h}));
        host.emit(Event::ResolutionChangeRequested { width: w, height: h });
        host.schedule_save();
    }

    fn back_to_good_size(&mut self, host: &mut dyn Host, state: &Value) -> Option<String> {
        let failed = (dim(state, "stream_width"), dim(state, "stream_height"));
        let good = self.good_size?;
        if (Some(good.0), Some(good.1)) == failed || !self.sizes.res.contains(&good) {
            return None;
        }
        self.sizes.select(Some(good));
        let label = size_label(good.0, good.1);
        self.saved_resolution_text = Some(label.clone());
        self.pending_resolution_text = Some(label.clone());
        host.schedule_save();
        Some(label)
    }

    // ── Frame rate ────────────────────────────────────────────────────────

    /// Picks the wanted rate, or the fastest the lens can below. True if the rate in use changed.
    fn show_fps(&mut self) -> bool {
        let before = self.fps_current;
        let limit = self.camera_max_fps;
        let usable: Vec<u32> = FPS_CHOICES.iter().copied().filter(|f| limit == 0 || *f <= limit).collect();
        let usable = if usable.is_empty() { vec![FPS_CHOICES[0]] } else { usable };
        let use_ = usable.iter().copied().filter(|f| *f <= self.wanted_fps).max().unwrap_or(usable[0]);
        self.fps_current = FPS_CHOICES.iter().position(|f| *f == use_).unwrap_or(3);
        use_ != FPS_CHOICES[before]
    }

    fn set_camera_max_fps(&mut self, host: &mut dyn Host, max_fps: Option<i64>) {
        let max_fps = max_fps.filter(|m| *m > 0).map_or(0, |m| m as u32);
        if max_fps == self.camera_max_fps {
            return;
        }
        self.camera_max_fps = max_fps;
        if self.show_fps() {
            self.apply_fps(host); // the picked rate stays saved, for a lens that can do it
        }
    }

    fn apply_fps(&self, host: &mut dyn Host) {
        let fps = self.fps();
        host.update_stream_output(OutputChange { fps: Some(Some(fps)), ..OutputChange::default() }, None);
        self.send(host, json!({"action": "fps_target", "value": fps}));
    }

    /// A saved rate (older versions took any 5-60) as the closest choice, the lower one on a tie.
    fn nearest_fps(fps: &Value) -> u32 {
        let Some(f) = fps.as_f64().map(|f| f as i64).or_else(|| fps.as_str().and_then(|s| s.trim().parse().ok())) else { return DEFAULT_FPS };
        FPS_CHOICES.iter().copied().min_by_key(|c| ((*c as i64 - f).abs(), *c)).unwrap()
    }

    // ── Format ────────────────────────────────────────────────────────────

    fn set_format(&mut self, host: &mut dyn Host, fmt: &'static str) {
        if fmt == self.format {
            return;
        }
        self.format = fmt;
        host.schedule_save();
        if !host.is_streaming() {
            host.emit(Event::StreamTransportChanged); // a stream reconnects instead, which says it again
        }
        // The route decides the phone's codec, so switching means reconnecting
        host.reconnect_stream();
    }

    fn wifi_hint(&self) -> String {
        let (band, rssi, _) = &self.wifi;
        if self.over_usb {
            return String::new();
        }
        if rssi.is_some_and(|r| r <= WEAK_WIFI_DBM) {
            return tr("The phone's Wi-Fi signal is weak. Moving it closer to the router or plugging in a cable will help the most.");
        }
        if band.as_deref() == Some("2.4") {
            return tr("The phone is on 2.4 GHz Wi-Fi which is slower and often crowded. A 5 GHz network or a cable will help the most.");
        }
        String::new()
    }

    fn on_stream_behind(&mut self, host: &mut dyn Host, sid: &str, behind: bool) {
        if !self.phone_picked {
            return;
        }
        if !behind {
            host.clear_issue(Some("behind"), None); // only one the panels showed can have been behind
            return;
        }
        if sid != host.focused_source_id().unwrap_or_default() {
            return;
        }
        let on_light = self.stream_format() == FORMAT_H264;
        let plugin = |label: String, id: &str| IssueAction { label, does: IssueDoes::Plugin { id: id.into() } };
        let (mut text, action) = if !on_light && self.h264_offered() {
            (tr("Try Light or lower quality."), Some(plugin(tr("Switch to Light"), A_LIGHT)))
        } else if on_light && self.dynamic() {
            (tr("Try a lower resolution or FPS."), None) // Dynamic already lowered what it could
        } else if on_light && self.phone_dynamic != Some(false) {
            (tr("Try Dynamic or lower quality."), Some(plugin(tr("Switch to Dynamic"), A_DYNAMIC)))
        } else {
            (tr("Try lower quality."), None)
        };
        let hint = self.wifi_hint();
        if !hint.is_empty() {
            text = format!("{hint} {text}");
        }
        host.show_issue("behind", Issue { title: tr("Can't keep up"), text, actions: action.into_iter().collect(), kind: "warn".into(),
                                          details: String::new(), tell_dismiss: false });
    }

    fn on_phone_state_inner(&mut self, host: &mut dyn Host, state: &Value) {
        // {} means the state fetch failed, not a phone without H.264 (a phone leaves codecs out when it's only MJPEG)
        let has_cameras = state.get("cameras").is_some();
        let codecs: Vec<String> = if has_cameras {
            let listed: Vec<String> = state.get("codecs").and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|c| c.as_str().map(String::from)).collect()).unwrap_or_default();
            if listed.is_empty() { vec![FORMAT_MJPEG.into()] } else { listed }
        } else {
            self.phone_codecs.clone()
        };
        if !codecs.is_empty() && codecs != self.phone_codecs {
            self.phone_codecs = codecs.clone();
        }
        if has_cameras {
            self.phone_dynamic = Some(truthy(state.get("dynamic_bitrate")));
            self.h264_profile = state.get("h264_profile").and_then(Value::as_str).map(String::from);
            self.intra_refresh = state.get("intra_refresh") == Some(&Value::Bool(true));
            self.wifi = (state.get("wifi_band").and_then(Value::as_str).map(String::from),
                         state.get("wifi_rssi").filter(|v| v.is_i64()).and_then(Value::as_i64),
                         state.get("wifi_mbps").filter(|v| v.is_i64()).and_then(Value::as_i64));
        }
        let mut back = None;
        let codec_error = truthy(state.get("codec_error"));
        if self.format == FORMAT_H264 && !codecs.is_empty() && !codecs.iter().any(|c| c == FORMAT_H264) {
            // Light is the default, and a phone without an encoder sends nothing on its route: Heavy for this phone
            self.set_format(host, FORMAT_MJPEG);
        } else if self.format == FORMAT_H264 && codec_error && truthy(state.get("codec_unsupported")) {
            // Too much for the phone's encoder at this size or rate: stop and let the choice be made. The stop waits
            // for every plugin to have this state, so none fills its controls back in after it.
            let sid = host.focused_source_id();
            self.stop_for = Some(sid.clone());
            host.schedule(0, T_STOP);
            back = self.back_to_good_size(host, state);
            let mut actions = vec![IssueAction { label: tr("Switch to Heavy"),
                                                 does: IssueDoes::StartAgain { source_id: sid.clone(), before: Some(A_HEAVY.into()) } }];
            let error = codec_error_text(state);
            let text = if let Some(size) = &back {
                actions.insert(0, IssueAction { label: tr("Start at {size}").field("size", size),
                                                does: IssueDoes::StartAgain { source_id: sid, before: None } });
                tr("{error}. It's set back to {size}, the last size that worked.").field("error", &error).field("size", size)
            } else {
                tr("{error}. Try a lower resolution or FPS, or switch to Heavy.").field("error", &error)
            };
            host.show_issue("encoder", Issue { title: tr("Too much for the phone's H.264 encoder"), text, actions, kind: "warn".into(),
                                               details: String::new(), tell_dismiss: false });
        } else if self.format == FORMAT_H264 && codec_error {
            // The phone went back to MJPEG; so does the stream, or it would keep asking for H.264
            host.show_issue("h264", Issue {
                title: tr("Switched to Heavy"),
                text: tr("{error}. You can try Light again in Stream output.").field("error", codec_error_text(state)),
                actions: Vec::new(), kind: "warn".into(), details: String::new(), tell_dismiss: false,
            });
            self.set_format(host, FORMAT_MJPEG);
        }
        let Some(cams) = state.get("cameras").and_then(Value::as_array) else { return };
        let Some(cur) = cams.iter().find(|c| truthy(c.get("current"))) else { return };
        let mut live = (dim(state, "stream_width"), dim(state, "stream_height"));
        if back.is_some() {
            live = (None, None); // the size it failed at isn't the one to show any more
        }
        if state.get("codec").and_then(Value::as_str) == Some(FORMAT_H264) && !codec_error {
            if let (Some(w), Some(h)) = live {
                self.good_size = Some((w, h));
            }
        }
        self.set_camera_max_fps(host, cur.get("maxFps").filter(|v| v.is_i64()).and_then(Value::as_i64));
        self.apply_camera(host, cur, live);
    }

    fn wifi_text(&self) -> String {
        let (band, rssi, mbps) = &self.wifi;
        let parts = [band.as_ref().filter(|b| !b.is_empty()).map(|b| format!("{b} GHz")), rssi.map(|r| format!("{r} dBm")),
                     mbps.filter(|m| *m != 0).map(|m| format!("{m} Mbps link"))];
        parts.into_iter().flatten().collect::<Vec<_>>().join(", ")
    }

    // ── Shortcuts ─────────────────────────────────────────────────────────

    fn choice_spec(id: &str, label: String, options: Vec<(String, String)>, wrap: bool) -> ActionSpec {
        let _ = wrap;
        ActionSpec {
            id: id.into(), label, group: tr("Stream output"),
            params: vec![
                json!({"name": "op", "label": tr("Does"), "kind": "choice", "default": "next",
                       "choices": [["next", tr("Next")], ["previous", tr("Previous")], ["set", tr("Pick")]]}),
                json!({"name": "option", "label": tr("Option"), "kind": "choice", "default": "",
                       "choices": options.iter().map(|(v, s)| json!([v, s])).collect::<Vec<_>>(), "shown_if": ["op", ["set"]]}),
            ],
            repeats: false, on_release: false, current: None, keywords: String::new(),
        }
    }

    fn slider_spec(id: &str, label: String, step: f64, max: f64, suffix: &str, current: i64, has_default: bool) -> ActionSpec {
        let mut ops = vec![json!(["up", tr("Increase by")]), json!(["down", tr("Decrease by")]), json!(["set", tr("Set to")])];
        if has_default {
            ops.push(json!(["reset", tr("Reset to default")]));
        }
        ActionSpec {
            id: id.into(), label, group: tr("Stream output"),
            params: vec![
                json!({"name": "op", "label": tr("Does"), "kind": "choice", "default": "up", "choices": ops}),
                json!({"name": "amount", "label": tr("By"), "kind": "number", "default": step, "minimum": 0, "maximum": max,
                       "suffix": suffix, "shown_if": ["op", ["up", "down"]]}),
                json!({"name": "value", "label": tr("Value"), "kind": "number", "default": null, "maximum": max,
                       "suffix": suffix, "shown_if": ["op", ["set"]]}),
            ],
            repeats: true, on_release: false, current: Some(current as f64), keywords: String::new(),
        }
    }

    /// The option a choice key lands on: (index) of options, or None if nothing changes. enabled says which can be picked.
    fn choice_target(params: &Value, options: &[(String, String)], enabled: &[bool], current: usize, wrap: bool) -> Result<usize, ()> {
        let op = params["op"].as_str().unwrap_or("next");
        if op == "set" {
            let want = params["option"].as_str().unwrap_or("");
            return options.iter().enumerate().position(|(i, (v, s))| enabled[i] && (v == want || s == want)).ok_or(());
        }
        let n = options.len() as i64;
        let step = if op == "next" { 1 } else { -1 };
        let mut i = current as i64;
        for _ in 0..n {
            i += step;
            if !wrap && !(0..n).contains(&i) {
                break;
            }
            i = i.rem_euclid(n);
            if enabled[i as usize] && i as usize != current {
                return Ok(i as usize);
            }
        }
        Err(())
    }

    fn slider_value(params: &Value, current: i64, min: i64, max: i64, default: Option<i64>) -> Option<i64> {
        let new = match params["op"].as_str().unwrap_or("up") {
            "set" => params["value"].as_f64()?.round() as i64,
            "reset" => default.unwrap_or(current),
            op => {
                let steps = params["amount"].as_f64().map_or(0, |a| (a.round() as i64).max(1));
                current + if op == "up" { steps } else { -steps }
            }
        };
        Some(new.clamp(min, max))
    }
}

fn size_of(s: &Value) -> Option<(u32, u32)> {
    let (w, h) = (s.get("width")?.as_i64()?, s.get("height")?.as_i64()?);
    (w > 0 && h > 0).then_some((w as u32, h as u32))
}

fn dim(state: &Value, key: &str) -> Option<u32> {
    state.get(key).filter(|v| v.is_i64()).and_then(Value::as_i64).filter(|v| *v > 0).map(|v| v as u32)
}

/// Python's truthiness for a JSON value.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64() != Some(0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// widgets.common.quality_label: what a JPEG quality looks like, in words.
fn quality_label(q: i64) -> String {
    let text = if q >= 91 { tr("{q}%: High") } else if q >= 80 { tr("{q}%: Balanced") } else if q >= 60 { tr("{q}%: Low") } else { tr("{q}%: Very low") };
    text.field("q", q.to_string())
}

impl Plugin for StreamOutputPlugin {
    fn stream_params(&self) -> Option<(Option<u32>, Option<u32>, u32)> {
        Some(StreamOutputPlugin::stream_params(self))
    }
    fn opening(&self) -> Option<telescope_core::link::StreamRequest> {
        let body = StreamOutputPlugin::opening(self);
        let number = |key: &str| body.get(key).and_then(Value::as_u64).unwrap_or(0) as u32;
        Some(telescope_core::link::StreamRequest { width: number("width"), height: number("height"), fps: number("fps"), camera_off: false })
    }
    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: NAME.into(),
            title: n_("Stream output").into(),
            about: n_("The resolution, frame rate and format of the stream.").into(),
            when_off: n_("Without it there's no Stream output card and streams use whatever size and format the phone picks.").into(),
            category: "streaming".into(),
            icon: "stream".into(),
            pinned: true,
            in_presets: true,
            device_local: true,
            ..PluginInfo::default()
        }
    }

    fn card(&mut self) -> Option<CardSpec> {
        Some(CardSpec {
            ui: UiSpec { source: SlintSource::Text(UI.into()), component: "StreamOutputCard".into(), props: self.props() },
            region: "left".into(),
        })
    }

    fn on_event(&mut self, host: &mut dyn Host, event: &Event) {
        match event {
            Event::CameraSwitched { camera } => {
                // A lens switch reuses the phone's reader, so the size carries over
                let live = self.sizes.current().map_or((None, None), |(w, h)| (Some(w), Some(h)));
                self.apply_camera(host, camera, live);
                self.render(host);
            }
            Event::DeviceChanged { .. } => {
                self.forget_sizes();
                self.phone_codecs.clear();
                self.phone_dynamic = None;
                self.h264_profile = None;
                self.intra_refresh = false;
                self.wifi = (None, None, None);
                self.render(host);
            }
            Event::StreamBehind { source_id, behind } => self.on_stream_behind(host, source_id, *behind),
            Event::SourceSelected { source_id } => {
                self.phone_picked = source_id.is_empty();
                self.render(host);
            }
            Event::PhonesChanged { count } => {
                self.phones = *count;
                self.render(host);
            }
            Event::TimerFired { owner, token } if owner == NAME => {
                if token == T_INITIAL {
                    self.push_initial_settings(host);
                } else if token == T_STOP {
                    let sid = self.stop_for.take().flatten();
                    host.stop_stream(sid.as_deref());
                }
            }
            _ => {}
        }
    }

    fn on_stream_start(&mut self, host: &mut dyn Host, stream: &StreamInfo) {
        self.stream = Some(stream.source_id.clone());
        let host_part = stream.url.split("://").nth(1).unwrap_or("").split('/').next().unwrap_or("");
        let hostname = host_part.rsplit_once(':').map_or(host_part, |(h, _)| h).trim_matches(['[', ']']);
        self.over_usb = matches!(hostname, "127.0.0.1" | "localhost" | "::1");
        host.schedule(INITIAL_PUSH_MS, T_INITIAL);
    }

    fn on_stream_stop(&mut self, host: &mut dyn Host) {
        host.cancel_schedule(T_INITIAL);
        if self.sizes.current().is_some() {
            self.saved_resolution_text = Some(self.sizes.current_text());
            // The lens's sizes stay up, so a size can be picked before starting again; the next start opens at it
            self.pending_resolution_text = self.saved_resolution_text.clone();
        }
        self.stream = None;
        self.current_camera_id = None;
    }

    fn on_phone_state(&mut self, host: &mut dyn Host, state: &Value) {
        self.on_phone_state_inner(host, state);
        self.render(host);
    }

    fn on_ui(&mut self, host: &mut dyn Host, event: &UiEvent) {
        let arg = |i: usize| event.args.get(i).cloned().unwrap_or(Value::Null);
        match event.callback.as_str() {
            "res-picked" => {
                let Some(i) = arg(0).as_u64() else { return };
                if (i as usize) < self.sizes.res.len() {
                    self.sizes.res_current = i as usize;
                    self.on_resolution(host);
                }
            }
            "ar-picked" => {
                let Some(i) = arg(0).as_u64().map(|i| i as usize).filter(|i| *i < self.sizes.ratios.len()) else { return };
                self.pick_ratio(host, i);
            }
            "fps-picked" => {
                let Some(i) = arg(0).as_u64().map(|i| i as usize).filter(|i| *i < FPS_CHOICES.len()) else { return };
                if self.camera_max_fps == 0 || FPS_CHOICES[i] <= self.camera_max_fps {
                    self.fps_current = i;
                    self.wanted_fps = self.fps();
                    self.apply_fps(host);
                    host.schedule_save();
                }
            }
            "format-picked" => {
                host.clear_issue(Some("h264"), None);
                let fmt = if arg(0).as_bool() == Some(true) { FORMAT_H264 } else { FORMAT_MJPEG };
                self.set_format(host, fmt);
            }
            "quality-moved" => {
                let Some(q) = arg(0).as_f64() else { return };
                self.set_quality(host, q.round() as i64);
            }
            "bitrate-moved" => {
                let Some(b) = arg(0).as_f64() else { return };
                self.set_bitrate(host, b.round() as i64);
            }
            _ => return,
        }
        self.render(host);
    }

    fn on_issue_action(&mut self, host: &mut dyn Host, _key: &str, id: &str) {
        match id {
            A_HEAVY => self.set_format(host, FORMAT_MJPEG),
            A_LIGHT => {
                host.clear_issue(Some("h264"), None);
                self.set_format(host, FORMAT_H264);
            }
            A_DYNAMIC => self.set_bitrate(host, DYNAMIC_POS),
            _ => return,
        }
        self.render(host);
    }

    fn actions(&mut self) -> Vec<ActionSpec> {
        let ratios: Vec<(String, String)> = self.sizes.ratios.iter().map(|(a, b)| (format!("{a}:{b}"), format!("{a}:{b}"))).collect();
        let sizes: Vec<(String, String)> = self.sizes.res.iter().map(|(w, h)| (format!("{w} x {h}"), size_label(*w, *h))).collect();
        let fps: Vec<(String, String)> = FPS_CHOICES.iter().map(|f| (format!("{f} fps"), format!("{f} fps"))).collect();
        let formats = vec![("Light".to_string(), tr("Light")), ("Heavy".to_string(), tr("Heavy"))];
        vec![
            Self::choice_spec("stream.aspect_ratio", tr("Aspect ratio"), ratios, true),
            Self::choice_spec("stream.resolution", tr("Resolution"), sizes, true),
            Self::choice_spec("stream.fps", tr("FPS"), fps, false),
            Self::choice_spec("stream.format", tr("Format"), formats, true),
            Self::slider_spec("stream.quality", tr("JPEG quality"), 5.0, MAX_QUALITY as f64, "", self.quality, true),
            Self::slider_spec("stream.bitrate", tr("Bitrate"), 1.0, DYNAMIC_POS as f64, " Mbps", self.bitrate, true),
        ]
    }

    fn run_action(&mut self, host: &mut dyn Host, id: &str, params: &Value) -> Option<String> {
        let said = |label: String, value: String| Some(tr("{label}: {value}").field("label", &label).field("value", &value));
        let out = match id {
            "stream.aspect_ratio" | "stream.resolution" if !self.sizes.enabled => return None,
            "stream.aspect_ratio" => {
                let opts: Vec<(String, String)> = self.sizes.ratios.iter().map(|(a, b)| (format!("{a}:{b}"), format!("{a}:{b}"))).collect();
                match Self::choice_target(params, &opts, &vec![true; opts.len()], self.sizes.ar, true) {
                    Ok(i) => {
                        self.pick_ratio(host, i);
                        said(tr("Aspect ratio"), opts[i].1.clone())
                    }
                    Err(()) if params["op"] == "set" => return None,
                    Err(()) => said(tr("Aspect ratio"), opts.get(self.sizes.ar)?.1.clone()),
                }
            }
            "stream.resolution" => {
                let opts: Vec<(String, String)> = self.sizes.res.iter().map(|(w, h)| (format!("{w} x {h}"), size_label(*w, *h))).collect();
                match Self::choice_target(params, &opts, &vec![true; opts.len()], self.sizes.res_current, true) {
                    Ok(i) => {
                        self.sizes.res_current = i;
                        self.on_resolution(host);
                        said(tr("Resolution"), opts[i].1.clone())
                    }
                    Err(()) if params["op"] == "set" => return None,
                    Err(()) => said(tr("Resolution"), opts.get(self.sizes.res_current)?.1.clone()),
                }
            }
            "stream.fps" => {
                let opts: Vec<(String, String)> = FPS_CHOICES.iter().map(|f| (format!("{f} fps"), format!("{f} fps"))).collect();
                let enabled: Vec<bool> = FPS_CHOICES.iter().map(|f| self.camera_max_fps == 0 || *f <= self.camera_max_fps).collect();
                match Self::choice_target(params, &opts, &enabled, self.fps_current, false) {
                    Ok(i) => {
                        self.fps_current = i;
                        self.wanted_fps = self.fps();
                        self.apply_fps(host);
                        host.schedule_save();
                        said(tr("FPS"), opts[i].1.clone())
                    }
                    Err(()) if params["op"] == "set" => return None,
                    Err(()) => said(tr("FPS"), opts[self.fps_current].1.clone()),
                }
            }
            "stream.format" => {
                let opts = vec![("Light".to_string(), tr("Light")), ("Heavy".to_string(), tr("Heavy"))];
                let enabled = [self.h264_offered(), true];
                let cur = usize::from(self.stream_format() != FORMAT_H264);
                match Self::choice_target(params, &opts, &enabled, cur, true) {
                    Ok(i) => {
                        host.clear_issue(Some("h264"), None);
                        self.set_format(host, if i == 0 { FORMAT_H264 } else { FORMAT_MJPEG });
                        said(tr("Format"), opts[i].1.clone())
                    }
                    Err(()) if params["op"] == "set" => return None,
                    Err(()) => said(tr("Format"), opts[cur].1.clone()),
                }
            }
            "stream.quality" => {
                if self.stream_format() == FORMAT_H264 {
                    return None; // hidden while Light streams
                }
                let q = Self::slider_value(params, self.quality, 1, MAX_QUALITY, Some(DEFAULT_QUALITY))?;
                self.set_quality(host, q);
                said(tr("JPEG quality"), format!("{}%", self.quality))
            }
            "stream.bitrate" => {
                if self.stream_format() != FORMAT_H264 {
                    return None;
                }
                let b = Self::slider_value(params, self.bitrate, 0, DYNAMIC_POS, Some(0))?;
                self.set_bitrate(host, b);
                said(tr("Bitrate"), self.bitrate_text())
            }
            _ => return None,
        };
        self.render(host);
        out
    }

    fn get_config(&self) -> Value {
        let mut cfg = json!({
            "fps": self.wanted_fps,
            "jpeg_quality": self.quality,
            "format": self.format,
            "bitrate_mbps": if self.dynamic() { DYNAMIC } else { self.bitrate },
        });
        let mut saved = self.saved_resolution_text.clone();
        if self.stream.is_some() && self.sizes.current().is_some() {
            saved = Some(self.sizes.current_text()); // stopped, a pick or preset already set it
        }
        if let Some(text) = saved.filter(|t| !t.is_empty()) {
            cfg["resolution"] = json!(text.replace(" × ", " x ")); // saved the way older versions read it
        }
        cfg
    }

    fn set_config(&mut self, cfg: &Value) -> Result<(), ConfigRejected> {
        // Always overwrite: the host applies defaults before each device's own config
        let mut res = cfg.get("resolution").and_then(|r| match r {
            Value::String(s) if !s.is_empty() => Some(s.clone()),
            Value::Null => None,
            Value::Bool(false) => None,
            other if !truthy(Some(other)) => None,
            other => Some(other.to_string()),
        });
        if let Some((w, h)) = res.as_deref().and_then(parse_size_label) {
            res = Some(size_label(w, h));
        }
        self.pending_resolution_text = res.clone();
        self.saved_resolution_text = res.clone();
        self.had_saved_resolution = res.is_some();
        if let Some(fps) = cfg.get("fps").filter(|v| truthy(Some(v))) {
            self.wanted_fps = Self::nearest_fps(fps);
            self.show_fps();
        }
        if let Some(q) = cfg.get("jpeg_quality").filter(|v| truthy(Some(v))) {
            let q = q.as_f64().map(|f| f as i64).or_else(|| q.as_str().and_then(|s| s.parse().ok()))
                .ok_or_else(|| ConfigRejected("jpeg_quality isn't a number".into()))?;
            self.quality = q.clamp(1, MAX_QUALITY);
        }
        self.format = if cfg.get("format").and_then(Value::as_str) == Some(FORMAT_MJPEG) { FORMAT_MJPEG } else { FORMAT_H264 };
        let mbps = match cfg.get("bitrate_mbps") {
            None | Some(Value::Null) | Some(Value::Bool(false)) => 0,
            Some(v) => v.as_f64().map(|f| f as i64).or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
                .ok_or_else(|| ConfigRejected("bitrate_mbps isn't a number".into()))?,
        };
        self.bitrate = if mbps == DYNAMIC { DYNAMIC_POS } else { mbps.clamp(0, MAX_BITRATE_MBPS) };
        Ok(())
    }

    fn apply_preset(&mut self, host: &mut dyn Host, cfg: &Value) -> Result<(), ConfigRejected> {
        let old_format = self.stream_format();
        self.set_config(cfg)?;
        if !self.phone_codecs.is_empty() && !self.phone_codecs.iter().any(|c| c == FORMAT_H264) {
            self.format = FORMAT_MJPEG; // a preset without a format can't ask this phone for Light
        }
        let wh = self.pending_resolution_text.as_deref().and_then(|t| self.sizes.find_by_label(t));
        if self.stream.is_none() {
            if wh.is_some() {
                self.sizes.select(wh); // the sizes still up from the last stream show it
            }
            self.render(host);
            return Ok(());
        }
        if self.stream_format() != old_format {
            host.reconnect_stream(); // the new stream picks everything up as it starts
            self.render(host);
            return Ok(());
        }
        host.emit(Event::StreamTransportChanged);
        self.push_initial_settings(host);
        host.update_stream_output(OutputChange { fps: Some(Some(self.fps())), ..OutputChange::default() }, None);
        if wh.is_some() && wh != self.sizes.current() {
            self.sizes.select(wh);
            self.on_resolution(host);
        }
        if wh.is_some() {
            self.pending_resolution_text = None;
        }
        self.render(host);
        Ok(())
    }

    fn diagnostics(&self) -> Vec<(String, String)> {
        let fmt = self.stream_format();
        let mut out = vec![
            ("Format".into(), format!("{fmt}{}", if self.format != fmt { " (H.264 chosen)" } else { "" })),
            ("Phone codecs".into(), if self.phone_codecs.is_empty() { "unknown".into() } else { self.phone_codecs.join(", ") }),
            ("Resolution".into(), self.sizes.current_text()),
            ("FPS".into(), format!("{}{}", self.fps(), if self.fps() != self.wanted_fps { format!(" ({} chosen)", self.wanted_fps) } else { String::new() })),
            ("Bitrate".into(), format!("{}{}", if self.dynamic() { "Dynamic".to_string() } else if self.bitrate > 0 { format!("{} Mbps", self.bitrate) } else { "Auto".into() },
                                      if self.dynamic() && self.phone_dynamic == Some(false) { " (phone uses Auto)" } else { "" })),
        ];
        if let Some(profile) = self.h264_profile.as_ref().filter(|p| !p.is_empty()) {
            out.push(("H.264 profile".into(), profile.clone()));
            out.push(("H.264 refresh".into(), if self.intra_refresh { "intra refresh" } else { "keyframes" }.into()));
        }
        if self.wifi.0.is_some() || self.wifi.1.is_some() || self.wifi.2.is_some() {
            out.push(("Phone Wi-Fi".into(), self.wifi_text()));
        }
        out
    }
}

impl StreamOutputPlugin {
    fn pick_ratio(&mut self, host: &mut dyn Host, i: usize) {
        let ratio = self.sizes.ratios[i];
        let current = self.sizes.current();
        self.sizes.ar = i;
        self.sizes.fill_res(ratio);
        if let Some(cur) = current {
            // The size nearest the current height, not the largest: that can be more than the phone's encoder takes
            if let Some(near) = closest_size(cur, self.sizes.by_ratio[&ratio].clone()) {
                self.sizes.res_current = self.sizes.res.iter().position(|s| *s == near).unwrap_or(0);
            }
        }
        self.on_resolution(host); // switching shape is itself a resolution change
    }

    fn set_quality(&mut self, host: &mut dyn Host, q: i64) {
        let q = q.clamp(1, MAX_QUALITY);
        if q == self.quality {
            return;
        }
        self.quality = q;
        self.send(host, json!({"action": "jpeg_quality", "value": q}));
        host.schedule_save();
    }

    fn set_bitrate(&mut self, host: &mut dyn Host, b: i64) {
        let b = b.clamp(0, DYNAMIC_POS);
        if b == self.bitrate {
            return;
        }
        self.bitrate = b;
        self.send(host, json!({"action": "bitrate", "value": self.bitrate_bps()}));
        host.schedule_save();
        host.emit(Event::StreamTransportChanged);
    }
}

#[cfg(test)]
mod tests;
