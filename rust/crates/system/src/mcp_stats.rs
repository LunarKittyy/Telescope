//! What the MCP server's stats and watch tools say: the numbers of the snapshot core puts on the bus once a second (`core::stats`), as
//! min, average and max over a window with the worst moments and when they happened. It reads only core's snapshot, never the Stats
//! plugin, so it works with the panel switched off.

use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use telescope_core::stats::{DropReason, Drops, Snapshot, SourceSeries, SourceSnapshot};

/// The series the Stats panel's Standard view shows, and the ones Stats for nerds adds.
pub const STANDARD: [&str; 5] = ["received_fps", "delivered_fps", "repeated_fps", "dropped_fps", "mbps"];
pub const NERD_EXTRA: [&str; 4] = ["decoded_fps", "decode_ms", "step_ms", "camera_ms"];
/// The app's own series (nerd only).
pub const APP_SERIES: [&str; 2] = ["cpu_percent", "memory_mb"];
/// How many moments are named for each series.
const WORST: usize = 3;
/// A longer wait than this isn't offered: it would hold the connection for minutes.
pub const WATCH_MAX_SECONDS: u64 = 120;
/// What one snapshot holds of each series, so a stats call can't ask for more.
pub const WINDOW_MAX: usize = telescope_core::stats::KEPT;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Standard,
    Nerd,
}

impl Level {
    pub fn parse(text: Option<&str>) -> Result<Level, String> {
        match text {
            None | Some("standard") => Ok(Level::Standard),
            Some("nerd") => Ok(Level::Nerd),
            Some(other) => Err(format!("level is standard or nerd, not {other:?}.")),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Level::Standard => "standard",
            Level::Nerd => "nerd",
        }
    }
}

fn source_series<'a>(s: &'a SourceSeries, name: &str) -> Option<&'a Vec<f32>> {
    Some(match name {
        "received_fps" => &s.received_fps,
        "decoded_fps" => &s.decoded_fps,
        "delivered_fps" => &s.delivered_fps,
        "repeated_fps" => &s.repeated_fps,
        "dropped_fps" => &s.dropped_fps,
        "mbps" => &s.mbps,
        "decode_ms" => &s.decode_ms,
        "step_ms" => &s.step_ms,
        "camera_ms" => &s.camera_ms,
        _ => return None,
    })
}

/// For these a low number is the bad one (the picture slowed down); for the rest a high one is.
fn low_is_bad(name: &str) -> bool {
    matches!(name, "received_fps" | "decoded_fps" | "delivered_fps" | "mbps")
}

fn unit(name: &str) -> &'static str {
    if name.ends_with("_fps") {
        "frames per second"
    } else if name.ends_with("_ms") {
        "ms (the slowest in each second)"
    } else if name == "mbps" {
        "megabits per second"
    } else if name == "cpu_percent" {
        "percent of one core"
    } else {
        "MB"
    }
}

fn round(v: f32) -> f64 {
    (v as f64 * 100.0).round() / 100.0
}

/// Min, average and max of one series (oldest first, one value a second) and the few worst moments, with how long before the newest sample each was.
pub fn summarise(name: &str, values: &[f32]) -> Value {
    if values.is_empty() {
        return json!({"unit": unit(name), "samples": 0});
    }
    let (mut min, mut max, mut sum) = (f32::MAX, f32::MIN, 0.0f64);
    for &v in values {
        min = min.min(v);
        max = max.max(v);
        sum += v as f64;
    }
    let mut order: Vec<usize> = (0..values.len()).collect();
    order.sort_by(|&a, &b| {
        let (x, y) = (values[a], values[b]);
        if low_is_bad(name) { x.total_cmp(&y) } else { y.total_cmp(&x) }
    });
    // Only moments that were worse than the average are worth naming: a steady series has none
    let average = sum / values.len() as f64;
    let mut worst: Vec<usize> = order.into_iter().filter(|&i| if low_is_bad(name) { (values[i] as f64) < average } else { values[i] as f64 > average }).take(WORST).collect();
    worst.sort_unstable();
    let moments: Vec<Value> = worst.into_iter().map(|i| json!({"seconds_ago": values.len() - 1 - i, "value": round(values[i])})).collect();
    json!({"unit": unit(name), "samples": values.len(), "min": round(min), "average": round((sum / values.len() as f64) as f32), "max": round(max), "worst_moments": moments})
}

/// The series of one run of numbers, by stream and name, one value a second, oldest first.
#[derive(Default)]
pub struct Collected {
    sources: BTreeMap<String, BTreeMap<&'static str, Vec<f32>>>,
    app: BTreeMap<&'static str, Vec<f32>>,
    /// Each stream's drop totals when the numbers began, to say what was dropped since.
    drops_at_start: BTreeMap<String, Drops>,
    /// The newest snapshot, which has everything that isn't a series.
    latest: Option<Snapshot>,
    /// Each stream's last snapshot while it was there, in the order they first showed up, so a stream that stopped part way is still reported.
    seen: Vec<SourceSnapshot>,
    pub seconds: usize,
}

