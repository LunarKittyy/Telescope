//! Stats: a card with the numbers of the stream the panels show, read from the snapshot core puts on the bus once a second. Standard has what a stats panel usually does, Nerd adds the
//! rest. Per-plugin times only get measured while the Nerd view is open and the window is showing (Host::want_stats), and the phone's side comes from its state, which it only measures while the card is up (the `stats` control).

use serde_json::{json, Value};
use telescope_core::bar::{self, BarStat};
use telescope_core::event::Event;
use telescope_core::host::{Host, PluginLook};
use telescope_core::i18n::{n_, number, tr, Fields};
use telescope_core::plugin::{ConfigRejected, Plugin, PluginInfo};
use telescope_core::stats::{DropReason, NamedTiming, Route, Snapshot, SourceSnapshot, Transport};
use crate::stats_phone::{self, PhoneReading};
use telescope_core::ui::{CardSpec, SlintSource, UiEvent, UiSpec};

pub const NAME: &str = "stats";
const PHONE: &str = "phone";
const UI: &str = concat!(include_str!("../ui/parts.slint"), "\n", include_str!("../ui/stats.slint"));

/// The ids this card's readouts go by in the bottom bar. The frame rate, the throughput and the camera are the window's own (core::bar).
const PIN_FRAMES_IN: &str = "stats.frames_in";
const PIN_DECODED: &str = "stats.decoded";
const PIN_REPEATED: &str = "stats.repeated";
const PIN_DROPPED: &str = "stats.dropped";
const PIN_CPU: &str = "stats.cpu";
const PIN_MEMORY: &str = "stats.memory";
const PIN_TOTAL: &str = "stats.total_throughput";
const PIN_CONNECTION: &str = "stats.connection";
const PIN_READING: &str = "stats.reading_camera";
const PIN_STEPS_ON: &str = "stats.steps_on";
const PIN_MOVES: &str = "stats.balancer_moves";

/// How many plugins each of the per-plugin lists shows, slowest first.
const LISTED: usize = 8;
/// How many of the balancer's latest moves are listed.
const MOVES_LISTED: usize = 3;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum View {
    #[default]
    Standard,
    Nerd,
}

impl View {
    fn name(self) -> &'static str {
        match self {
            View::Standard => "standard",
            View::Nerd => "nerd",
        }
    }
}

/// One line of the card: a heading, or a label with its value, a note and a status kind (status_ok, status_warn, status_dim or "").
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Row {
    pub head: bool,
    pub icon: String,
    pub label: String,
    pub value: String,
    pub note: String,
    pub kind: String,
    /// The last minute as Slint path commands in a GRAPH_W by GRAPH_H box, or empty.
    pub graph: String,
    /// The id of the bottom bar readout this line can be pinned as, or empty.
    pub pin: String,
    /// The icon its pin wears in the bar when that isn't the one `pin_icon` knows (a plugin's own), and the name the bar gives it when that isn't the label.
    pub bar_icon: String,
    pub bar_name: String,
}

impl Row {
    pub(crate) fn head(icon: &str, label: &str) -> Row {
        Row { head: true, icon: icon.into(), label: label.into(), ..Row::default() }
    }

    pub(crate) fn line(label: &str, value: String) -> Row {
        Row { label: label.into(), value, ..Row::default() }
    }

    pub(crate) fn note(mut self, note: String) -> Row {
        self.note = note;
        self
    }

    /// The line gets a star, which pins its number into the bottom bar under this id.
    pub(crate) fn pin(mut self, id: &str) -> Row {
        self.pin = id.into();
        self
    }

    /// A plugin's own line: its icon in the bar and a name that says which list it is from.
    pub(crate) fn bar(mut self, icon: &str, name: String) -> Row {
        self.bar_icon = icon.into();
        self.bar_name = name;
        self
    }

    pub(crate) fn kind(mut self, kind: &str) -> Row {
        self.kind = kind.into();
        self
    }

    /// Draws the series next to the value. `floor` is the top of the graph at least, so a steady number stays flat and only a real spike reaches the top.
    pub(crate) fn graph(mut self, series: &[f32], floor: f32) -> Row {
        self.graph = graph(series, floor);
        self
    }

    /// Something only the phone's side can tell, which the protocol doesn't carry yet.
    pub(crate) fn later(label: &str) -> Row {
        Row::line(label, tr("Not available yet")).kind("status_dim")
    }

    fn json(&self, pinned: &[String], locked: bool) -> Value {
        let is_pinned = !self.pin.is_empty() && pinned.contains(&self.pin);
        let locked = locked && self.pin == bar::CAMERA;
        json!({"head": self.head, "icon": self.icon, "label": self.label, "value": self.value, "note": self.note, "kind": self.kind, "graph": self.graph,
            "pin": self.pin, "pinned": is_pinned, "locked": locked && is_pinned})
    }
}

/// The graph's box in px, one pixel for each of the KEPT seconds.
pub const GRAPH_W: usize = telescope_core::stats::KEPT;
pub const GRAPH_H: f32 = 20.0;

