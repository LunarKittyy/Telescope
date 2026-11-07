//! Stats: a card with the numbers of the stream the panels show, read from the snapshot core puts on the bus once a second. Standard has what a stats panel usually does, Nerd adds the
//! rest. Per-plugin times only get measured while the Nerd view is open and the window is showing (Host::want_stats), and what needs the phone's side shows as not available yet.

use serde_json::{json, Value};
use telescope_core::event::Event;
use telescope_core::host::Host;
use telescope_core::i18n::{n_, number, tr, Fields};
use telescope_core::plugin::{ConfigRejected, Plugin, PluginInfo};
use telescope_core::stats::{DropReason, NamedTiming, Route, Snapshot, SourceSnapshot, Transport};
use telescope_core::ui::{CardSpec, SlintSource, UiEvent, UiSpec};

pub const NAME: &str = "stats";
const UI: &str = concat!(include_str!("../ui/parts.slint"), "\n", include_str!("../ui/stats.slint"));

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
}

impl Row {
    fn head(icon: &str, label: &str) -> Row {
        Row { head: true, icon: icon.into(), label: label.into(), ..Row::default() }
    }

    fn line(label: &str, value: String) -> Row {
        Row { label: label.into(), value, ..Row::default() }
    }

    fn note(mut self, note: String) -> Row {
        self.note = note;
        self
    }

    fn kind(mut self, kind: &str) -> Row {
        self.kind = kind.into();
        self
    }

    /// Draws the series next to the value. `floor` is the top of the graph at least, so a steady number stays flat and only a real spike reaches the top.
    fn graph(mut self, series: &[f32], floor: f32) -> Row {
        self.graph = graph(series, floor);
        self
    }

    /// Something only the phone's side can tell, which the protocol doesn't carry yet.
    fn later(label: &str) -> Row {
        Row::line(label, tr("Not available yet")).kind("status_dim")
    }

