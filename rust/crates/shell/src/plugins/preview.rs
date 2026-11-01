//! plugins/preview.py: the video stage in the middle of the window and the camera button under it. The frames
//! themselves are drawn by the window (preview.rs); this keeps what the stage says and the toolbar.

use serde_json::{json, Value};
use telescope_core::i18n::{n_, tr};
use telescope_core::plugin::{ConfigRejected, Plugin, PluginInfo};
use telescope_core::event::LensBox;
use telescope_core::pose::{self, Person};
use telescope_core::ui::{ActionSpec, CardSpec, SlintSource, UiEvent, UiSpec};
use telescope_core::view::{upright_to_shown, Geometry};
use telescope_core::{vision, Event, Host};

// Translators: the preview area doesn't wrap lines, so keep the line breaks and each line about as long as this one
const IDLE_TEXT: &str = "Not streaming\n\nPress Start streaming and the phone's camera comes up on its own.";
const IDLE_BROWSER_TEXT: &str = "Not streaming\n\nPress Start streaming and the browser's camera comes up once it connects.";
const IDLE_WEBCAM_TEXT: &str = "Not streaming\n\nPress Start streaming to use this camera.";
const IDLE_MIC_TEXT: &str = "Not streaming\n\nStart mic only streams the phone's mic with its camera off.";
const WAITING_TEXT: &str = "Waiting for the first frame\u{2026}";
const CAMERA_OFF_TEXT: &str = "Camera off\n\nThe phone's mic is still streaming.\nStop streaming ends it.";
// Broken by hand like the rest: the stage doesn't wrap, and these are its longest lines
const CAMERA_OFF_AUTO_TEXT: &str = "Camera off\n\nNo app was using the camera, so it turned off.\nThe phone's mic is still streaming, and the camera\ncomes back when an app opens it.";

/// px the mouse has to move before a press is a drag, not a click.
const DRAG_START: f64 = 4.0;
/// Zoom factor per wheel notch.
const WHEEL_STEP: f64 = 1.1;
/// How long the square stays where you clicked.
const MARKER_MS: u64 = 1000;
const MARKER_TIMER: &str = "focus-marker";
/// The lens boxes stay this long after the framing last moved, then fade over FADE_STEPS of FADE_STEP_MS.
const BOXES_HOLD_MS: u64 = 1000;
const FADE_STEP_MS: u64 = 50;
const FADE_STEPS: f32 = 6.0;
const BOXES_HOLD_TIMER: &str = "boxes-hold";
const BOXES_FADE_TIMER: &str = "boxes-fade";
/// What asks vision for people while the skeleton is on.
const SKELETON_OWNER: &str = "Preview skeleton";

/// One line of a skeleton, in the frame as shown (0..1): sure when the model is sure of both ends, out for an arm Auto
/// framing would count as held out.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Line {
    pub from: (f64, f64),
    pub to: (f64, f64),
    pub sure: bool,
    pub out: bool,
}

/// skeleton: the lines to draw for people.
pub fn skeleton(people: &[Person], map_point: &dyn Fn(f64, f64) -> (f64, f64), aspect: f64) -> Vec<Line> {
    let mut lines = Vec::new();
    for person in people {
        let out_sides = pose::hands_out(std::slice::from_ref(person), aspect);
        for (a, b) in pose::BONES {
            let (Some(pa), Some(pb)) = (person.point(a, pose::ARM_SEEN), person.point(b, pose::ARM_SEEN)) else { continue };
            let sure = person.point(a, pose::SEEN).is_some() && person.point(b, pose::SEEN).is_some();
            let (side, part) = b.split_once('_').unwrap_or(("", b));
            let out = out_sides.contains(&side) && ["elbow", "wrist", "index", "pinky", "thumb"].contains(&part);
            lines.push(Line { from: map_point(pa.0, pa.1), to: map_point(pb.0, pb.1), sure, out });
        }
    }
    lines
}

/// The left button while it's down on the frame: where it went down, as (u, v) and in px of the frame on screen.
struct Press {
    point: (f64, f64),
    start: (f64, f64),
    last: (f64, f64),
    dragging: bool,
}

#[derive(Default)]
pub struct Preview {
    /// The Hide/Show choice (on by default).
    hidden: bool,
    /// The first-run checklist has the stage.
    setup_needed: bool,
    /// Another source (Browser camera, a webcam) has its own idle text.
    picked: String,
    /// The pop-out window is open, and whether opening it hid the card (so closing it shows the card again).
    popout: bool,
    hid_for_popout: bool,
    /// Times Pop out was pressed while the window was open: the window comes to the front for each.
    popout_asks: i64,
    /// Which stream this is, counted (_stream_gen): nothing the window was waiting on from the last one holds up the next.
    stream_gen: i64,
    /// The Lens outlines button: the boxes stay on. It stays disabled until lenses send boxes.
    lens_boxes: bool,
    /// What the phone's other lenses see, and how far the boxes have faded since the framing last moved.
    boxes: Vec<LensBox>,
    boxes_opacity: f32,
    /// The Skeleton button, and the people it draws.
    skeleton: bool,
    smooth: bool,
    people: Vec<Person>,
    /// A click focuses there, and a drag pans (FrameLabel).
    pickable: bool,
    pannable: bool,
    press: Option<Press>,
}

impl Preview {
    /// _idle_text: what the stage says with no frame to show.
    pub fn idle_text(&self, host: &dyn Host) -> String {
        if !host.is_streaming() {
            if !host.is_camera_on() {
                return tr(IDLE_MIC_TEXT);
            }
            if self.picked.is_empty() {
                return tr(IDLE_TEXT);
            }
            let browser = self.picked == "browser" || self.picked.starts_with("browser:");
            return tr(if browser { IDLE_BROWSER_TEXT } else { IDLE_WEBCAM_TEXT });
        }
        if !host.is_camera_on() {
            return tr(if host.is_camera_off_auto() { CAMERA_OFF_AUTO_TEXT } else { CAMERA_OFF_TEXT });
        }
        tr(WAITING_TEXT)
    }