/// A series as a line the newest second at the right edge. Nothing for less than two seconds.
fn graph(series: &[f32], floor: f32) -> String {
    if series.len() < 2 {
        return String::new();
    }
    let top = series.iter().copied().fold(floor.max(f32::EPSILON), f32::max);
    let first = GRAPH_W.saturating_sub(series.len());
    let skip = series.len().saturating_sub(GRAPH_W);
    let mut out = String::new();
    for (i, v) in series.iter().skip(skip).enumerate() {
        let x = (first + i) as f32 + 0.5;
        let y = GRAPH_H - 1.0 - (v.max(0.0) / top) * (GRAPH_H - 2.0);
        out.push_str(&format!("{} {x:.1} {y:.1} ", if i == 0 { "M" } else { "L" }));
    }
    out
}

/// The source the card shows: the focused stream's, or the first while none is focused.
fn shown_source<'a>(snap: &'a Snapshot, focused: Option<&String>) -> Option<&'a SourceSnapshot> {
    match focused {
        Some(id) => snap.sources.iter().find(|s| &s.source_id == id),
        None => snap.sources.first(),
    }
}

const SECOND_NS: u64 = 1_000_000_000;

/// The phone's delay readings (when they came in, in monotonic ns, and the delay) as one value for each of the last KEPT seconds up to `now_ns`,
/// the slowest reading in a second, a second with none repeating the one before. Starts at the first reading.
fn delay_series(samples: &[(u64, f32)], now_ns: u64) -> Vec<f32> {
    let mut seconds: Vec<Option<f32>> = vec![None; telescope_core::stats::KEPT];
    for (at, ms) in samples {
        let age = (now_ns.saturating_sub(*at) / SECOND_NS) as usize;
        if let Some(i) = seconds.len().checked_sub(1 + age) {
            seconds[i] = Some(seconds[i].map_or(*ms, |m| m.max(*ms)));
        }
    }
    let mut out = Vec::new();
    let mut last = None;
    for second in seconds {
        last = second.or(last);
        if let Some(v) = last {
            out.push(v);
        }
    }
    out
}

/// The card's fold key (the shell's "<plugin>/<card title in English>").
const FOLD_KEY: &str = "stats/Stats";

/// What the card knows besides the snapshot.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Context {
    /// The Stream output card's bitrate_mbps: 0 is automatic, -1 Dynamic, anything else Mbps. None without that card.
    pub asked_mbps: Option<f64>,
    /// An app is reading the virtual camera, as camera_watched said.
    pub watched: Option<bool>,
    /// What the shown stream's phone said about itself, once it has.
    pub phone: Option<PhoneReading>,
    /// The phone's delay each second of the last minute, oldest first.
    pub phone_delay: Vec<f32>,
    /// The virtual camera's name as the bar reads it, while the shown stream has one.
    pub camera: Option<String>,
    /// How many streams run.
    pub streams: usize,
    /// How the plugins are shown to people, so a list of them names them the way the Plugins tab does.
    pub looks: Vec<PluginLook>,
}

fn fps(v: f32) -> String {
    tr("{n} fps").field("n", number(v as f64, 1, false))
}

fn mbps(v: f64) -> String {
    tr("{n} Mbps").field("n", number(v, 1, false))
}

fn ms(v: f32) -> String {
    tr("{n} ms").field("n", if v < 10.0 { number(v as f64, 1, false) } else { number(v as f64, 0, false) })
}

fn count(n: u64) -> String {
    tr("{n} in all").field("n", n)
}

fn timing_note(max_ms: f32) -> String {
    tr("slowest {n}").field("n", ms(max_ms))
}

/// A stage's name for people, from perf::STAGES.
fn stage_label(stage: &str) -> String {
    match stage {
        "decode" => tr("Decoding"),
        "receive" => tr("Decoder: waiting for output"),
        "take_frame" => tr("Decoder: taking the frame"),
        "transfer" => tr("Decoder: copying the picture"),
        "pipeline" => tr("All frame steps"),
        "upload" => tr("Upload to the graphics card"),
        "submit" => tr("Frame steps on the card"),
        "gpu_wait" => tr("Waiting for the graphics card"),
        "readback" => tr("Back from the graphics card"),
        "camera" => tr("To the virtual camera"),
        other => other.to_string(),
    }
}

fn reason_label(reason: DropReason) -> String {
    match reason {
        DropReason::LostOnLink => tr("Lost on the link"),
        DropReason::Late => tr("Arrived too late"),
        DropReason::DecoderFailed => tr("The decoder failed"),
        DropReason::WaitingForKeyframe => tr("Skipped waiting for a keyframe"),
        DropReason::Paced => tr("Dropped by pacing"),
        DropReason::StepFailed => tr("A frame step failed"),
        DropReason::Unknown => tr("Unknown cause"),
    }
}

