//! plugins/preview.py: the video stage in the middle of the window and the camera button under it. The frames
//! themselves are drawn by the window (preview.rs); this keeps what the stage says and the toolbar.

use serde_json::{json, Value};
use telescope_core::i18n::{n_, tr};
use telescope_core::plugin::{ConfigRejected, Plugin, PluginInfo};
use telescope_core::ui::{ActionSpec, CardSpec, SlintSource, UiEvent, UiSpec};
use telescope_core::{Event, Host};

// Translators: the preview area doesn't wrap lines, so keep the line breaks and each line about as long as this one
const IDLE_TEXT: &str = "Not streaming\n\nPress Start streaming and the phone's camera comes up on its own.";
const IDLE_BROWSER_TEXT: &str = "Not streaming\n\nPress Start streaming and the browser's camera comes up once it connects.";
const IDLE_WEBCAM_TEXT: &str = "Not streaming\n\nPress Start streaming to use this camera.";
const IDLE_MIC_TEXT: &str = "Not streaming\n\nStart mic only streams the phone's mic with its camera off.";
const WAITING_TEXT: &str = "Waiting for the first frame\u{2026}";
const CAMERA_OFF_TEXT: &str = "Camera off\n\nThe phone's mic is still streaming.\nStop streaming ends it.";
// Broken by hand like the rest: the stage doesn't wrap, and these are its longest lines
const CAMERA_OFF_AUTO_TEXT: &str = "Camera off\n\nNo app was using the camera, so it turned off.\nThe phone's mic is still streaming, and the camera\ncomes back when an app opens it.";

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
    /// The Lens outlines button, kept as saved though it stays disabled until lenses send boxes.
    lens_boxes: bool,
    skeleton: bool,
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
        });
        if clear || self.hidden {
            props["has-frame"] = json!(false);
        }
        host.set_ui_props("card", &props);
    }

    fn toggle(&mut self, host: &mut dyn Host) {
        self.hidden = !self.hidden;
        self.render(host, true);
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
    }

    fn actions(&mut self) -> Vec<ActionSpec> {
        let action = |id: &str, label: String| ActionSpec { id: id.into(), label, group: tr("Preview"), params: Vec::new(),
                                                             repeats: false, on_release: false, current: None, keywords: String::new() };
        vec![action("show_hide", tr("Show or hide the preview")), action("pop_out", tr("Pop out the preview"))]
    }

    fn run_action(&mut self, host: &mut dyn Host, id: &str, _params: &Value) -> Option<String> {
        match id {
            // Hide waits while the pop-out is open, as its button does
            "show_hide" if !self.popout => self.toggle(host),
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
            _ => return,
        }
        self.render(host, false);
    }

    fn on_stream_start(&mut self, host: &mut dyn Host, _stream: &telescope_core::plugin::StreamInfo) {
        self.render(host, true);
    }

    fn on_stream_stop(&mut self, host: &mut dyn Host) {
        self.render(host, true);
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
            "camera-clicked" => {
                let on = host.is_camera_on();
                host.set_camera_on(!on, false);
            }
            _ => {}
        }
    }

    fn get_config(&self) -> Value {
        json!({"lens_boxes": self.lens_boxes, "skeleton": self.skeleton})
    }

    fn set_config(&mut self, cfg: &Value) -> Result<(), ConfigRejected> {
        self.lens_boxes = cfg.get("lens_boxes") == Some(&Value::Bool(true));
        // The Skeleton button only shows once vision is ported, and a hidden button stays off
        self.skeleton = false;
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
        let stream = telescope_core::plugin::StreamInfo { url: "http://phone/".into(), source_id: "phone-1".into() };
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

    #[test]
    fn camera_off_while_streaming_shows_the_note_with_its_icon() {
        let (mut p, mut host) = ready();
        host.streaming = true;
        host.camera_off = true;
        p.on_camera_off(&mut host);
        let c = card(&host);
        assert_eq!((c["stage-text"].as_str(), c["camera-off"].as_bool()), (Some(CAMERA_OFF_TEXT), Some(true)));
    }
}
