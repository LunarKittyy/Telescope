use crate::shell::State;
use serde_json::Value;
use std::time::{Duration, Instant};
use telescope_core::event::Event;
use telescope_core::host::{PreviewMessage, RequestId};
use telescope_core::i18n::{tr, Fields};

const CORE: &str = "core";

struct Entry {
    owner: String,
    source: String,
    message: PreviewMessage,
    since: Instant,
}

#[derive(Default)]
pub(crate) struct Messages {
    entries: Vec<Entry>,
    painted: Option<Instant>,
}

#[derive(Default)]
pub(crate) struct Drawing {
    pub visible: bool,
    pub text: String,
    pub detail: String,
    pub step: String,
    pub countdown: String,
    pub progress: f32,
    pub counting: bool,
    pub dim: bool,
}

fn elapsed_ms(since: Instant, now: Instant) -> i64 {
    now.saturating_duration_since(since).as_millis().min(i64::MAX as u128) as i64
}

fn countdown(remaining_ms: i64) -> String {
    if remaining_ms <= 0 {
        tr("Almost done")
    } else if remaining_ms < 3_000 {
        String::new()
    } else {
        let seconds = remaining_ms.saturating_add(999) / 1_000;
        if seconds >= 60 {
            tr("{m} min {s} s left").field("m", seconds / 60).field("s", seconds % 60)
        } else {
            tr("{s} s left").field("s", seconds)
        }
    }
}

impl Messages {
    pub fn set(&mut self, owner: &str, source: &str, message: Option<PreviewMessage>, now: Instant) {
        self.entries.retain(|e| e.owner != owner || e.source != source);
        if let Some(message) = message {
            self.entries.push(Entry { owner: owner.into(), source: source.into(), message, since: now });
        }
    }

    fn drop_where(&mut self, mut drop: impl FnMut(&Entry) -> bool) -> Vec<(String, String)> {
        let mut ended = Vec::new();
        self.entries.retain(|e| {
            let gone = drop(e);
            if gone {
                ended.push((e.owner.clone(), e.source.clone()));
            }
            !gone
        });
        ended
    }

    fn expire(&mut self, now: Instant) -> Vec<(String, String)> {
        self.drop_where(|e| elapsed_ms(e.since, now) >= e.message.max_ms.max(0))
    }

    pub fn due(&mut self, now: Instant) -> bool {
        if self.painted.is_some_and(|last| now.saturating_duration_since(last) < Duration::from_millis(250)) {
            return false;
        }
        self.painted = Some(now);
        true
    }

    pub fn drawing(&self, source: Option<&str>, now: Instant) -> Drawing {
        let Some(e) = source.and_then(|s| self.entries.iter().rev().find(|e| e.source == s)) else { return Drawing::default() };
        let m = &e.message;
        let remaining = m.remaining_ms.map(|ms| ms.saturating_sub(elapsed_ms(e.since, now)).max(0));
        let progress = match (remaining, m.total_ms.filter(|&ms| ms > 0)) {
            (Some(left), Some(total)) => (1.0 - left as f64 / total as f64).clamp(0.0, 1.0) as f32,
            (Some(0), _) => 1.0,
            _ => 0.0,
        };
        Drawing { visible: true, text: m.text.clone(), detail: m.detail.clone(), step: m.step.clone(),
            countdown: remaining.map(countdown).unwrap_or_default(), progress, counting: remaining.is_some(), dim: m.dim }
    }
}

pub(crate) struct Photo {
    source: String,
    exposures_ms: Vec<i64>,
    shots: i64,
    taken: i64,
    paused: bool,
}

fn positive(value: Option<&Value>, default: i64) -> i64 {
    value.and_then(Value::as_i64).filter(|n| *n > 0).unwrap_or(default)
}