/// A drop cause as a phrase for Copy diagnostics, which is always English.
fn drop_cause(reason: DropReason) -> &'static str {
    match reason {
        DropReason::LostOnLink => "lost on the link",
        DropReason::Late => "arrived too late",
        DropReason::DecoderFailed => "failed to decode",
        DropReason::WaitingForKeyframe => "waiting for a keyframe",
        DropReason::Paced => "dropped by pacing",
        DropReason::StepFailed => "lost to a failed frame step",
        DropReason::Unknown => "of unknown cause",
    }
}

/// "privacy_blur" as a name for people.
fn plugin_label(name: &str) -> String {
    let mut chars = name.replace('_', " ").chars().collect::<Vec<_>>();
    if let Some(first) = chars.first_mut() {
        *first = first.to_ascii_uppercase();
    }
    chars.into_iter().collect()
}

fn asked_text(asked: f64) -> String {
    if asked < 0.0 {
        tr("asked for Dynamic")
    } else if asked == 0.0 {
        tr("asked for Automatic")
    } else {
        tr("asked for {n}").field("n", mbps(asked))
    }
}

fn slowest_first(list: &[NamedTiming], listed: usize) -> Vec<&NamedTiming> {
    let mut list: Vec<&NamedTiming> = list.iter().collect();
    list.sort_by(|a, b| b.timing.mean_ms.total_cmp(&a.timing.mean_ms));
    list.truncate(listed);
    list
}

/// The listed plugins by shown name, so a row keeps its place while the timings move.
fn fixed_order<'a>(list: &'a [NamedTiming], looks: &[PluginLook], listed: usize) -> Vec<&'a NamedTiming> {
    let mut list = slowest_first(list, listed);
    list.sort_by_key(|t| plugin_title(&t.name, looks));
    list
}

/// A plugin's name as people read it (what the Plugins tab calls it, translated), or the id tidied up when no plugin by that name is known.
fn plugin_title(name: &str, looks: &[PluginLook]) -> String {
    match looks.iter().find(|l| l.name == name && !l.title.is_empty()) {
        Some(look) => tr(&look.title),
        None => plugin_label(name),
    }
}

/// A plugin's own icon, for its line's pin in the bar; the chip when none is known.
fn plugin_icon(name: &str, looks: &[PluginLook]) -> String {
    looks.iter().find(|l| l.name == name && !l.icon.is_empty()).map_or_else(|| "plug".to_string(), |l| l.icon.clone())
}

/// A drop cause as part of a pin's id: stable whatever the language.
fn drop_key(reason: DropReason) -> &'static str {
    match reason {
        DropReason::LostOnLink => "lost_on_link",
        DropReason::Late => "late",
        DropReason::DecoderFailed => "decoder_failed",
        DropReason::WaitingForKeyframe => "waiting_for_keyframe",
        DropReason::Paced => "paced",
        DropReason::StepFailed => "step_failed",
        DropReason::Unknown => "unknown",
    }
}

/// min / average / max of a series, and how many seconds ago its biggest value was.
fn spread(series: &[f32]) -> Option<(f32, f32, f32, usize)> {
    if series.is_empty() {
        return None;
    }
    let min = series.iter().copied().fold(f32::INFINITY, f32::min);
    let (at, max) = series.iter().copied().enumerate().fold((0, f32::MIN), |best, (i, v)| if v >= best.1 { (i, v) } else { best });
    Some((min, series.iter().sum::<f32>() / series.len() as f32, max, series.len() - 1 - at))
}

fn series_line(name: &str, unit: &str, series: &[f32]) -> Option<(String, String)> {
    let (min, avg, max, ago) = spread(series)?;
    Some((format!("Stats {name}"), format!("min {min:.1}, average {avg:.1}, max {max:.1} {unit}, the max {ago} s ago, over {} s", series.len())))
}

/// The Nerd view as lines for Copy diagnostics: the totals, then each series' spread over the last minute. Plain English, no names.
pub fn summary(snap: &Snapshot, s: &SourceSnapshot, phone: Option<&PhoneReading>) -> Vec<(String, String)> {
    let mut out = vec![
        ("Stats stream".to_string(), format!("{} fps asked, {} s running, {} frames in, {} out, {} dropped", s.target_fps, s.seconds, s.totals.received, s.totals.delivered, s.drops.total())),
    ];
    if s.drops.total() > 0 {
        let causes: Vec<String> = DropReason::ALL.into_iter().filter(|r| s.drops.of(*r) > 0).map(|r| format!("{} {}", s.drops.of(r), drop_cause(r))).collect();
        out.push(("Stats dropped".to_string(), format!("{}, {}", s.drops.total(), causes.join(", "))));
    }
    let q = &s.series;
    let lines = [
        series_line("frames in", "fps", &q.received_fps),
        series_line("frames decoded", "fps", &q.decoded_fps),
        series_line("frames out", "fps", &q.delivered_fps),
        series_line("frames repeated", "fps", &q.repeated_fps),
        series_line("frames dropped", "fps", &q.dropped_fps),
        series_line("bitrate", "Mbps", &q.mbps),
        series_line("decode, slowest each second", "ms", &q.decode_ms),
        series_line("frame steps, slowest each second", "ms", &q.step_ms),
        series_line("virtual camera hand-over, slowest each second", "ms", &q.camera_ms),
        series_line("app processor, of one core", "%", &snap.app.series.cpu_percent),
        series_line("app memory", "MB", &snap.app.series.memory_mb),
    ];
    out.extend(lines.into_iter().flatten());
    let slowest: Vec<String> = slowest_first(&s.plugins, 3).iter().take(3).map(|t| format!("{} {:.1} ms", t.name, t.timing.mean_ms)).collect();
    if !slowest.is_empty() {
        out.push(("Stats slowest plugin steps".to_string(), slowest.join(", ")));
    }
    if let Some(p) = phone {
        out.extend(stats_phone::summary(p));
    }
    out
}