    /// The stage and toolbar as they are now; clear drops the frame the window drew (_clear).
    fn render(&self, host: &mut dyn Host, clear: bool) {
        let camera_on = host.is_camera_on();
        let (can, why) = host.can_turn_camera_off();
        let tip = if !camera_on {
            tr("Turn the phone's camera back on.")
        } else if !can {
            why
        } else if host.is_streaming() {
            tr("Turn the phone's camera off. Its mic keeps streaming.")
        } else {
            tr("Start with the phone's camera off, streaming just its mic.")
        };
        let mut props = json!({
            "visible-card": !self.setup_needed,
            "showing": !self.hidden,
            "toggle-text": if self.hidden { tr("Show") } else { tr("Hide") },
            "stage-text": if self.hidden { tr("Preview hidden") } else { self.idle_text(host) },
            "camera-off": !self.hidden && host.is_streaming() && !camera_on,
            "camera-on": camera_on,
            "camera-enabled": !camera_on || can,
            "camera-tip": tip,
            "popout": self.popout,
            "popout-asks": self.popout_asks,
            "stream-gen": self.stream_gen,
        });
        if clear || self.hidden {
            props["has-frame"] = json!(false);
        }
        host.set_ui_props("card", &props);
    }

    /// _push_boxes. A box the whole frame fits inside has no edge to show.
    fn show_boxes(&self, host: &mut dyn Host) {
        const EPS: f32 = 1e-3;
        let boxes: Vec<Value> = self.boxes.iter().filter(|b| !(b.x0 <= EPS && b.y0 <= EPS && b.x1 >= 1.0 - EPS && b.y1 >= 1.0 - EPS))
            .map(|b| json!({"label": b.label, "x0": b.x0, "y0": b.y0, "x1": b.x1, "y1": b.y1})).collect();
        host.set_ui_props("card", &json!({
            "lens-boxes": boxes,
            "boxes-opacity": if self.lens_boxes { 1.0 } else { self.boxes_opacity },
            "lenses-enabled": !self.boxes.is_empty(),
            "lenses-on": self.lens_boxes,
        }));
    }

    fn lens_boxes_seen(&mut self, host: &mut dyn Host, boxes: &[LensBox], moved: bool) {
        self.boxes = boxes.to_vec();
        if moved && !boxes.is_empty() {
            host.cancel_schedule(BOXES_FADE_TIMER);
            host.cancel_schedule(BOXES_HOLD_TIMER);
            self.boxes_opacity = 1.0;
            host.schedule(BOXES_HOLD_MS, BOXES_HOLD_TIMER);
        }
        self.show_boxes(host);
    }

    /// _show_skeleton_btn: only with the vision download there; MediaPipe itself loads when the button goes on.
    fn skeleton_there(host: &dyn Host) -> bool {
        let state = host.vision_state();
        state != vision::MISSING && state != vision::FAILED
    }

    /// _push_skeleton, and the button as vision is now.
    fn show_skeleton(&self, host: &mut dyn Host) {
        let lines = if self.people.is_empty() {
            Vec::new()
        } else {
            let geometry = Geometry::from(&host.view());
            let aspect = match host.view_aspect() {
                a if a > 0.0 => a as f64,
                _ => 16.0 / 9.0,
            };
            skeleton(&self.people, &|x, y| upright_to_shown(&geometry, x, y), aspect)
        };
        let lines: Vec<Value> = lines.iter().map(|l| json!({"x0": l.from.0, "y0": l.from.1, "x1": l.to.0, "y1": l.to.1, "sure": l.sure, "out": l.out})).collect();
        host.set_ui_props("card", &json!({"skeleton-lines": lines, "skeleton-shown": Self::skeleton_there(host), "skeleton-on": self.skeleton}));
    }

    /// _on_skeleton.
    fn set_skeleton(&mut self, host: &mut dyn Host, on: bool) {
        self.skeleton = on;
        host.want_people(SKELETON_OWNER, on);
        if !on {
            self.people.clear();
        }
        self.show_skeleton(host);
        host.schedule_save();
    }

    /// Saved settings or the plugins' start reached the toolbar: a button vision can't back goes off.
    fn settle(&mut self, host: &mut dyn Host) {
        self.skeleton &= Self::skeleton_there(host);
        host.want_people(SKELETON_OWNER, self.skeleton);
        self.show_skeleton(host);
        self.show_smooth(host);
        self.show_boxes(host);
    }

    fn toggle(&mut self, host: &mut dyn Host) {
        self.hidden = !self.hidden;
        self.render(host, true);
    }

    fn toggle_lenses(&mut self, host: &mut dyn Host) {
        self.lens_boxes = !self.lens_boxes;
        self.show_boxes(host);
    }

    fn toggle_smooth(&mut self, host: &mut dyn Host) {
        self.smooth = !self.smooth;
        self.show_smooth(host);
        host.schedule_save();
    }

    fn show_smooth(&self, host: &mut dyn Host) {
        host.set_ui_props("card", &json!({"smooth-on": self.smooth}));
    }