fn names(level: Level) -> Vec<&'static str> {
    let mut all: Vec<&'static str> = STANDARD.to_vec();
    if level == Level::Nerd {
        all.extend(NERD_EXTRA);
    }
    all
}

impl Collected {
    /// The last `window` seconds of a snapshot.
    pub fn from_snapshot(snapshot: &Snapshot, window: usize) -> Collected {
        let mut c = Collected { seconds: window, ..Collected::default() };
        for source in &snapshot.sources {
            let mut by_name = BTreeMap::new();
            for name in STANDARD.iter().chain(&NERD_EXTRA) {
                if let Some(values) = source_series(&source.series, name) {
                    by_name.insert(*name, values[values.len().saturating_sub(window)..].to_vec());
                }
            }
            // The totals a window before the newest sample: the stream's start when it has been running for less
            let totals = &source.series.drop_totals;
            let start = if totals.len() > window { totals[totals.len() - window - 1] } else { Drops::default() };
            c.drops_at_start.insert(source.source_id.clone(), start);
            c.sources.insert(source.source_id.clone(), by_name);
        }
        let cut = |v: &Vec<f32>| v[v.len().saturating_sub(window)..].to_vec();
        c.app.insert("cpu_percent", cut(&snapshot.app.series.cpu_percent));
        c.app.insert("memory_mb", cut(&snapshot.app.series.memory_mb));
        c.seen = snapshot.sources.clone();
        c.latest = Some(snapshot.clone());
        c
    }

    /// Adds the newest second of a snapshot, as watch does once a second. The first one it sees is where the drops are counted from.
    pub fn push(&mut self, snapshot: &Snapshot) {
        for source in &snapshot.sources {
            let known = self.sources.contains_key(&source.source_id);
            let by_name = self.sources.entry(source.source_id.clone()).or_default();
            for name in STANDARD.iter().chain(&NERD_EXTRA) {
                if let Some(last) = source_series(&source.series, name).and_then(|v| v.last()) {
                    by_name.entry(*name).or_default().push(*last);
                }
            }
            if !known {
                // What this stream had dropped up to the first second it was seen in doesn't belong to the watch
                self.drops_at_start.insert(source.source_id.clone(), source.drops);
            }
            match self.seen.iter_mut().find(|s| s.source_id == source.source_id) {
                Some(last) => *last = source.clone(),
                None => self.seen.push(source.clone()),
            }
        }
        if let Some(v) = snapshot.app.series.cpu_percent.last() {
            self.app.entry("cpu_percent").or_default().push(*v);
        }
        if let Some(v) = snapshot.app.series.memory_mb.last() {
            self.app.entry("memory_mb").or_default().push(*v);
        }
        self.seconds += 1;
        self.latest = Some(snapshot.clone());
    }

    /// Nothing was collected: no stream was ever in a snapshot.
    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }

    /// The report: per stream and for the app, over the seconds collected. `only` limits it to series of these names (empty: all of the level).
    pub fn report(&self, level: Level, only: &[String]) -> Value {
        let Some(latest) = &self.latest else { return json!({}) };
        let wanted = |name: &str| only.is_empty() || only.iter().any(|n| n == name);
        let sources: Vec<Value> = self.seen.iter().map(|s| self.source_report(s, level, &wanted)).collect();
        let mut app = Map::new();
        if level == Level::Nerd {
            for name in APP_SERIES.iter().filter(|n| wanted(n)) {
                app.insert((*name).into(), summarise(name, self.app.get(name).map(Vec::as_slice).unwrap_or(&[])));
            }
            app.insert("balance".into(), serde_json::to_value(&latest.app.balance).unwrap_or(Value::Null));
            app.insert("hooks_ms".into(), serde_json::to_value(&latest.app.hooks).unwrap_or(Value::Null));
        } else {
            app.insert("cpu_percent".into(), json!(latest.app.cpu_percent.map(round)));
            app.insert("memory_mb".into(), json!(latest.app.memory_mb.map(round)));
        }
        json!({"level": level.name(), "seconds": self.seconds, "streams": sources, "app": app})
    }

    fn source_report(&self, s: &SourceSnapshot, level: Level, wanted: &dyn Fn(&str) -> bool) -> Value {
        let mut series = Map::new();
        if let Some(by_name) = self.sources.get(&s.source_id) {
            for name in names(level).into_iter().filter(|n| wanted(n)) {
                series.insert(name.into(), summarise(name, by_name.get(name).map(Vec::as_slice).unwrap_or(&[])));
            }
        }
        let start = self.drops_at_start.get(&s.source_id).copied().unwrap_or_default();
        let dropped = s.drops.since(&start);
        let reasons = |d: &Drops| -> Value {
            Value::Object(DropReason::ALL.iter().filter(|r| d.of(**r) > 0).map(|r| (serde_json::to_value(r).ok().and_then(|v| v.as_str().map(String::from)).unwrap_or_default(), json!(d.of(*r)))).collect())
        };
        let mut out = json!({
            "source": s.source_id,
            "running_seconds": s.seconds,
            "link": s.link,
            "target_fps": s.target_fps,
            "now": s.now,
            "frames_since_start": s.totals,
            "dropped_in_window": {"total": dropped.total(), "by_reason": reasons(&dropped)},
            "dropped_since_start": {"total": s.drops.total(), "by_reason": reasons(&s.drops)},
            "series": series,
        });
        if level == Level::Nerd {
            out["stages_last_second"] = serde_json::to_value(&s.stages).unwrap_or(Value::Null);
            out["plugin_frame_steps_last_second"] = if latest_detailed(self) { serde_json::to_value(&s.plugins).unwrap_or(Value::Null) } else { json!("not measured yet") };
        }
        out
    }
}