/// The rows for one source's numbers.
pub fn rows(snap: &Snapshot, s: &SourceSnapshot, view: View, ctx: &Context) -> Vec<Row> {
    rows_listing(snap, s, view, ctx, LISTED)
}

/// The rows with each plugin list cut to `listed` lines; the card shows a few, the bar's offers need them all.
fn rows_listing(snap: &Snapshot, s: &SourceSnapshot, view: View, ctx: &Context, listed: usize) -> Vec<Row> {
    let nerd = view == View::Nerd;
    let target = s.target_fps.max(1) as f32;
    let period_ms = 1000.0 / target;
    let mut out = vec![Row::head("video", &tr("Frames"))];
    // Judged only once the stream has had a few seconds to get going
    let judged = s.seconds >= 3;
    let behind = judged && s.now.received_fps < target * 0.85;
    out.push(Row::line(&tr("Frames in"), fps(s.now.received_fps)).kind(if behind { "status_warn" } else { "" })
        .note(tr("asked for {n}").field("n", fps(target))).graph(&s.series.received_fps, target).pin(PIN_FRAMES_IN));
    if nerd {
        out.push(Row::line(&tr("Decoded"), fps(s.now.decoded_fps)).note(count(s.totals.decoded)).graph(&s.series.decoded_fps, target).pin(PIN_DECODED));
    }
    out.push(Row::line(&tr("Frames out"), fps(s.now.delivered_fps)).note(count(s.totals.delivered)).graph(&s.series.delivered_fps, target).pin(bar::FPS));
    out.push(Row::line(&tr("Repeated"), fps(s.now.repeated_fps)).note(count(s.totals.repeated)).graph(&s.series.repeated_fps, target).pin(PIN_REPEATED));

    out.push(Row::head("stop", &tr("Dropped frames")));
    if s.drops.total() == 0 {
        out.push(Row::line(&tr("Dropped"), tr("None")).kind("status_ok").pin(PIN_DROPPED));
    } else {
        out.push(Row::line(&tr("Dropped"), fps(s.now.dropped_fps)).note(count(s.drops.total())).kind(if s.now.dropped_fps > 0.0 { "status_warn" } else { "" }).graph(&s.series.dropped_fps, 3.0).pin(PIN_DROPPED));
        for reason in DropReason::ALL {
            let n = s.drops.of(reason);
            if n > 0 {
                out.push(Row::line(&reason_label(reason), n.to_string()).pin(&format!("stats.dropped_{}", drop_key(reason))));
            }
        }
    }

    out.push(Row::head("connection", &tr("Link")));
    let route = match s.link.route {
        Route::Usb => tr("USB"),
        Route::Network => tr("Wi-Fi or network"),
        Route::Local => tr("This computer"),
        Route::Unknown => tr("Not known"),
    };
    let transport = match s.link.transport {
        Transport::Udp => tr("UDP"),
        Transport::Tcp => tr("TCP"),
        Transport::Local | Transport::Unknown => String::new(),
    };
    out.push(Row::line(&tr("Connection"), route).note(transport).pin(PIN_CONNECTION));
    let mut bitrate = Row::line(&tr("Bitrate"), mbps(s.now.mbps as f64)).graph(&s.series.mbps, 1.0).pin(bar::THROUGHPUT);
    if let (Some(asked), true) = (ctx.asked_mbps, s.link.route != Route::Local) {
        bitrate = bitrate.note(asked_text(asked));
    }
    out.push(bitrate);

    // Only a phone has a side of its own to show; a webcam or the Browser camera is on this computer
    if s.link.route != Route::Local {
        out.push(Row::head("phone", &tr("Phone")));
        match &ctx.phone {
            Some(p) => out.extend(stats_phone::rows(p, nerd, &ctx.phone_delay, period_ms)),
            None => out.push(Row::line(&tr("From the phone"), tr("Not available yet")).kind("status_dim")),
        }
    }
    out.push(Row::head("video", &tr("Virtual camera")));
    let apps = match ctx.watched {
        Some(true) => tr("In use"),
        Some(false) => tr("Nothing"),
        None => tr("Not known"),
    };
    if let Some(name) = ctx.camera.as_ref().filter(|n| !n.is_empty()) {
        out.push(Row::line(&tr("Camera"), name.clone()).pin(bar::CAMERA));
    }
    out.push(Row::line(&tr("Reading the virtual camera"), apps).pin(PIN_READING));

    if nerd {
        out.push(Row::head("chip", &tr("On this computer")));
        let balance = &snap.app.balance;
        let place = if balance.on_card { tr("The graphics card") } else { tr("The processor") };
        out.push(Row::line(&tr("Frame steps run on"), place).note(if balance.pinned { tr("held there on purpose") } else { String::new() }).pin(PIN_STEPS_ON));
        out.push(Row::line(&tr("Balancer moves"), balance.moves.to_string()).pin(PIN_MOVES));
        for (i, m) in balance.recent.iter().rev().take(MOVES_LISTED).enumerate() {
            let label = if m.to_card { tr("Moved to the graphics card") } else { tr("Moved to the processor") };
            let ago = tr("{n} s ago").field("n", number(m.seconds_ago as f64, 0, false));
            let why = match m.why {
                telescope_core::balance::Why::CardBusy => tr("frames waited {n} for the card").field("n", ms(m.wait_ms)),
                telescope_core::balance::Why::CardClear => tr("the card was clear for a while"),
            };
            out.push(Row::line(&label, ago).note(why).pin(&format!("stats.balancer_move_{i}")));
        }
        let period = 1000.0 / target;
        for t in &s.stages {
            let slow = t.timing.mean_ms > period && matches!(t.name.as_str(), "decode" | "pipeline" | "camera");
            let series = match t.name.as_str() {
                "decode" => s.series.decode_ms.as_slice(),
                "pipeline" => s.series.step_ms.as_slice(),
                "camera" => s.series.camera_ms.as_slice(),
                _ => &[],
            };
            out.push(Row::line(&stage_label(&t.name), ms(t.timing.mean_ms)).note(timing_note(t.timing.max_ms)).kind(if slow { "status_warn" } else { "" }).graph(series, period).pin(&format!("stats.stage_{}", t.name)));
        }

        out.push(Row::head("plug", &tr("Each plugin's frame step")));
        if s.plugins.is_empty() {
            out.push(Row::line(&tr("Measuring"), String::new()).kind("status_dim"));
        }
        for t in fixed_order(&s.plugins, &ctx.looks, listed) {
            let label = plugin_title(&t.name, &ctx.looks);
            let bar_name = tr("{plugin}, frame step").field("plugin", label.clone());
            out.push(Row::line(&label, ms(t.timing.mean_ms)).note(timing_note(t.timing.max_ms)).pin(&format!("stats.step_{}", t.name)).bar(&plugin_icon(&t.name, &ctx.looks), bar_name));
        }
        out.push(Row::head("plug", &tr("Each plugin's events and card updates")));
        if snap.app.hooks.is_empty() {
            out.push(Row::line(&tr("Measuring"), String::new()).kind("status_dim"));
        }
        for t in fixed_order(&snap.app.hooks, &ctx.looks, listed) {
            let label = plugin_title(&t.name, &ctx.looks);
            let bar_name = tr("{plugin}, events and card updates").field("plugin", label.clone());
            out.push(Row::line(&label, ms(t.timing.mean_ms)).note(timing_note(t.timing.max_ms)).pin(&format!("stats.hook_{}", t.name)).bar(&plugin_icon(&t.name, &ctx.looks), bar_name));
        }

    }
    out.push(Row::head("monitor", &tr("The app")));
    out.push(match snap.app.cpu_percent {
        Some(c) => Row::line(&tr("Processor"), tr("{n}% of one core").field("n", number(c as f64, 0, false))).graph(&snap.app.series.cpu_percent, 100.0).pin(PIN_CPU),
        None => Row::later(&tr("Processor")),
    });
    out.push(match snap.app.memory_mb {
        Some(m) => Row::line(&tr("Memory"), tr("{n} MB").field("n", number(m as f64, 0, false))).graph(&snap.app.series.memory_mb, 0.0).pin(PIN_MEMORY),
        None => Row::later(&tr("Memory")),
    });
    // The sum of every stream, which stays put whichever one the card shows
    if snap.sources.len() > 1 {
        let total: f32 = snap.sources.iter().map(|x| x.now.mbps).sum();
        out.push(Row::line(&tr("All streams"), mbps(total as f64)).pin(PIN_TOTAL));
    }
    tidy(out)
}