    fn stage_cursor(&self) -> &'static str {
        match (self.press.as_ref().is_some_and(|p| p.dragging), self.pannable, self.pickable) {
            (true, ..) => "grabbing",
            (_, true, _) => "grab",
            (_, _, true) => "cross",
            _ => "",
        }
    }

    fn show_cursor(&self, host: &mut dyn Host) {
        host.set_ui_props("card", &json!({"stage-cursor": self.stage_cursor()}));
    }

    /// A point on the picture to the frame as shown, past a plugin's warp (Desk mode).
    fn unwarped(host: &dyn Host, u: f64, v: f64) -> (f32, f32) {
        let (x, y) = host.unwarp(u, v);
        (x.clamp(0.0, 1.0) as f32, y.clamp(0.0, 1.0) as f32)
    }

    /// FrameLabel's mouse events: (u, v) on the frame, which is width x height px on screen.
    fn view_pointer(&mut self, host: &mut dyn Host, kind: &str, (u, v): (f64, f64), (width, height): (f64, f64)) {
        let at = (u * width, v * height);
        match kind {
            "down" => {
                if (self.pickable || self.pannable) && (0.0..=1.0).contains(&u) && (0.0..=1.0).contains(&v) {
                    self.press = Some(Press { point: (u, v), start: at, last: at, dragging: false });
                }
            }
            "move" => {
                if !self.pannable {
                    return;
                }
                let Some(press) = self.press.as_mut() else { return };
                if !press.dragging {
                    if (at.0 - press.start.0).abs() + (at.1 - press.start.1).abs() < DRAG_START {
                        return;
                    }
                    press.dragging = true;
                    self.show_cursor(host);
                }
                let Some(press) = self.press.as_mut() else { return };
                let (du, dv) = ((at.0 - press.last.0) / width.max(1.0), (at.1 - press.last.1) / height.max(1.0));
                press.last = at;
                // A drag on a warped picture moves what's under the mouse
                let (x0, y0) = host.unwarp(0.5, 0.5);
                let (x1, y1) = host.unwarp(0.5 + du, 0.5 + dv);
                host.emit(Event::ViewDragged { du: (x1 - x0) as f32, dv: (y1 - y0) as f32 });
            }
            "up" => {
                let Some(press) = self.press.take() else { return };
                self.show_cursor(host);
                if press.dragging || !self.pickable {
                    return;
                }
                host.set_ui_props("card", &json!({"marker-shown": true, "marker-u": press.point.0, "marker-v": press.point.1}));
                host.cancel_schedule(MARKER_TIMER);
                host.schedule(MARKER_MS, MARKER_TIMER);
                let (x, y) = Self::unwarped(host, press.point.0, press.point.1);
                host.emit(Event::FocusPointPicked { x, y });
            }
            "cancel" => {
                self.press = None;
                self.show_cursor(host);
            }
            _ => {}
        }
    }

    /// _open_popout: the card hides while the window shows the picture, and Hide waits until it closes.
    fn open_popout(&mut self, host: &mut dyn Host) {
        if self.popout {
            self.popout_asks += 1;
        } else {
            self.hid_for_popout = !self.hidden;
            self.hidden = true;
            self.popout = true;
        }
        self.render(host, true);
    }

    /// _on_popout_closed.
    fn popout_closed(&mut self, host: &mut dyn Host) {
        if !self.popout {
            return;
        }
        self.popout = false;
        if self.hid_for_popout {
            self.hidden = false;
        }
        self.hid_for_popout = false;
        self.render(host, true);
    }
}