fn latest_detailed(c: &Collected) -> bool {
    c.latest.as_ref().is_some_and(|l| l.detailed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use telescope_core::stats::{Counts, NamedTiming, Rates, Timing};

    fn source(id: &str, fps: Vec<f32>, drops: Vec<Drops>) -> SourceSnapshot {
        let n = fps.len();
        SourceSnapshot {
            source_id: id.into(),
            seconds: n as u64,
            target_fps: 30,
            totals: Counts { received: 100, ..Counts::default() },
            drops: drops.last().copied().unwrap_or_default(),
            now: Rates { received_fps: *fps.last().unwrap_or(&0.0), ..Rates::default() },
            stages: vec![NamedTiming { name: "decode".into(), timing: Timing { mean_ms: 4.0, max_ms: 9.0, count: 30 } }],
            series: SourceSeries {
                received_fps: fps.clone(), delivered_fps: fps.clone(), decoded_fps: fps, dropped_fps: vec![0.0; n], mbps: vec![8.0; n],
                decode_ms: vec![4.0; n], step_ms: vec![2.0; n], camera_ms: vec![1.0; n], repeated_fps: vec![0.0; n], drop_totals: drops,
                ..SourceSeries::default()
            },
            ..SourceSnapshot::default()
        }
    }

    fn snapshot(sources: Vec<SourceSnapshot>) -> Snapshot {
        Snapshot { at_ms: 5000, sources, ..Snapshot::default() }
    }

    #[test]
    fn a_series_is_min_average_max_and_the_worst_moments_with_when() {
        let mut fps = vec![30.0; 10];
        fps[2] = 12.0;
        fps[8] = 20.0;
        let s = summarise("received_fps", &fps);
        assert_eq!((s["min"].as_f64(), s["max"].as_f64(), s["samples"].as_u64()), (Some(12.0), Some(30.0), Some(10)));
        assert_eq!(s["average"].as_f64(), Some(27.2));
        let moments = s["worst_moments"].as_array().unwrap();
        // The lowest fps are the worst, named oldest first with how long before the newest sample
        assert_eq!(moments[0], json!({"seconds_ago": 7, "value": 12.0}));
        assert_eq!(moments[1], json!({"seconds_ago": 1, "value": 20.0}));
    }

    #[test]
    fn for_times_the_highest_are_the_worst_and_a_steady_series_names_none() {
        let s = summarise("decode_ms", &[4.0, 5.0, 40.0, 4.0]);
        assert_eq!(s["worst_moments"], json!([{"seconds_ago": 1, "value": 40.0}]));
        assert_eq!(summarise("received_fps", &[30.0; 5])["worst_moments"], json!([]));
    }

    #[test]
    fn an_empty_series_says_so_instead_of_inventing_numbers() {
        assert_eq!(summarise("mbps", &[])["samples"], json!(0));
    }

    #[test]
    fn a_window_is_the_tail_of_what_the_snapshot_keeps_and_drops_count_from_its_start() {
        let mut totals = vec![Drops::default(); 20];
        for (i, t) in totals.iter_mut().enumerate() {
            t.lost_on_link = if i >= 12 { 5 } else { 0 };
            t.late = if i >= 15 { 2 } else { 0 };
        }
        let fps: Vec<f32> = (0..20).map(|i| i as f32).collect();
        let c = Collected::from_snapshot(&snapshot(vec![source("phone", fps, totals)]), 10);
        let report = c.report(Level::Standard, &[]);
        let stream = &report["streams"][0];
        assert_eq!(stream["series"]["received_fps"]["samples"], json!(10));
        assert_eq!(stream["series"]["received_fps"]["min"].as_f64(), Some(10.0));
        // All five lost frames landed inside the last ten seconds, and so did both late ones
        assert_eq!(stream["dropped_in_window"]["by_reason"], json!({"lost_on_link": 5, "late": 2}));
        assert_eq!(stream["dropped_in_window"]["total"], json!(7));
    }

    #[test]
    fn standard_leaves_out_what_only_nerds_want_and_a_series_filter_narrows_it() {
        let c = Collected::from_snapshot(&snapshot(vec![source("phone", vec![30.0; 5], vec![Drops::default(); 5])]), 5);
        let standard = c.report(Level::Standard, &[]);
        assert!(standard["streams"][0]["series"].get("decode_ms").is_none() && standard["streams"][0]["series"].get("received_fps").is_some());
        assert!(standard["streams"][0].get("stages_last_second").is_none());
        let nerd = c.report(Level::Nerd, &[]);
        assert!(nerd["streams"][0]["series"].get("decode_ms").is_some() && nerd["streams"][0].get("stages_last_second").is_some());
        assert!(nerd["app"].get("balance").is_some());
        let only = c.report(Level::Nerd, &["decode_ms".to_string()]);
        assert_eq!(only["streams"][0]["series"].as_object().unwrap().keys().collect::<Vec<_>>(), ["decode_ms"]);
    }

    #[test]
    fn a_watch_builds_its_series_a_second_at_a_time_and_counts_drops_from_when_it_began() {
        let mut watch = Collected::default();
        assert!(watch.is_empty());
        for second in 0..6u64 {
            // The stream had dropped 4 before the watch began, then loses one more frame in each of seconds 3 to 5
            let totals: Vec<Drops> = (0..=second).map(|i| Drops { lost_on_link: 4 + i.saturating_sub(2), ..Drops::default() }).collect();
            let fps = vec![30.0; second as usize + 1];
            watch.push(&snapshot(vec![source("phone", fps, totals)]));
        }
        let report = watch.report(Level::Standard, &[]);
        assert_eq!(report["seconds"], json!(6));
        assert_eq!(report["streams"][0]["series"]["received_fps"]["samples"], json!(6));
        assert_eq!(report["streams"][0]["dropped_in_window"]["total"], json!(3), "{report}");
    }

    #[test]
    fn a_stream_that_stopped_part_way_through_a_watch_is_still_reported() {
        let mut watch = Collected::default();
        for second in 1..=5usize {
            watch.push(&snapshot(vec![source("phone", vec![30.0; second], vec![Drops::default(); second]), source("webcam", vec![25.0; second], vec![Drops::default(); second])]));
        }
        // The webcam goes at second 6 and then the phone too: core sends snapshots with fewer streams, and finally none
        watch.push(&snapshot(vec![source("phone", vec![30.0; 6], vec![Drops::default(); 6])]));
        watch.push(&snapshot(vec![]));
        assert!(!watch.is_empty());
        let report = watch.report(Level::Standard, &[]);
        let streams = report["streams"].as_array().unwrap();
        assert_eq!(streams.iter().map(|s| s["source"].as_str().unwrap()).collect::<Vec<_>>(), ["phone", "webcam"], "{report}");
        assert_eq!(streams[0]["series"]["received_fps"]["samples"], json!(6));
        assert_eq!(streams[1]["series"]["received_fps"]["samples"], json!(5), "what it gave before it went");
        assert_eq!(report["seconds"], json!(7));
    }

    #[test]
    fn a_watch_that_only_ever_saw_empty_snapshots_collected_nothing() {
        let mut watch = Collected::default();
        watch.push(&snapshot(vec![]));
        watch.push(&snapshot(vec![]));
        assert!(watch.is_empty());
    }

    #[test]
    fn plugin_times_say_when_nothing_measured_them_yet() {
        let mut snap = snapshot(vec![source("phone", vec![30.0; 3], vec![Drops::default(); 3])]);
        let c = Collected::from_snapshot(&snap, 3);
        assert_eq!(c.report(Level::Nerd, &[])["streams"][0]["plugin_frame_steps_last_second"], json!("not measured yet"));
        snap.detailed = true;
        let c = Collected::from_snapshot(&snap, 3);
        assert!(c.report(Level::Nerd, &[])["streams"][0]["plugin_frame_steps_last_second"].is_array());
    }

    #[test]
    fn level_is_standard_unless_nerd_is_asked_for() {
        assert_eq!(Level::parse(None), Ok(Level::Standard));
        assert_eq!(Level::parse(Some("nerd")), Ok(Level::Nerd));
        assert!(Level::parse(Some("loud")).is_err());
    }
}