/// The icon a pinned readout wears in the bar, one of its own for each so two numbers never look alike (the fps and throughput icons are the
/// window's, which draws those two itself). A line without one is a text readout: the bar says its name with its value.
fn pin_icon(id: &str) -> &'static str {
    match id {
        PIN_FRAMES_IN => "download",
        PIN_DECODED => "cube",
        PIN_REPEATED => "repeat",
        PIN_DROPPED => "skip_next",
        PIN_CPU => "chip",
        PIN_MEMORY => "memory",
        PIN_TOTAL => "layers",
        PIN_CONNECTION => "link",
        PIN_READING => "eye",
        PIN_STEPS_ON => "layout",
        PIN_MOVES => "compare",
        stats_phone::PIN_DELAY => "timer",
        stats_phone::PIN_WIFI => "wifi",
        stats_phone::PIN_TEMPERATURE => "thermometer",
        stats_phone::PIN_THROTTLING => "alert",
        stats_phone::PIN_CAMERA_FPS => "aperture",
        stats_phone::PIN_ENCODER_FPS => "video",
        stats_phone::PIN_ENCODE => "phone",
        stats_phone::PIN_QUEUE => "send-phone",
        // Every other line (a drop cause, a stage of the pipeline, a balancer move) is a text readout that names itself, and a plugin's line brings its own icon
        _ => "",
    }
}