impl Photo {
    fn new(source: &str, options: &Value, camera: bool) -> Self {
        let exposure = positive(options.get("exposure_ns"), 0);
        let mut exposures_ms: Vec<i64> = options.get("bracket_ev").and_then(Value::as_str).filter(|s| !s.trim().is_empty()).map(|s| {
            s.split(',').take(100).filter_map(|ev| ev.trim().parse::<f64>().ok().filter(|ev| ev.is_finite()))
                .map(|ev| (exposure as f64 * 2f64.powf(ev) / 1_000_000.0).max(0.0) as i64).collect()
        }).unwrap_or_else(|| vec![exposure / 1_000_000]);
        if exposures_ms.is_empty() {
            exposures_ms.push(exposure / 1_000_000);
        }
        let shots = positive(options.get("count"), 1).saturating_mul(positive(options.get("focus_steps"), 1)).saturating_mul(exposures_ms.len() as i64);
        let extension = options.get("extension").is_some_and(|v| !v.is_null() && v.as_str() != Some(""));
        let paused = camera || extension || exposures_ms.iter().any(|ms| *ms >= telescope_core::shot::FREEZES_FROM_NS / 1_000_000);
        Self { source: source.into(), exposures_ms, shots, taken: 0, paused }
    }

    fn update(&mut self, data: &Value) {
        self.shots = positive(data.get("shots"), self.shots);
        self.taken = data.get("taken").and_then(Value::as_i64).unwrap_or(self.taken).clamp(0, self.shots);
        self.paused |= data.get("paused").and_then(Value::as_bool) == Some(true);
    }

    fn estimate(&self, taken: i64) -> i64 {
        let left = self.shots.saturating_sub(taken).max(0);
        let n = self.exposures_ms.len() as i64;
        let cycle = self.exposures_ms.iter().fold(0i64, |sum, ms| sum.saturating_add(*ms));
        let full = (left / n).saturating_mul(cycle);
        let rest = (0..left % n).fold(0i64, |sum, i| sum.saturating_add(self.exposures_ms[((taken % n + i) % n) as usize]));
        full.saturating_add(rest).saturating_add(left.saturating_mul(1_500))
    }

    fn message(&self) -> Option<PreviewMessage> {
        if !self.paused {
            return None;
        }
        let remaining = self.estimate(self.taken);
        let total = self.estimate(0);
        Some(PreviewMessage {
            text: tr("Taking a picture"), detail: tr("Video will come back when done."),
            step: if self.shots > 1 { tr("Shot {n} of {m}").field("n", self.taken.saturating_add(1).min(self.shots)).field("m", self.shots) } else { String::new() },
            remaining_ms: Some(remaining), total_ms: Some(total), dim: true,
            max_ms: remaining.saturating_mul(2).saturating_add(60_000),
        })
    }
}

impl State {
    pub(crate) fn set_preview_message(&mut self, owner: &str, source: Option<&str>, message: Option<PreviewMessage>) {
        if !self.index("preview").is_some_and(|i| self.slots[i].stopped.is_none()) {
            return;
        }
        let Some(source) = source.map(str::to_string).or_else(|| self.streams.source_id.clone()) else { return };
        self.preview_messages.set(owner, &source, message, Instant::now());
    }

    fn messages_ended(&self, ended: Vec<(String, String)>) {
        for (owner, source_id) in ended {
            self.posts.push_event(&owner, Event::PreviewMessageEnded { source_id });
        }
    }

    pub(crate) fn drop_owner_messages(&mut self, owner: &str) {
        let ended = self.preview_messages.drop_where(|e| e.owner == owner || owner == "preview");
        self.messages_ended(ended);
    }

    pub(crate) fn drop_stream_messages(&mut self, source: Option<&str>) {
        self.photo_messages.retain(|_, p| source.is_some_and(|s| p.source != s));
        let ended = self.preview_messages.drop_where(|e| source.is_none_or(|s| e.source == s));
        self.messages_ended(ended);
    }

    pub(crate) fn expire_preview_messages(&mut self, now: Instant) -> bool {
        let ended = self.preview_messages.expire(now);
        let changed = !ended.is_empty();
        self.messages_ended(ended);
        changed
    }

    pub(crate) fn begin_photo_message(&mut self, request: RequestId, source: &str, options: &Value, camera: bool) {
        let photo = Photo::new(source, options, camera);
        if let Some(message) = photo.message() {
            self.set_preview_message(CORE, Some(source), Some(message));
        }
        self.photo_messages.insert(request.0, photo);
    }

