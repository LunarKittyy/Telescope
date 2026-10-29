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
        vec![ActionSpec { id: "show_hide".into(), label: tr("Show or hide the preview"), group: tr("Preview"), params: Vec::new(),
                          repeats: false, on_release: false, current: None, keywords: String::new() }]
    }

    fn run_action(&mut self, host: &mut dyn Host, id: &str, _params: &Value) -> Option<String> {
        (id == "show_hide").then(|| {
            self.toggle(host);
            String::new()
        })
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
            "toggle" => self.toggle(host),
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