/// A line the phone or this computer can't measure isn't shown, and a heading with nothing left under it goes too.
fn tidy(rows: Vec<Row>) -> Vec<Row> {
    let missing = tr("Not available yet");
    let lines: Vec<Row> = rows.into_iter().filter(|r| r.head || !(r.kind == "status_dim" && r.value == missing)).collect();
    let mut out: Vec<Row> = Vec::new();
    for (i, row) in lines.iter().enumerate() {
        if row.head && lines.get(i + 1).is_none_or(|next| next.head) {
            continue;
        }
        out.push(row.clone());
    }
    out
}

/// What the bottom bar can show from the card: every line with a "stats." star, as the bar reads it. The window's own readouts (frame
/// rate, throughput, camera) are not offered here, the window has them.
///
/// `s` is the stream the panels show; without it (it is not in the snapshot yet, a second stream that has just started) only the app's own
/// numbers, which don't depend on a stream, are offered.
pub fn offers(snap: &Snapshot, s: Option<&SourceSnapshot>, ctx: &Context) -> Vec<BarStat> {
    let mut out = Vec::new();
    let Some(source) = s.or(snap.sources.first()) else { return out };
    for row in rows_listing(snap, source, View::Nerd, ctx, usize::MAX) {
        let app_wide = matches!(row.pin.as_str(), PIN_CPU | PIN_MEMORY | PIN_TOTAL);
        if !row.head && row.pin.starts_with("stats.") && row.kind != "status_dim" && (s.is_some() || app_wide) {
            let tone = match row.kind.as_str() {
                "status_warn" => "warn",
                _ => "",
            };
            let icon = if row.bar_icon.is_empty() { pin_icon(&row.pin).to_string() } else { row.bar_icon.clone() };
            let name = if row.bar_name.is_empty() { row.label.clone() } else { row.bar_name.clone() };
            // Without an icon the bar has no other way to say what the number is: it reads "Name: value", cut short with the whole of it in the tooltip
            let (value, tip) = if icon.is_empty() { (format!("{}: {}", row.label, row.value), if row.note.is_empty() { format!("{}: {}", row.label, row.value) } else { format!("{}: {}, {}", row.label, row.value, row.note) }) } else { (row.value.clone(), row.note.clone()) };
            out.push(BarStat { id: row.pin.clone(), name, tip, icon, value, sub: String::new(), tone: tone.into() });
        }
    }
    out
}

pub struct StatsPlugin {
    view: View,
    latest: Option<Snapshot>,
    /// The window is on screen, which is when anything shows and the costly numbers are worth taking.
    window_shown: bool,
    watched: Option<bool>,
    wanting: bool,
    /// The shown stream's phone, from its last /v1/state, and how long its delay was each moment since the card came up.
    phone: Option<PhoneReading>,
    phone_delay: Vec<(u64, f32)>,
    /// The stream the phone's readings and the lease belong to: the one the panels show.
    phone_source: Option<String>,
    /// The card is folded, so nothing on it shows.
    folded: bool,
    /// The phone is being asked for its measurements: the card is up with a stream running.
    asking: bool,
    /// A `stats` 1 went out and no 0 has since, so one is owed.
    phone_on: bool,
}

impl Default for StatsPlugin {
    fn default() -> Self {
        StatsPlugin::new()
    }
}

impl StatsPlugin {
    pub fn new() -> StatsPlugin {
        StatsPlugin { view: View::Standard, latest: None, window_shown: true, watched: None, wanting: false, phone: None, phone_delay: Vec::new(), phone_source: None, folded: false, asking: false, phone_on: false }
    }

    pub fn view(&self) -> View {
        self.view
    }

    /// Whether the costly numbers are being asked for.
    pub fn wanting(&self) -> bool {
        self.wanting
    }

    fn props(&self, host: &dyn Host) -> Value {
        let context = self.context(host);
        let mut props = json!({
            "view": if self.view == View::Nerd { 1 } else { 0 },
            "view-standard": tr("Standard"),
            "view-nerd": tr("Nerd"),
            "view-tip": tr("Show every number that can be measured, including what each plugin costs."),
            "empty-detail": tr("Start a stream to see its numbers."),
        });
        let pinned = host.bar_pins();
        let locked = host.stream_count() >= 2;
        let focused = host.focused_source_id();
        let shown = self.latest.as_ref().and_then(|snap| {
            Some((snap, shown_source(snap, focused.as_ref())?))
        });
        match shown {
            Some((snap, source)) => {
                props["rows"] = Value::Array(rows(snap, source, self.view, &context).iter().map(|r| r.json(&pinned, locked)).collect());
                props["empty-text"] = json!("");
                props["source-name"] = json!(if snap.sources.len() > 1 { tr("Stream on virtual camera {n}").field("n", source.slot) } else { String::new() });
            }
            None => {
                props["rows"] = json!([]);
                props["empty-text"] = json!(tr("Not streaming"));
                props["source-name"] = json!("");
            }
        }
        props
    }