impl Plugin for Preview {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: "preview".into(),
            title: n_("Preview").into(),
            about: n_("The live picture in the middle of the window, and the camera on/off button under it.").into(),
            when_off: n_("Without it the window doesn't show the picture but apps using the virtual camera still get it.").into(),
            category: "preview".into(),
            icon: "eye".into(),
            ..Default::default()
        }
    }

    fn card(&mut self) -> Option<CardSpec> {
        Some(CardSpec {
            ui: UiSpec {
                source: SlintSource::Text(include_str!("../../ui/cards/preview.slint").into()),
                component: "PreviewCard".into(),
                props: json!({"stage-text": tr(IDLE_TEXT), "toggle-text": tr("Hide")}),
            },
            region: "center".into(),
        })
    }

    fn on_plugins_ready(&mut self, host: &mut dyn Host) {
        self.render(host, true);
        self.settle(host);
    }

    fn on_profile_applied(&mut self, host: &mut dyn Host) {
        self.settle(host);
    }

    fn actions(&mut self) -> Vec<ActionSpec> {
        let action = |id: &str, label: String| ActionSpec { id: id.into(), label, group: tr("Preview"), params: Vec::new(),
                                                             repeats: false, on_release: false, current: None, keywords: String::new() };
        vec![action("show_hide", tr("Show or hide the preview")), action("lens_outlines", tr("Lens outlines")),
             action("skeleton", tr("Skeleton")), action("pop_out", tr("Pop out the preview"))]
    }

    fn run_action(&mut self, host: &mut dyn Host, id: &str, _params: &Value) -> Option<String> {
        match id {
            // Hide waits while the pop-out is open, as its button does
            "show_hide" if !self.popout => self.toggle(host),
            "lens_outlines" if !self.boxes.is_empty() => self.toggle_lenses(host),
            "skeleton" if Self::skeleton_there(host) => self.set_skeleton(host, !self.skeleton),
            "pop_out" => self.open_popout(host),
            _ => return None,
        }
        Some(String::new())
    }

    fn on_event(&mut self, host: &mut dyn Host, event: &Event) {
        match event {
            Event::SetupNeeded { showing } => self.setup_needed = *showing,
            Event::SourceSelected { source_id } => self.picked = source_id.clone(),
            Event::CameraOnChanged { .. } | Event::MicChanged { .. } => {}
            Event::FocusPointAvailable { available } => {
                self.pickable = *available;
                return self.show_cursor(host);
            }
            Event::ViewPannable { pannable } => {
                self.pannable = *pannable;
                return self.show_cursor(host);
            }
            Event::TimerFired { owner, token } if owner == "preview" && token == MARKER_TIMER => {
                return host.set_ui_props("card", &json!({"marker-shown": false}));
            }
            Event::TimerFired { owner, token } if owner == "preview" && (token == BOXES_HOLD_TIMER || token == BOXES_FADE_TIMER) => {
                self.boxes_opacity = if token == BOXES_HOLD_TIMER { 1.0 } else { (self.boxes_opacity - 1.0 / FADE_STEPS).max(0.0) };
                if self.boxes_opacity > 1e-3 {
                    host.schedule(FADE_STEP_MS, BOXES_FADE_TIMER);
                } else {
                    self.boxes_opacity = 0.0;
                }
                return self.show_boxes(host);
            }
            Event::LensBoxes { boxes, moved } => return self.lens_boxes_seen(host, boxes, *moved),
            Event::PeopleSeen { source_id, people } => {
                if self.skeleton && *source_id == host.focused_source_id().unwrap_or_default() {
                    self.people = people.iter().filter_map(Person::from_value).collect();
                    self.show_skeleton(host);
                }
                return;
            }
            Event::ViewChanged { .. } | Event::VisionStateChanged { .. } => {
                if self.skeleton && !Self::skeleton_there(host) {
                    return self.set_skeleton(host, false);
                }
                return self.show_skeleton(host);
            }
            // A pick needs the picture to pick on
            Event::PreviewPickChanged { owner } => {
                if !owner.is_empty() && self.hidden && !self.popout {
                    self.toggle(host);
                }
                return;
            }
            _ => return,
        }
        self.render(host, false);
    }

    fn on_stream_start(&mut self, host: &mut dyn Host, _stream: &telescope_core::plugin::StreamInfo) {
        self.stream_gen += 1;
        self.render(host, true);
    }

    fn on_stream_stop(&mut self, host: &mut dyn Host) {
        self.press = None;
        self.people.clear();
        self.render(host, true);
        self.show_cursor(host);
        self.show_skeleton(host);
    }

    fn on_camera_off(&mut self, host: &mut dyn Host) {
        self.render(host, true);
    }

    fn on_camera_on(&mut self, host: &mut dyn Host) {
        self.render(host, true);
    }

    fn on_ui(&mut self, host: &mut dyn Host, event: &UiEvent) {
        match event.callback.as_str() {
            "toggle" if !self.popout => self.toggle(host),
            "pop-out" => self.open_popout(host),
            "popout-closed" => self.popout_closed(host),
            "lenses-clicked" => self.toggle_lenses(host),
            "skeleton-clicked" => self.set_skeleton(host, !self.skeleton),
            "smooth-clicked" => self.toggle_smooth(host),
            "view-pointer" => {
                let n = |i: usize| event.args.get(i).and_then(Value::as_f64).unwrap_or(0.0);
                let kind = event.args.first().and_then(Value::as_str).unwrap_or("");
                self.view_pointer(host, kind, (n(1), n(2)), (n(3), n(4)));
            }
            "view-wheel" => {
                let n = |i: usize| event.args.get(i).and_then(Value::as_f64).unwrap_or(0.0);
                let (u, v) = Self::unwarped(host, n(0), n(1));
                host.emit(Event::ViewScrolled { factor: WHEEL_STEP.powf(n(2)) as f32, u, v });
            }
            "camera-clicked" => {
                let on = host.is_camera_on();
                host.set_camera_on(!on, false);
            }
            _ => {}
        }
    }

    fn get_config(&self) -> Value {
        let mut cfg = json!({"lens_boxes": self.lens_boxes, "skeleton": self.skeleton});
        // Python has no such setting, so its settings files stay as they were unless someone turns this on
        if self.smooth {
            cfg["smooth"] = json!(true);
        }
        cfg
    }

    fn set_config(&mut self, cfg: &Value) -> Result<(), ConfigRejected> {
        self.lens_boxes = cfg.get("lens_boxes") == Some(&Value::Bool(true));
        // Whether vision can back it comes in settle, which has the host
        self.skeleton = cfg.get("skeleton") == Some(&Value::Bool(true));
        self.smooth = cfg.get("smooth") == Some(&Value::Bool(true));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use telescope_core::testing::FakeHost;

    /// The card's properties as every set_ui_props so far left them.
    fn card(host: &FakeHost) -> Value {
        let mut props = serde_json::Map::new();
        for call in &host.calls {
            if let Some(json) = call.strip_prefix("set_ui_props card ") {
                props.extend(serde_json::from_str::<serde_json::Map<String, Value>>(json).unwrap());
            }
        }
        Value::Object(props)
    }

    fn ready() -> (Preview, FakeHost) {
        let (mut p, mut host) = (Preview::default(), FakeHost::new());
        p.on_plugins_ready(&mut host);
        (p, host)
    }

    fn click(p: &mut Preview, host: &mut FakeHost, callback: &str) {
        p.on_ui(host, &UiEvent { ui: "card".into(), callback: callback.into(), args: vec![] });
    }

    #[test]
    fn preview_starts_active_and_toggles_off_and_back_on() {
        let (mut p, mut host) = ready();
        assert_eq!((card(&host)["showing"].as_bool(), card(&host)["toggle-text"].as_str()), (Some(true), Some("Hide")));
        click(&mut p, &mut host, "toggle");
        let c = card(&host);
        assert_eq!((c["showing"].as_bool(), c["toggle-text"].as_str(), c["stage-text"].as_str()), (Some(false), Some("Show"), Some("Preview hidden")));
        assert_eq!(c["has-frame"], false);
        click(&mut p, &mut host, "toggle");
        let c = card(&host);
        assert_eq!((c["toggle-text"].as_str(), c["stage-text"].as_str()), (Some("Hide"), Some(IDLE_TEXT)));
    }

    #[test]
    fn the_idle_text_names_the_browser_while_a_browser_source_is_picked() {
        let (mut p, mut host) = ready();
        for (sid, text) in [("browser", IDLE_BROWSER_TEXT), ("browser:b1", IDLE_BROWSER_TEXT), ("webcam:usb-c920", IDLE_WEBCAM_TEXT), ("", IDLE_TEXT)] {
            p.on_event(&mut host, &Event::SourceSelected { source_id: sid.into() });
            assert_eq!(card(&host)["stage-text"], text, "{sid}");
        }
        assert!(!IDLE_BROWSER_TEXT.contains("phone"));
    }

    #[test]
    fn preview_placeholder_says_waiting_while_a_stream_is_up() {
        let (mut p, mut host) = ready();
        host.streaming = true;
        click(&mut p, &mut host, "toggle");
        click(&mut p, &mut host, "toggle");
        assert_eq!(card(&host)["stage-text"], WAITING_TEXT);
    }

    #[test]
    fn stream_start_and_stop_swap_the_placeholder() {
        let (mut p, mut host) = ready();
        host.streaming = true;
        let stream = telescope_core::plugin::StreamInfo { url: "http://phone/".into(), source_id: "phone-1".into(), session: 0 };
        p.on_stream_start(&mut host, &stream);
        assert_eq!(card(&host)["stage-text"], WAITING_TEXT);
        host.streaming = false;
        p.on_stream_stop(&mut host);
        let c = card(&host);
        assert_eq!((c["stage-text"].as_str(), c["has-frame"].as_bool()), (Some(IDLE_TEXT), Some(false)));
    }

    #[test]
    fn the_camera_button_turns_the_camera_off_and_says_so() {
        let (mut p, mut host) = ready();
        host.can_turn_off = Some((true, String::new()));
        p.on_event(&mut host, &Event::MicChanged { enabled: true, muted: false });
        assert_eq!((card(&host)["camera-on"].as_bool(), card(&host)["camera-enabled"].as_bool()), (Some(true), Some(true)));
        click(&mut p, &mut host, "camera-clicked");
        assert!(host.camera_off);
        p.on_event(&mut host, &Event::CameraOnChanged { on: false });
        let c = card(&host);
        assert_eq!((c["camera-on"].as_bool(), c["camera-tip"].as_str()), (Some(false), Some("Turn the phone's camera back on.")));
        // Not streaming yet: the stage says what Start will do
        assert_eq!(c["stage-text"], IDLE_MIC_TEXT);
        click(&mut p, &mut host, "camera-clicked");
        p.on_event(&mut host, &Event::CameraOnChanged { on: true });
        assert!(!host.camera_off);
        assert_eq!(card(&host)["stage-text"], IDLE_TEXT);
    }

    #[test]
    fn the_camera_can_only_go_off_with_the_mic_on() {
        let (mut p, mut host) = ready();
        host.can_turn_off = Some((false, "Turn the mic on first: with both off there's nothing to stream.".into()));
        p.on_event(&mut host, &Event::MicChanged { enabled: false, muted: false });
        let c = card(&host);
        assert_eq!(c["camera-enabled"], false);
        assert!(c["camera-tip"].as_str().unwrap().contains("mic"));
        host.camera_off = true;
        p.on_event(&mut host, &Event::CameraOnChanged { on: false });
        assert_eq!(card(&host)["camera-enabled"], true);
    }

    #[test]
    fn open_and_close_popout_disables_then_restores_toggle() {
        let (mut p, mut host) = ready();
        click(&mut p, &mut host, "pop-out");
        let c = card(&host);
        assert_eq!((c["showing"].as_bool(), c["popout"].as_bool()), (Some(false), Some(true)));
        click(&mut p, &mut host, "toggle");
        assert_eq!(card(&host)["showing"], false, "Hide is disabled while the window is open");
        click(&mut p, &mut host, "popout-closed");
        let c = card(&host);
        assert_eq!((c["showing"].as_bool(), c["popout"].as_bool()), (Some(true), Some(false)), "the card it hid comes back");
    }

    #[test]
    fn a_popout_opened_with_the_card_hidden_leaves_it_hidden() {
        let (mut p, mut host) = ready();
        click(&mut p, &mut host, "toggle");
        click(&mut p, &mut host, "pop-out");
        click(&mut p, &mut host, "popout-closed");
        assert_eq!(card(&host)["showing"], false);
    }

    #[test]
    fn second_popout_request_reuses_visible_window() {
        let (mut p, mut host) = ready();
        click(&mut p, &mut host, "pop-out");
        let before = card(&host)["popout-asks"].as_i64();
        click(&mut p, &mut host, "pop-out");
        let c = card(&host);
        assert_eq!((c["popout"].as_bool(), c["popout-asks"].as_i64()), (Some(true), before.map(|n| n + 1)));
    }

    fn mouse(p: &mut Preview, host: &mut FakeHost, kind: &str, at: (f64, f64)) {
        p.on_ui(host, &UiEvent { ui: "card".into(), callback: "view-pointer".into(), args: vec![json!(kind), json!(at.0), json!(at.1), json!(400.0), json!(200.0)] });
    }

    fn picked(host: &FakeHost) -> Vec<(f32, f32)> {
        host.events.iter().filter_map(|e| match e { Event::FocusPointPicked { x, y } => Some((*x, *y)), _ => None }).collect()
    }

    fn dragged(host: &FakeHost) -> Vec<(f32, f32)> {
        host.events.iter().filter_map(|e| match e { Event::ViewDragged { du, dv } => Some((*du, *dv)), _ => None }).collect()
    }

    #[test]
    fn a_click_focuses_where_it_went_down_and_leaves_a_square_there_for_a_second() {
        let (mut p, mut host) = ready();
        mouse(&mut p, &mut host, "down", (0.25, 0.5));
        mouse(&mut p, &mut host, "up", (0.25, 0.5));
        assert!(picked(&host).is_empty(), "this lens can't focus on a point");
        p.on_event(&mut host, &Event::FocusPointAvailable { available: true });
        assert_eq!(card(&host)["stage-cursor"], "cross");
        mouse(&mut p, &mut host, "down", (0.25, 0.5));
        mouse(&mut p, &mut host, "move", (0.3, 0.6));
        mouse(&mut p, &mut host, "up", (0.3, 0.6));
        assert_eq!(picked(&host), [(0.25, 0.5)]);
        let c = card(&host);
        assert_eq!((c["marker-shown"].as_bool(), c["marker-u"].as_f64(), c["marker-v"].as_f64()), (Some(true), Some(0.25), Some(0.5)));
        assert_eq!(host.scheduled.iter().filter(|(_, token)| token == MARKER_TIMER).count(), 1);
        p.on_event(&mut host, &Event::TimerFired { owner: "preview".into(), token: MARKER_TIMER.into() });
        assert_eq!(card(&host)["marker-shown"], false);
    }

    #[test]
    fn the_letterbox_bars_arent_the_picture() {
        let (mut p, mut host) = ready();
        p.on_event(&mut host, &Event::FocusPointAvailable { available: true });
        mouse(&mut p, &mut host, "down", (-0.1, 0.5));
        mouse(&mut p, &mut host, "up", (0.5, 0.5));
        assert!(picked(&host).is_empty());
    }

    #[test]
    fn a_drag_past_the_first_px_pans_and_isnt_a_click() {
        let (mut p, mut host) = ready();
        p.on_event(&mut host, &Event::FocusPointAvailable { available: true });
        p.on_event(&mut host, &Event::ViewPannable { pannable: true });
        assert_eq!(card(&host)["stage-cursor"], "grab");
        mouse(&mut p, &mut host, "down", (0.5, 0.5));
        mouse(&mut p, &mut host, "move", (0.505, 0.5)); // 2 px
        assert!(dragged(&host).is_empty());
        mouse(&mut p, &mut host, "move", (0.55, 0.5));
        assert_eq!(card(&host)["stage-cursor"], "grabbing");
        mouse(&mut p, &mut host, "move", (0.6, 0.55));
        mouse(&mut p, &mut host, "up", (0.6, 0.55));
        let moved = dragged(&host);
        assert_eq!(moved.len(), 2);
        assert!((moved[0].0 - 0.05).abs() < 1e-5 && (moved[1].0 - 0.05).abs() < 1e-5 && (moved[1].1 - 0.05).abs() < 1e-5, "{moved:?}");
        assert!(picked(&host).is_empty());
        assert_eq!(card(&host)["stage-cursor"], "grab");
    }

    #[test]
    fn a_wheel_notch_zooms_by_a_tenth_around_the_mouse() {
        let (mut p, mut host) = ready();
        p.on_ui(&mut host, &UiEvent { ui: "card".into(), callback: "view-wheel".into(), args: vec![json!(0.25), json!(0.75), json!(1.0)] });
        assert!(host.events.iter().any(|e| matches!(e, Event::ViewScrolled { factor, u, v } if (*factor - 1.1).abs() < 1e-6 && *u == 0.25 && *v == 0.75)));
    }

    #[test]
    fn camera_off_while_streaming_shows_the_note_with_its_icon() {
        let (mut p, mut host) = ready();
        host.streaming = true;
        host.camera_off = true;
        p.on_camera_off(&mut host);
        let c = card(&host);
        assert_eq!((c["stage-text"].as_str(), c["camera-off"].as_bool()), (Some(CAMERA_OFF_TEXT), Some(true)));
    }

    #[test]
    fn the_stage_says_the_camera_is_off_and_why() {
        let (mut p, mut host) = ready();
        host.streaming = true;
        let stream = telescope_core::plugin::StreamInfo { url: "u".into(), source_id: "phone-1".into(), session: 0 };
        p.on_stream_start(&mut host, &stream);
        host.camera_off = true;
        p.on_camera_off(&mut host);
        let c = card(&host);
        assert_eq!((c["stage-text"].as_str(), c["camera-off"].as_bool(), c["has-frame"].as_bool()), (Some(CAMERA_OFF_TEXT), Some(true), Some(false)));
        host.camera_off_auto = true;
        p.on_camera_off(&mut host);
        assert_eq!(card(&host)["stage-text"], CAMERA_OFF_AUTO_TEXT);
        host.camera_off = false;
        p.on_camera_on(&mut host);
        let c = card(&host);
        assert_eq!((c["stage-text"].as_str(), c["camera-off"].as_bool()), (Some(WAITING_TEXT), Some(false)));
    }

    #[test]
    fn the_video_stage_makes_room_for_the_checklist() {
        let (mut p, mut host) = ready();
        p.on_event(&mut host, &Event::SetupNeeded { showing: true });
        assert_eq!(card(&host)["visible-card"], false);
        p.on_event(&mut host, &Event::SetupNeeded { showing: false });
        assert_eq!(card(&host)["visible-card"], true);
    }

    #[test]
    fn a_pick_shows_a_hidden_preview() {
        let (mut p, mut host) = ready();
        click(&mut p, &mut host, "toggle");
        assert_eq!(card(&host)["showing"], false);
        p.on_event(&mut host, &Event::PreviewPickChanged { owner: String::new() });
        assert_eq!(card(&host)["showing"], false, "the end of a pick shows nothing");
        p.on_event(&mut host, &Event::PreviewPickChanged { owner: "test".into() });
        assert_eq!(card(&host)["showing"], true);

        // The pop-out has the picture to pick on, so the card stays hidden for it
        click(&mut p, &mut host, "pop-out");
        p.on_event(&mut host, &Event::PreviewPickChanged { owner: "test".into() });
        assert_eq!(card(&host)["showing"], false);
    }

    fn boxes() -> Vec<LensBox> {
        vec![LensBox { label: "3\u{d7}".into(), x0: 0.33, y0: 0.33, x1: 0.67, y1: 0.67 }]
    }

    fn fire(p: &mut Preview, host: &mut FakeHost, token: &str) {
        assert!(host.is_scheduled(token), "{token} isn't waiting");
        host.scheduled.retain(|(_, t)| t != token);
        p.on_event(host, &Event::TimerFired { owner: "preview".into(), token: token.into() });
    }

    #[test]
    fn lens_boxes_show_for_a_moment_after_a_move_then_fade() {
        let (mut p, mut host) = ready();
        assert_eq!(card(&host)["lenses-enabled"], false);
        p.on_event(&mut host, &Event::LensBoxes { boxes: boxes(), moved: false });
        let c = card(&host);
        assert_eq!((c["boxes-opacity"].as_f64(), c["lenses-enabled"].as_bool()), (Some(0.0), Some(true)), "known, but nothing moved");
        assert_eq!(c["lens-boxes"][0]["label"], "3\u{d7}");
        assert!((c["lens-boxes"][0]["x1"].as_f64().unwrap() - 0.67).abs() < 1e-6);
        assert!(!host.is_scheduled(BOXES_HOLD_TIMER));
        p.on_event(&mut host, &Event::LensBoxes { boxes: boxes(), moved: true });
        assert_eq!(card(&host)["boxes-opacity"], 1.0);
        assert_eq!(host.scheduled.iter().find(|(_, t)| t == BOXES_HOLD_TIMER).map(|(ms, _)| *ms), Some(1000));
        fire(&mut p, &mut host, BOXES_HOLD_TIMER);
        assert_eq!(card(&host)["boxes-opacity"], 1.0, "the fade starts here");
        let mut steps = 0;
        while host.is_scheduled(BOXES_FADE_TIMER) {
            let before = card(&host)["boxes-opacity"].as_f64().unwrap();
            fire(&mut p, &mut host, BOXES_FADE_TIMER);
            assert!(card(&host)["boxes-opacity"].as_f64().unwrap() < before);
            steps += 1;
        }
        assert_eq!(card(&host)["boxes-opacity"], 0.0);
        assert_eq!(steps as u64 * FADE_STEP_MS, 300);

        click(&mut p, &mut host, "lenses-clicked");
        assert_eq!((card(&host)["boxes-opacity"].as_f64(), card(&host)["lenses-on"].as_bool()), (Some(1.0), Some(true)), "always on");
        assert_eq!(p.get_config(), json!({"lens_boxes": true, "skeleton": false}));
        p.set_config(&json!({"lens_boxes": false})).unwrap();
        p.on_profile_applied(&mut host);
        assert_eq!((card(&host)["boxes-opacity"].as_f64(), card(&host)["lenses-on"].as_bool()), (Some(0.0), Some(false)));

        p.on_event(&mut host, &Event::LensBoxes { boxes: vec![], moved: false });
        assert_eq!(card(&host)["lenses-enabled"], false);
    }

    #[test]
    fn smooth_is_off_until_clicked_and_comes_back_after_a_restart() {
        let (mut p, mut host) = ready();
        assert_eq!(card(&host)["smooth-on"], false);
        assert_eq!(p.get_config(), json!({"lens_boxes": false, "skeleton": false}), "nothing new in the file until it's used");
        click(&mut p, &mut host, "smooth-clicked");
        assert_eq!(card(&host)["smooth-on"], true);
        assert_eq!(host.calls.iter().filter(|c| *c == "schedule_save").count(), 1, "saved when it's clicked");
        let saved = p.get_config();
        assert_eq!(saved["smooth"], true);

        let (mut next, mut host) = ready();
        next.set_config(&saved).unwrap();
        next.on_profile_applied(&mut host);
        assert_eq!(card(&host)["smooth-on"], true);
        next.set_config(&json!({})).unwrap();
        next.on_profile_applied(&mut host);
        assert_eq!(card(&host)["smooth-on"], false);
    }

    #[test]
    fn another_move_starts_the_moment_over() {
        let (mut p, mut host) = ready();
        p.on_event(&mut host, &Event::LensBoxes { boxes: boxes(), moved: true });
        fire(&mut p, &mut host, BOXES_HOLD_TIMER);
        fire(&mut p, &mut host, BOXES_FADE_TIMER);
        p.on_event(&mut host, &Event::LensBoxes { boxes: boxes(), moved: true });
        assert_eq!(card(&host)["boxes-opacity"], 1.0);
        assert!(!host.is_scheduled(BOXES_FADE_TIMER) && host.is_scheduled(BOXES_HOLD_TIMER));
    }

    #[test]
    fn a_box_the_whole_frame_fits_inside_has_no_edge_to_show() {
        let (mut p, mut host) = ready();
        let whole = LensBox { label: "1\u{d7}".into(), x0: 0.0, y0: 0.0, x1: 1.0, y1: 1.0 };
        p.on_event(&mut host, &Event::LensBoxes { boxes: vec![whole, boxes().remove(0)], moved: true });
        let c = card(&host);
        assert_eq!(c["lens-boxes"].as_array().unwrap().len(), 1);
        assert_eq!(c["lens-boxes"][0]["label"], "3\u{d7}");
    }

    /// Shoulders at 0.45 and 0.55 of the width, 0.4 down; the person's left arm straight out on request.
    fn figure(arm_out: bool) -> Value {
        let mut points = vec![("left_shoulder", 0.55, 0.4, 0.99), ("right_shoulder", 0.45, 0.4, 0.99), ("left_hip", 0.53, 0.75, 0.9),
                              ("right_hip", 0.47, 0.75, 0.9), ("nose", 0.5, 0.28, 0.99), ("left_eye", 0.51, 0.26, 0.99), ("left_ear", 0.53, 0.27, 0.9)];
        if arm_out {
            points.extend([("left_elbow", 0.62, 0.42, 0.9), ("left_wrist", 0.69, 0.42, 0.9)]);
        }
        Person::new(points).to_value()
    }

    fn skeleton_card(state: &str) -> (Preview, FakeHost) {
        let (mut p, mut host) = (Preview::default(), FakeHost::new());
        host.vision = Some(state.into());
        host.focused = Some("phone".into());
        p.on_plugins_ready(&mut host);
        (p, host)
    }

    fn seen(p: &mut Preview, host: &mut FakeHost, source: &str, people: Vec<Value>) {
        p.on_event(host, &Event::PeopleSeen { source_id: source.into(), people });
    }

    fn lines(host: &FakeHost) -> Vec<Value> {
        card(host)["skeleton-lines"].as_array().cloned().unwrap_or_default()
    }

    fn close(a: &Value, b: f64) -> bool {
        (a.as_f64().unwrap() - b).abs() < 1e-6
    }

    #[test]
    fn the_skeleton_button_only_shows_with_the_vision_download() {
        for (state, shown) in [(vision::MISSING, false), (vision::FAILED, false), (vision::OFF, true), (vision::READY, true)] {
            let (_, host) = skeleton_card(state);
            assert_eq!(card(&host)["skeleton-shown"], shown, "{state}");
        }
        // The download went away with the button on
        let (mut p, mut host) = skeleton_card(vision::READY);
        click(&mut p, &mut host, "skeleton-clicked");
        host.vision = Some(vision::MISSING.into());
        host.calls.clear();
        p.on_event(&mut host, &Event::VisionStateChanged { state: vision::MISSING.into() });
        assert_eq!((card(&host)["skeleton-shown"].as_bool(), card(&host)["skeleton-on"].as_bool()), (Some(false), Some(false)));
        assert!(host.calls.iter().any(|c| c == "want_people Preview skeleton false"));
    }

    #[test]
    fn the_skeleton_draws_the_shown_streams_people_over_the_preview() {
        let (mut p, mut host) = skeleton_card(vision::READY);
        seen(&mut p, &mut host, "phone", vec![figure(false)]);
        assert!(lines(&host).is_empty(), "not asked for");
        host.calls.clear();
        click(&mut p, &mut host, "skeleton-clicked");
        assert!(host.calls.iter().any(|c| c == "want_people Preview skeleton true"));
        assert!(host.calls.iter().any(|c| c == "schedule_save"));
        seen(&mut p, &mut host, "another phone", vec![figure(false)]);
        assert!(lines(&host).is_empty());
        seen(&mut p, &mut host, "phone", vec![figure(false)]);
        let all = lines(&host);
        let shoulders: Vec<&Value> = all.iter().filter(|l| close(&l["y0"], 0.4) && close(&l["y1"], 0.4)).collect();
        assert_eq!(shoulders.len(), 1);
        let mut xs = [shoulders[0]["x0"].as_f64().unwrap(), shoulders[0]["x1"].as_f64().unwrap()];
        xs.sort_by(f64::total_cmp);
        assert!((xs[0] - 0.45).abs() < 1e-6 && (xs[1] - 0.55).abs() < 1e-6, "{xs:?}");
        host.calls.clear();
        click(&mut p, &mut host, "skeleton-clicked");
        assert!(lines(&host).is_empty());
        assert!(host.calls.iter().any(|c| c == "want_people Preview skeleton false"));
    }

    #[test]
    fn the_skeleton_follows_the_zoom_and_marks_an_arm_held_out() {
        let (mut p, mut host) = skeleton_card(vision::READY);
        click(&mut p, &mut host, "skeleton-clicked");
        seen(&mut p, &mut host, "phone", vec![figure(true)]);
        assert!(lines(&host).iter().any(|l| close(&l["y0"], 0.4) && close(&l["y1"], 0.4)), "zoomed out to begin with");
        host.view.zoom = 2.0;
        let view = host.view.clone();
        p.on_event(&mut host, &Event::ViewChanged { view });
        let all = lines(&host);
        let shoulders = all.iter().find(|l| close(&l["y0"], 0.3) && close(&l["y1"], 0.3)).expect("the shoulders, moved by the zoom");
        let mut xs = [shoulders["x0"].as_f64().unwrap(), shoulders["x1"].as_f64().unwrap()];
        xs.sort_by(f64::total_cmp);
        assert!((xs[0] - 0.4).abs() < 1e-6 && (xs[1] - 0.6).abs() < 1e-6 && shoulders["out"] == false, "{xs:?}");
        assert_eq!(all.iter().filter(|l| l["out"] == true).count(), 2, "shoulder to elbow and elbow to wrist, not the side of the body or the face");
    }

    #[test]
    fn the_skeleton_line_is_sure_only_of_two_points_the_model_is_sure_of() {
        let person = Person::new([("left_shoulder", 0.5, 0.4, 0.3), ("right_shoulder", 0.4, 0.4, 0.9), ("left_hip", 0.5, 0.7, 0.9), ("right_hip", 0.4, 0.7, 0.9)]);
        let lines = skeleton(&[person], &|x, y| (x, y), 16.0 / 9.0);
        let at = |from: (f64, f64), to: (f64, f64)| lines.iter().find(|l| (l.from == from && l.to == to) || (l.from == to && l.to == from)).unwrap().sure;
        assert!(!at((0.5, 0.4), (0.4, 0.4)));
        assert!(at((0.5, 0.7), (0.4, 0.7)));
    }

    #[test]
    fn people_go_when_the_stream_stops() {
        let (mut p, mut host) = skeleton_card(vision::READY);
        click(&mut p, &mut host, "skeleton-clicked");
        seen(&mut p, &mut host, "phone", vec![figure(false)]);
        assert!(!lines(&host).is_empty());
        p.on_stream_stop(&mut host);
        assert!(lines(&host).is_empty());
        assert_eq!(card(&host)["skeleton-on"], true, "the button stays on");
    }

    #[test]
    fn the_skeleton_is_saved_and_comes_back_only_with_vision_there() {
        let (mut p, mut host) = skeleton_card(vision::READY);
        p.set_config(&json!({"skeleton": true})).unwrap();
        host.calls.clear();
        p.on_profile_applied(&mut host);
        assert!(host.calls.iter().any(|c| c == "want_people Preview skeleton true"));
        assert_eq!(p.get_config()["skeleton"], true);

        let (mut p, mut host) = skeleton_card(vision::MISSING);
        p.set_config(&json!({"skeleton": true})).unwrap();
        host.calls.clear();
        p.on_profile_applied(&mut host);
        assert_eq!(p.get_config()["skeleton"], false, "a hidden button stays off");
        assert!(host.calls.iter().any(|c| c == "want_people Preview skeleton false"));
    }

    #[test]
    fn the_lens_and_skeleton_buttons_have_keys() {
        let (mut p, mut host) = skeleton_card(vision::MISSING);
        assert!(p.actions().iter().any(|a| a.id == "lens_outlines") && p.actions().iter().any(|a| a.id == "skeleton"));
        assert_eq!(p.run_action(&mut host, "lens_outlines", &Value::Null), None, "nothing to outline yet");
        assert_eq!(p.run_action(&mut host, "skeleton", &Value::Null), None, "hidden");
        p.on_event(&mut host, &Event::LensBoxes { boxes: boxes(), moved: false });
        assert!(p.run_action(&mut host, "lens_outlines", &Value::Null).is_some());
        assert_eq!(card(&host)["lenses-on"], true);
        host.vision = Some(vision::OFF.into());
        assert!(p.run_action(&mut host, "skeleton", &Value::Null).is_some());
        assert_eq!(card(&host)["skeleton-on"], true);
    }
}