    pub(crate) fn photo_message_event(&mut self, event: &Event) {
        match event {
            Event::RequestProgress { request, data } => {
                let Some(photo) = self.photo_messages.get_mut(&request.0) else { return };
                photo.update(data);
                let (source, message) = (photo.source.clone(), photo.message());
                if message.is_some() && !self.photo_messages.iter().any(|(id, p)| *id > request.0 && p.source == source) {
                    self.set_preview_message(CORE, Some(&source), message);
                }
            }
            Event::RequestDone { request, .. } => {
                let Some(photo) = self.photo_messages.remove(&request.0) else { return };
                let message = self.photo_messages.values().rev().find(|p| p.source == photo.source).and_then(Photo::message);
                self.set_preview_message(CORE, Some(&photo.source), message);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::{Arc, Mutex};
    use telescope_core::plugin::{Plugin, PluginInfo};

    struct Listener(&'static str, Arc<Mutex<Vec<Event>>>);

    impl Plugin for Listener {
        fn info(&self) -> PluginInfo {
            PluginInfo { name: self.0.into(), ..Default::default() }
        }
        fn on_event(&mut self, _host: &mut dyn telescope_core::Host, event: &Event) {
            if matches!(event, Event::PreviewMessageEnded { .. }) {
                self.1.lock().unwrap().push(event.clone());
            }
        }
    }

    fn state() -> (State, Arc<Mutex<Vec<Event>>>) {
        let mut state = State::new(telescope_core::config::ConfigFile::at(telescope_core::testing::scratch_dir("preview-message").join("config.json")));
        state.colour = telescope_core::colour::ColourPipeline::inline();
        let heard = Arc::new(Mutex::new(Vec::new()));
        state.register(Box::new(Listener("preview", Arc::default()))).unwrap();
        state.register(Box::new(Listener("sender", heard.clone()))).unwrap();
        state.streams.source_id = Some("phone".into());
        (state, heard)
    }

    fn message(text: &str) -> PreviewMessage {
        PreviewMessage { text: text.into(), detail: String::new(), step: String::new(), remaining_ms: Some(24_000), total_ms: Some(40_000), dim: true, max_ms: 60_000 }
    }

    #[test]
    fn countdown_never_shows_negative_seconds_or_the_last_two_seconds() {
        assert_eq!(countdown(24_000), "24 s left");
        assert_eq!(countdown(24_001), "25 s left");
        assert_eq!(countdown(3_000), "3 s left");
        assert_eq!(countdown(2_999), "");
        assert_eq!(countdown(1), "");
        assert_eq!(countdown(0), "Almost done");
        assert_eq!(countdown(-1), "Almost done");
        assert_eq!(countdown(60_000), "1 min 0 s left");
        assert_eq!(countdown(84_000), "1 min 24 s left");
    }

    #[test]
    fn newest_message_is_on_top_and_reveals_the_one_underneath() {
        let (mut stack, now) = (Messages::default(), Instant::now());
        stack.set("a", "phone", Some(message("first")), now);
        stack.set("b", "other", Some(message("other stream")), now);
        stack.set("b", "phone", Some(message("second")), now);
        assert_eq!(stack.drawing(Some("phone"), now).text, "second");
        stack.set("b", "phone", None, now);
        assert_eq!(stack.drawing(Some("phone"), now).text, "first");
        assert_eq!(stack.drawing(Some("other"), now).text, "other stream");
        stack.set("a", "phone", Some(message("replaced")), now);
        stack.set("a", "phone", None, now);
        assert!(!stack.drawing(Some("phone"), now).visible);
        assert!(!stack.drawing(None, now).visible);
    }

    #[test]
    fn frozen_frames_count_down_and_expiry_also_removes_covered_messages() {
        let (mut stack, now) = (Messages::default(), Instant::now());
        let mut short = message("short");
        short.max_ms = 1_000;
        stack.set("a", "phone", Some(short), now);
        stack.set("b", "phone", Some(message("top")), now);
        assert!(stack.expire(now + Duration::from_millis(999)).is_empty());
        assert_eq!(stack.expire(now + Duration::from_secs(1)), [("a".into(), "phone".into())]);
        let drawing = stack.drawing(Some("phone"), now + Duration::from_secs(24));
        assert_eq!(drawing.countdown, "Almost done");
        assert_eq!(drawing.progress, 1.0);
        let mut uncounted = message("wait");
        uncounted.remaining_ms = None;
        stack.set("b", "phone", Some(uncounted), now);
        let drawing = stack.drawing(Some("phone"), now);
        assert!(!drawing.counting);
        assert!(drawing.countdown.is_empty());
    }

    #[test]
    fn photo_messages_follow_the_exposures_and_progress() {
        let mut photo = Photo::new("phone", &json!({"exposure_ns": 10_000_000_000i64, "count": 3}), false);
        let first = photo.message().unwrap();
        assert_eq!(first.remaining_ms, Some(34_500));
        assert_eq!(first.total_ms, Some(34_500));
        assert_eq!(first.max_ms, 129_000);
        assert_eq!(first.step, "Shot 1 of 3");
        photo.update(&json!({"taken": 1, "shots": 3}));
        let next = photo.message().unwrap();
        assert_eq!(next.remaining_ms, Some(23_000));
        assert_eq!(next.total_ms, first.total_ms);
        assert_eq!(next.step, "Shot 2 of 3");
        assert!(Photo::new("phone", &json!({"exposure_ns": 16_666_667}), false).message().is_none());
        assert!(Photo::new("camera", &json!({}), true).message().is_some());
        assert!(Photo::new("phone", &json!({"extension": "night"}), false).message().is_some());
    }

    #[test]
    fn bracket_uses_its_largest_exposure_to_decide_whether_video_pauses() {
        let mut photo = Photo::new("phone", &json!({"exposure_ns": 250_000_000, "bracket_ev": "-1,0,1", "count": 2}), false);
        assert_eq!(photo.shots, 6);
        assert_eq!(photo.message().unwrap().remaining_ms, Some(10_750));
        photo.update(&json!({"taken": 1, "shots": 6}));
        assert_eq!(photo.message().unwrap().remaining_ms, Some(9_125));
        let mut fast = Photo::new("phone", &json!({}), false);
        fast.update(&json!({"paused": true, "taken": 0, "shots": 8}));
        assert_eq!(fast.message().unwrap().step, "Shot 1 of 8");
    }

    #[test]
    fn host_expiry_tells_only_the_owner_and_restores_the_message_below() {
        let (mut state, heard) = state();
        state.set_preview_message("other", None, Some(message("under")));
        let mut short = message("short");
        short.max_ms = 10;
        state.set_preview_message("sender", None, Some(short));
        let now = Instant::now() + Duration::from_millis(20);
        assert!(state.expire_preview_messages(now));
        state.deliver_events();
        assert_eq!(*heard.lock().unwrap(), [Event::PreviewMessageEnded { source_id: "phone".into() }]);
        assert_eq!(state.preview_messages.drawing(Some("phone"), now).text, "under");
        assert!(!state.expire_preview_messages(now));
        state.set_preview_message("sender", None, Some(message("clear")));
        state.set_preview_message("sender", None, None);
        state.deliver_events();
        assert_eq!(heard.lock().unwrap().len(), 1, "an explicit clear sends no ended event");
    }

    #[test]
    fn owner_cleanup_drops_every_stream_and_a_panic_does_too() {
        let (mut state, heard) = state();
        state.set_preview_message("sender", Some("phone"), Some(message("one")));
        state.set_preview_message("sender", Some("other"), Some(message("two")));
        crate::host::Ctx { state: &mut state, me: "sender".into() }.release_everything();
        state.deliver_events();
        assert_eq!(heard.lock().unwrap().len(), 2);
        assert!(!state.preview_messages.drawing(Some("phone"), Instant::now()).visible);
        assert!(!state.preview_messages.drawing(Some("other"), Instant::now()).visible);
        state.set_preview_message("sender", None, Some(message("again")));
        state.stop_after_panic(state.index("sender").unwrap(), "stopped for the test");
        assert!(!state.preview_messages.drawing(Some("phone"), Instant::now()).visible);
    }

    #[test]
    fn a_stream_going_away_clears_its_messages_and_ignores_late_progress() {
        let (mut state, heard) = state();
        state.set_preview_message("sender", Some("phone"), Some(message("one")));
        state.set_preview_message("sender", Some("other"), Some(message("two")));
        state.begin_photo_message(RequestId(1), "phone", &json!({"exposure_ns": 10_000_000_000i64}), false);
        state.let_go_of_photos(Some("phone"));
        state.photo_message_event(&Event::RequestProgress { request: RequestId(1), data: json!({"paused": true}) });
        state.deliver_events();
        assert_eq!(*heard.lock().unwrap(), [Event::PreviewMessageEnded { source_id: "phone".into() }]);
        assert!(!state.preview_messages.drawing(Some("phone"), Instant::now()).visible);
        assert!(state.preview_messages.drawing(Some("other"), Instant::now()).visible);
    }

    #[test]
    fn core_photos_raise_at_once_and_clear_on_every_kind_of_end() {
        let (mut state, heard) = state();
        for (i, code) in ["done", "failed", "interrupted", "hidden"].iter().enumerate() {
            let request = RequestId(i as u64);
            state.begin_photo_message(request, "phone", &json!({"exposure_ns": 10_000_000_000i64}), false);
            assert_eq!(state.preview_messages.drawing(Some("phone"), Instant::now()).text, "Taking a picture");
            state.posts.push_event("sender", Event::RequestDone { request, ok: *code == "done", message: String::new(), data: json!({"code": code}) });
            state.deliver_events();
            assert!(!state.preview_messages.drawing(Some("phone"), Instant::now()).visible);
        }
        state.begin_photo_message(RequestId(10), "phone", &json!({"exposure_ns": 16_666_667}), false);
        assert!(!state.preview_messages.drawing(Some("phone"), Instant::now()).visible);
        state.posts.push_event("sender", Event::RequestProgress { request: RequestId(10), data: json!({"paused": true, "taken": 2, "shots": 8}) });
        state.deliver_events();
        assert_eq!(state.preview_messages.drawing(Some("phone"), Instant::now()).step, "Shot 3 of 8");
        state.let_go_of_photos(None);
        state.begin_photo_message(RequestId(11), "camera", &json!({}), true);
        assert!(state.preview_messages.drawing(Some("camera"), Instant::now()).visible);
        assert!(heard.lock().unwrap().is_empty(), "core messages don't send ended events to the photo plugin");
    }

    #[test]
    fn messages_do_nothing_without_the_preview_plugin() {
        let (mut state, _) = state();
        state.set_preview_message("sender", None, Some(message("shown")));
        state.stop_after_panic(state.index("preview").unwrap(), "stopped for the test");
        state.set_preview_message("sender", None, Some(message("ignored")));
        state.begin_photo_message(RequestId(1), "phone", &json!({"exposure_ns": 10_000_000_000i64}), false);
        assert!(!state.preview_messages.drawing(Some("phone"), Instant::now()).visible);
    }

    #[test]
    fn a_rejected_queued_photo_restores_the_photo_already_under_way() {
        let (mut state, _) = state();
        state.begin_photo_message(RequestId(1), "phone", &json!({"exposure_ns": 10_000_000_000i64, "count": 3}), false);
        state.photo_message_event(&Event::RequestProgress { request: RequestId(1), data: json!({"taken": 1, "shots": 3}) });
        state.begin_photo_message(RequestId(2), "phone", &json!({"exposure_ns": 20_000_000_000i64}), false);
        state.photo_message_event(&Event::RequestDone { request: RequestId(2), ok: false, message: String::new(), data: json!({"code": "failed"}) });
        assert_eq!(state.preview_messages.drawing(Some("phone"), Instant::now()).step, "Shot 2 of 3");
    }
}