    fn context(&self, host: &dyn Host) -> Context {
        Context {
            asked_mbps: host.plugin_config("stream_output").and_then(|c| c.get("bitrate_mbps").and_then(Value::as_f64)),
            watched: self.watched,
            phone: self.phone.clone(),
            phone_delay: delay_series(&self.phone_delay, telescope_core::time::monotonic_ns()),
            camera: host.bar_stat(bar::CAMERA).map(|s| s.value),
            streams: self.latest.as_ref().map_or(0, |snap| snap.sources.len()),
            looks: host.plugin_looks(),
        }
    }

    fn show(&self, host: &mut dyn Host) {
        if self.window_shown {
            let props = self.props(host);
            host.set_ui_props("card", &props);
            self.offer(host);
        }
    }

    /// The numbers the bar can pin, for the shown stream (and the app's, which don't depend on it). Nothing while there is nothing to read.
    fn offer(&self, host: &mut dyn Host) {
        let focused = host.focused_source_id();
        let stats = match self.latest.as_ref() {
            Some(snap) if host.bar_pins().iter().any(|p| p.starts_with("stats.")) => offers(snap, shown_source(snap, focused.as_ref()), &self.context(host)),
            _ => Vec::new(),
        };
        host.offer_bar_stats(&stats);
    }

    /// The card can be seen: the window is up and the card isn't folded.
    fn visible(&self) -> bool {
        self.window_shown && !self.folded
    }

    /// Readings and the lease belong to the stream the panels show: when that changes, the old phone is let go and its numbers go with it.
    fn follow_focus(&mut self, host: &mut dyn Host) {
        let focused = host.focused_source_id();
        if focused == self.phone_source {
            return;
        }
        self.release_phone(host);
        self.phone_source = focused;
        if let Some(id) = &self.phone_source {
            self.phone = host.phone_state(Some(id)).and_then(|state| PhoneReading::from_state(&state));
        }
    }

    /// Lets the current phone go (when it is still streaming to hear it) and forgets its numbers.
    fn release_phone(&mut self, host: &mut dyn Host) {
        if self.phone_on {
            if let Some(id) = self.phone_source.clone().filter(|id| host.is_streaming_from(Some(id))) {
                host.phone_control(Some(&id), &json!({"action": "stats", "value": 0}));
            }
        }
        self.phone_on = false;
        self.phone = None;
        self.phone_delay.clear();
    }

    /// The phone only measures while the card can be seen with a stream running: it's asked every few seconds, and told to stop when that ends.
    fn ask_phone(&mut self, host: &mut dyn Host) {
        self.follow_focus(host);
        let want = (self.visible() || self.pinned(host, "stats.phone_")) && host.is_streaming();
        if want {
            if !self.asking {
                self.asking = true;
                host.schedule(stats_phone::ASK_MS, PHONE);
                host.request_phone_state(None);
            }
            self.send_phone(host, true);
        } else {
            if self.asking {
                self.asking = false;
                host.cancel_schedule(PHONE);
            }
            self.send_phone(host, false);
        }
    }

    /// A reading of the shown phone's state: the card's numbers, and the lease renewed while it is wanted.
    fn take_phone_state(&mut self, host: &mut dyn Host, state: &Value) {
        self.follow_focus(host);
        self.phone = PhoneReading::from_state(state);
        if let Some(ms) = self.phone.as_ref().and_then(PhoneReading::phone_delay_ms) {
            let now = telescope_core::time::monotonic_ns();
            self.phone_delay.push((now, ms as f32));
            self.phone_delay.retain(|(at, _)| now.saturating_sub(*at) < telescope_core::stats::KEPT as u64 * SECOND_NS);
        }
        if self.asking {
            self.send_phone(host, true);
        }
        self.show(host);
    }

    /// An older phone doesn't take the control, so nothing is sent until its state says it does.
    fn send_phone(&mut self, host: &mut dyn Host, on: bool) {
        let source = self.phone_source.clone();
        if on {
            if self.phone.as_ref().is_some_and(|p| p.takes_stats) {
                host.phone_control(source.as_deref(), &json!({"action": "stats", "value": 1}));
                self.phone_on = true;
            }
        } else if self.phone_on {
            host.phone_control(source.as_deref(), &json!({"action": "stats", "value": 0}));
            self.phone_on = false;
        }
    }

    /// Whether a number of this card is pinned into the bar (ids starting with `prefix`), which keeps it measured while the window is up.
    fn pinned(&self, host: &dyn Host, prefix: &str) -> bool {
        self.window_shown && host.bar_pins().iter().any(|p| p.starts_with(prefix))
    }