    fn json(&self) -> Value {
        json!({"head": self.head, "icon": self.icon, "label": self.label, "value": self.value, "note": self.note, "kind": self.kind, "graph": self.graph})
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

/// What the card knows besides the snapshot.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Context {
    /// The Stream output card's bitrate_mbps: 0 is automatic, -1 Dynamic, anything else Mbps. None without that card.
    pub asked_mbps: Option<f64>,
    /// An app is reading the virtual camera, as camera_watched said.
    pub watched: Option<bool>,
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
        "camera" => tr("Handing the frame to the virtual camera"),
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

fn slowest_first(list: &[NamedTiming]) -> Vec<&NamedTiming> {
    let mut list: Vec<&NamedTiming> = list.iter().collect();
    list.sort_by(|a, b| b.timing.mean_ms.total_cmp(&a.timing.mean_ms));
    list.truncate(LISTED);
    list
}

/// What is slowing a stream down, as far as its numbers can tell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Not enough running yet to say.
    Warming,
    /// Nothing is slow and nothing is dropping.
    Fine,
    /// Frames are lost on the link, over Wi-Fi or the network.
    LinkLosing,
    /// Frames come in below the asked rate, with nothing lost: the phone isn't sending them.
    PhoneSlow,
    /// A step on this computer takes longer than a frame lasts: the stage's name from perf::STAGES.
    ComputerSlow(&'static str),
    /// Frames are dropped, mostly for this reason.
    Dropping(DropReason),
}

/// How many of the latest seconds count as "now" when looking for drops.
const RECENT_S: usize = 10;

impl Verdict {
    /// A short word for Copy diagnostics, which is always English.
    pub fn code(self) -> String {
        match self {
            Verdict::Warming => "warming up".into(),
            Verdict::Fine => "nothing slow".into(),
            Verdict::LinkLosing => "frames lost on the link".into(),
            Verdict::PhoneSlow => "frames arrive below the asked rate".into(),
            Verdict::ComputerSlow(stage) => format!("computer slow in {stage}"),
            Verdict::Dropping(r) => format!("frames dropped, mostly {r:?}"),
        }
    }

    /// The line the card shows and its status kind.
    pub fn text(self) -> (String, &'static str) {
        match self {
            Verdict::Warming => (tr("Gathering numbers"), "status_dim"),
            Verdict::Fine => (tr("Nothing is slowing the stream down."), "status_ok"),
            Verdict::LinkLosing => (tr("The link is losing frames. A weaker Wi-Fi signal, a busy network or a USB cable that isn't seated well can do that."), "status_warn"),
            Verdict::PhoneSlow => (tr("Frames arrive below the asked rate and none are lost, so the phone isn't sending them fast enough."), "status_warn"),
            Verdict::ComputerSlow(stage) => (tr("This computer is the slow part: {step} takes longer than a frame lasts.").field("step", stage_label(stage).to_lowercase()), "status_warn"),
            Verdict::Dropping(reason) => (tr("Frames are being dropped, mostly because of: {reason}.").field("reason", reason_label(reason).to_lowercase()), "status_warn"),
        }
    }
}

/// Works out what is slowing one source down, the computer's steps first since those are the ones a person can change.
pub fn verdict(s: &SourceSnapshot) -> Verdict {
    if s.seconds < 3 {
        return Verdict::Warming;
    }
    let target = s.target_fps.max(1) as f32;
    let period = 1000.0 / target;
    if let Some(t) = s.stages.iter().find(|t| matches!(t.name.as_str(), "decode" | "pipeline" | "camera") && t.timing.mean_ms > period) {
        let stage = telescope_core::perf::STAGES.iter().find(|n| **n == t.name).copied().unwrap_or("pipeline");
        return Verdict::ComputerSlow(stage);
    }
    let recent = |series: &[f32]| series.iter().rev().take(RECENT_S).sum::<f32>();
    if recent(&s.series.dropped_fps) > 0.0 {
        let worst = DropReason::ALL.into_iter().max_by_key(|r| s.drops.of(*r)).unwrap_or(DropReason::Unknown);
        return if worst == DropReason::LostOnLink { Verdict::LinkLosing } else { Verdict::Dropping(worst) };
    }
    if s.now.received_fps < target * 0.85 && s.link.route != Route::Local {
        return Verdict::PhoneSlow;
    }
    Verdict::Fine
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

/// The Nerd view as lines for Copy diagnostics: the verdict, then each series' spread over the last minute. Plain English, no names.
pub fn summary(snap: &Snapshot, s: &SourceSnapshot) -> Vec<(String, String)> {
    let mut out = vec![
        ("Stats verdict".to_string(), verdict(s).code()),
        ("Stats stream".to_string(), format!("{} fps asked, {} s running, {} frames in, {} out, {} dropped", s.target_fps, s.seconds, s.totals.received, s.totals.delivered, s.drops.total())),
    ];
    let reasons: Vec<String> = DropReason::ALL.into_iter().filter(|r| s.drops.of(*r) > 0).map(|r| format!("{r:?} {}", s.drops.of(r))).collect();
    if !reasons.is_empty() {
        out.push(("Stats drops".to_string(), reasons.join(", ")));
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
    let slowest: Vec<String> = slowest_first(&s.plugins).iter().take(3).map(|t| format!("{} {:.1} ms", t.name, t.timing.mean_ms)).collect();
    if !slowest.is_empty() {
        out.push(("Stats slowest plugin steps".to_string(), slowest.join(", ")));
    }
    out
}

/// The rows for one source's numbers.
pub fn rows(snap: &Snapshot, s: &SourceSnapshot, view: View, ctx: &Context) -> Vec<Row> {
    let nerd = view == View::Nerd;
    let target = s.target_fps.max(1) as f32;
    let mut out = vec![Row::head("video", &tr("Frames"))];
    // Judged only once the stream has had a few seconds to get going
    let judged = s.seconds >= 3;
    let behind = judged && s.now.received_fps < target * 0.85;
    out.push(Row::line(&tr("Frames in"), fps(s.now.received_fps)).kind(if behind { "status_warn" } else { "" })
        .note(tr("asked for {n}").field("n", fps(target))).graph(&s.series.received_fps, target));
    if nerd {
        out.push(Row::line(&tr("Decoded"), fps(s.now.decoded_fps)).note(count(s.totals.decoded)).graph(&s.series.decoded_fps, target));
    }
    out.push(Row::line(&tr("Frames out"), fps(s.now.delivered_fps)).note(count(s.totals.delivered)).graph(&s.series.delivered_fps, target));
    out.push(Row::line(&tr("Repeated"), fps(s.now.repeated_fps)).note(count(s.totals.repeated)).graph(&s.series.repeated_fps, target));

    out.push(Row::head("stop", &tr("Dropped frames")));
    if s.drops.total() == 0 {
        out.push(Row::line(&tr("Dropped"), tr("None")).kind("status_ok"));
    } else {
        out.push(Row::line(&tr("Dropped"), fps(s.now.dropped_fps)).note(count(s.drops.total())).kind(if s.now.dropped_fps > 0.0 { "status_warn" } else { "" }).graph(&s.series.dropped_fps, 3.0));
        for reason in DropReason::ALL {
            let n = s.drops.of(reason);
            if n > 0 {
                out.push(Row::line(&reason_label(reason), n.to_string()));
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
    out.push(Row::line(&tr("Connection"), route).note(transport));
    let mut bitrate = Row::line(&tr("Bitrate"), mbps(s.now.mbps as f64)).graph(&s.series.mbps, 1.0);
    if let (Some(asked), true) = (ctx.asked_mbps, s.link.route != Route::Local) {
        bitrate = bitrate.note(asked_text(asked));
    }
    out.push(bitrate);

    out.push(Row::head("phone", &tr("Phone")));
    // Everything here needs the phone app, so it's one line instead of a row of the same answer each
    let missing = if nerd {
        tr("The delay from the camera, Wi-Fi band and signal, temperature and throttling, camera frame rate, encoding time and queue, and round trip, jitter and clock offset")
    } else {
        tr("The delay from the camera, Wi-Fi band and signal, and temperature and throttling")
    };
    out.push(Row::line(&tr("From the phone"), tr("Not available yet")).kind("status_dim").note(missing));
    let apps = match ctx.watched {
        Some(true) => tr("In use"),
        Some(false) => tr("Nothing"),
        None => tr("Not known"),
    };
    out.push(Row::line(&tr("Reading the virtual camera"), apps).note(tr("Naming the apps isn't possible yet")));

    if nerd {
        out.push(Row::head("chip", &tr("On this computer")));
        let balance = &snap.app.balance;
        let place = if balance.on_card { tr("The graphics card") } else { tr("The processor") };
        out.push(Row::line(&tr("Frame steps run on"), place).note(if balance.pinned { tr("held there on purpose") } else { String::new() }));
        out.push(Row::line(&tr("Balancer moves"), balance.moves.to_string()));
        for m in balance.recent.iter().rev().take(MOVES_LISTED) {
            let label = if m.to_card { tr("Moved to the graphics card") } else { tr("Moved to the processor") };
            let ago = tr("{n} s ago").field("n", number(m.seconds_ago as f64, 0, false));
            let why = match m.why {
                telescope_core::balance::Why::CardBusy => tr("frames waited {n} for the card").field("n", ms(m.wait_ms)),
                telescope_core::balance::Why::CardClear => tr("the card was clear for a while"),
            };
            out.push(Row::line(&label, ago).note(why));
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
            out.push(Row::line(&stage_label(&t.name), ms(t.timing.mean_ms)).note(timing_note(t.timing.max_ms)).kind(if slow { "status_warn" } else { "" }).graph(series, period));
        }

        out.push(Row::head("plug", &tr("Each plugin's frame step")));
        if s.plugins.is_empty() {
            out.push(Row::line(&tr("Measuring"), "…".into()).kind("status_dim"));
        }
        for t in slowest_first(&s.plugins) {
            out.push(Row::line(&plugin_label(&t.name), ms(t.timing.mean_ms)).note(timing_note(t.timing.max_ms)));
        }
        out.push(Row::head("plug", &tr("Each plugin's events and card updates")));
        if snap.app.hooks.is_empty() {
            out.push(Row::line(&tr("Measuring"), "…".into()).kind("status_dim"));
        }
        for t in slowest_first(&snap.app.hooks) {
            out.push(Row::line(&plugin_label(&t.name), ms(t.timing.mean_ms)).note(timing_note(t.timing.max_ms)));
        }

        out.push(Row::head("monitor", &tr("The app")));
        out.push(match snap.app.cpu_percent {
            Some(c) => Row::line(&tr("Processor"), tr("{n}% of one core").field("n", number(c as f64, 0, false))).graph(&snap.app.series.cpu_percent, 100.0),
            None => Row::later(&tr("Processor")),
        });
        out.push(match snap.app.memory_mb {
            Some(m) => Row::line(&tr("Memory"), tr("{n} MB").field("n", number(m as f64, 0, false))).graph(&snap.app.series.memory_mb, 0.0),
            None => Row::later(&tr("Memory")),
        });
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
}

impl Default for StatsPlugin {
    fn default() -> Self {
        StatsPlugin::new()
    }
}

impl StatsPlugin {
    pub fn new() -> StatsPlugin {
        StatsPlugin { view: View::Standard, latest: None, window_shown: true, watched: None, wanting: false }
    }

    pub fn view(&self) -> View {
        self.view
    }

    /// Whether the costly numbers are being asked for.
    pub fn wanting(&self) -> bool {
        self.wanting
    }

    fn props(&self, host: &dyn Host) -> Value {
        let context = Context {
            asked_mbps: host.plugin_config("stream_output").and_then(|c| c.get("bitrate_mbps").and_then(Value::as_f64)),
            watched: self.watched,
        };
        let mut props = json!({
            "view": if self.view == View::Nerd { 1 } else { 0 },
            "view-standard": tr("Standard"),
            "view-nerd": tr("Nerd"),
            "view-tip": tr("Every number that can be measured, including what each plugin costs"),
        });
        let focused = host.focused_source_id();
        let shown = self.latest.as_ref().and_then(|snap| {
            let source = snap.sources.iter().find(|s| Some(&s.source_id) == focused.as_ref()).or_else(|| snap.sources.first())?;
            Some((snap, source))
        });
        match shown {
            Some((snap, source)) => {
                let (text, kind) = verdict(source).text();
                props["verdict"] = json!(text);
                props["verdict-kind"] = json!(kind);
                props["rows"] = Value::Array(rows(snap, source, self.view, &context).iter().map(Row::json).collect());
                props["empty-text"] = json!("");
                props["source-name"] = json!(if snap.sources.len() > 1 { tr("Stream on virtual camera {n}").field("n", source.slot) } else { String::new() });
            }
            None => {
                props["verdict"] = json!("");
                props["rows"] = json!([]);
                props["empty-text"] = json!(tr("Start a stream to see its numbers."));
                props["source-name"] = json!("");
            }
        }
        props
    }

    fn show(&self, host: &mut dyn Host) {
        if self.window_shown {
            let props = self.props(host);
            host.set_ui_props("card", &props);
        }
    }

    /// The costly numbers are only taken while the Nerd view can be seen.
    fn want(&mut self, host: &mut dyn Host) {
        let want = self.window_shown && self.view == View::Nerd;
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
        let props = json!({"view": 0, "empty-text": tr("Start a stream to see its numbers."), "rows": []});
        Some(CardSpec { ui: UiSpec { source: SlintSource::Text(UI.into()), component: "StatsCard".into(), props }, region: "right".into() })
    }

    fn on_plugins_ready(&mut self, host: &mut dyn Host) {
        self.want(host);
        self.show(host);
    }

    fn on_event(&mut self, host: &mut dyn Host, event: &Event) {
        match event {
            Event::StatsUpdated { snapshot } => {
                self.latest = Some(snapshot.clone());
                self.show(host);
            }
            Event::CameraWatched { watched } => self.watched = Some(*watched),
            Event::WindowShown { shown } => {
                self.window_shown = *shown;
                self.want(host);
                self.show(host);
            }
            _ => {}
        }
    }

    fn on_stream_stop(&mut self, host: &mut dyn Host) {
        if !host.is_streaming() {
            self.latest = None;
            self.show(host);
        }
    }

    fn on_focus_gained(&mut self, host: &mut dyn Host, _stream: &telescope_core::plugin::StreamInfo) {
        self.show(host);
    }

    fn on_ui(&mut self, host: &mut dyn Host, event: &UiEvent) {
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
        if self.wanting {
            self.wanting = false;
            host.want_stats(NAME, false);
        }
    }

    fn diagnostics(&self) -> Vec<(String, String)> {
        match self.latest.as_ref().and_then(|snap| Some((snap, snap.sources.first()?))) {
            Some((snap, source)) if self.view == View::Nerd => summary(snap, source),
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