    /// The costly numbers are taken while the Nerd view can be seen, and while any of this card's numbers is pinned into the bar.
    fn want(&mut self, host: &mut dyn Host) {
        let want = (self.visible() && self.view == View::Nerd) || self.pinned(host, "stats.");
        if want != self.wanting {
            self.wanting = want;
            host.want_stats(NAME, want);
        }
    }
}

impl Plugin for StatsPlugin {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: NAME.into(),
            title: n_("Stats").into(),
            about: n_("Numbers about the stream as it runs: frames in and out, dropped frames and why, the link and bitrate, how long each step takes and what the app uses.").into(),
            when_off: n_("Without it there's no Stats card. The stream carries on the same.").into(),
            optional: true,
            category: "streaming".into(),
            icon: "chip".into(),
            ..Default::default()
        }
    }

    fn card(&mut self) -> Option<CardSpec> {
        let props = json!({"view": 0, "empty-text": tr("Not streaming"), "empty-detail": tr("Start a stream to see its numbers."), "rows": []});
        Some(CardSpec { ui: UiSpec { source: SlintSource::Text(UI.into()), component: "StatsCard".into(), props }, region: "right".into() })
    }

    fn on_plugins_ready(&mut self, host: &mut dyn Host) {
        self.want(host);
        self.ask_phone(host);
        self.show(host);
    }

    fn on_event(&mut self, host: &mut dyn Host, event: &Event) {
        match event {
            Event::StatsUpdated { snapshot } => {
                self.latest = Some(snapshot.clone());
                self.follow_focus(host);
                self.show(host);
            }
            Event::CameraWatched { watched } => self.watched = Some(*watched),
            // A pin from the bar's own menu or from another card: the stars follow, and what the bar wants read changes
            Event::BarPinsChanged { .. } => {
                self.want(host);
                self.ask_phone(host);
                self.show(host);
            }
            Event::PhoneStateUpdated { state } => self.take_phone_state(host, state),
            // The answers to this card's own asks come back as polls, for the shown stream only
            Event::PhonePolled { shown: true, state, .. } => self.take_phone_state(host, state),
            Event::TimerFired { token, .. } if token == PHONE => {
                self.asking = false;
                self.ask_phone(host);
            }
            Event::WindowShown { shown } => {
                self.window_shown = *shown;
                self.want(host);
                self.ask_phone(host);
                self.show(host);
            }
            Event::CardFolded { key, folded } if key == FOLD_KEY => {
                self.folded = *folded;
                self.want(host);
                self.ask_phone(host);
                self.show(host);
            }
            _ => {}
        }
    }

    fn on_stream_start(&mut self, host: &mut dyn Host, _stream: &telescope_core::plugin::StreamInfo) {
        self.ask_phone(host);
    }

    fn on_stream_stop(&mut self, host: &mut dyn Host) {
        if !host.is_streaming() {
            self.latest = None;
            self.phone = None;
            self.phone_delay.clear();
            self.phone_on = false;
            self.phone_source = None;
            self.ask_phone(host);
            self.show(host);
        }
    }

    fn on_focus_gained(&mut self, host: &mut dyn Host, _stream: &telescope_core::plugin::StreamInfo) {
        self.ask_phone(host);
        self.show(host);
    }

    fn on_focus_lost(&mut self, host: &mut dyn Host) {
        self.ask_phone(host);
        self.show(host);
    }

    fn on_ui(&mut self, host: &mut dyn Host, event: &UiEvent) {
        if event.ui == "card" && event.callback == "pin-toggled" {
            if let Some(id) = event.args.first().and_then(Value::as_str) {
                host.pin_to_bar(id, event.args.get(1).and_then(Value::as_bool).unwrap_or(true));
            }
            self.show(host);
            return;
        }
        if event.ui == "card" && event.callback == "view-picked" {
            let view = if event.args.first().and_then(Value::as_i64) == Some(1) { View::Nerd } else { View::Standard };
            if view != self.view {
                self.view = view;
                host.schedule_save();
            }
            self.want(host);
            self.show(host);
        }
    }

    fn shutdown(&mut self, host: &mut dyn Host) {
        self.window_shown = false;
        host.offer_bar_stats(&[]);
        self.ask_phone(host);
        if self.wanting {
            self.wanting = false;
            host.want_stats(NAME, false);
        }
    }

    fn diagnostics(&self) -> Vec<(String, String)> {
        match self.latest.as_ref().and_then(|snap| Some((snap, shown_source(snap, self.phone_source.as_ref())?))) {
            Some((snap, source)) if self.view == View::Nerd => summary(snap, source, self.phone.as_ref()),
            _ => Vec::new(),
        }
    }

    fn get_config(&self) -> Value {
        json!({"view": self.view.name()})
    }

    fn set_config(&mut self, cfg: &Value) -> Result<(), ConfigRejected> {
        self.view = match cfg.get("view").and_then(Value::as_str) {
            Some("nerd") => View::Nerd,
            _ => View::Standard,
        };
        Ok(())
    }

}

#[cfg(test)]
#[path = "stats_tests.rs"]
mod tests;

