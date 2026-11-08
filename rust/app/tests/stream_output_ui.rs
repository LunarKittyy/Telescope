// The Stream output card drawn with the kit's real widgets (test_plugins.py's test_stream_quality_and_bitrate_reset_on_a_double_click).
use i_slint_backend_testing::ElementHandle;
use slint::platform::{PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition};
use slint_interpreter::{ComponentInstance, Value};
use serde_json::{json, Value as Json};
use std::cell::RefCell;
use std::rc::Rc;
use telescope_core::event::Event;
use telescope_core::plugin::{Plugin, StreamInfo};
use telescope_core::testing::FakeHost;
use telescope_core::ui::{SlintSource, UiEvent};
use telescope_outputs::stream_output::StreamOutputPlugin;
use telescope_shell::source::to_slint;

fn double_click(window: &slint::Window, e: &ElementHandle) {
    let (p, s) = (e.absolute_position(), e.size());
    let position = LogicalPosition::new(p.x + s.width / 3.0, p.y + s.height / 2.0);
    window.dispatch_event(WindowEvent::PointerMoved { position });
    for _ in 0..2 {
        window.dispatch_event(WindowEvent::PointerPressed { position, button: PointerEventButton::Left });
        window.dispatch_event(WindowEvent::PointerReleased { position, button: PointerEventButton::Left });
    }
}

#[test]
fn stream_quality_and_bitrate_reset_on_a_double_click() {
    i_slint_backend_testing::init_no_event_loop();
    let card = StreamOutputPlugin::new(true).card().expect("a card").ui;
    let SlintSource::Text(source) = card.source else { panic!("source text") };
    let def = telescope_shell::compile::compile("stream_output.slint", &source, vec![], &card.component).expect("the card compiles");
    let moved: Rc<RefCell<Vec<(String, f64)>>> = Rc::default();
    for name in ["quality-moved", "bitrate-moved"] {
        let (m, n) = (moved.clone(), name.to_string());
        let ui = def.create().unwrap();
        ui.set_callback(name, move |args| {
            if let Some(Value::Number(v)) = args.first() {
                m.borrow_mut().push((n.clone(), *v));
            }
            Value::Void
        }).unwrap();
        let (label, at) = if name == "quality-moved" { ("JPEG quality", 40.0) } else { ("Bitrate", 12.0) };
        ui.set_property("h264", Value::Bool(name == "bitrate-moved")).unwrap();
        ui.set_property(if name == "quality-moved" { "quality" } else { "bitrate" }, Value::Number(at)).unwrap();
        ui.set_property("bitrate-tip", Value::String("All the way right is Dynamic".into())).unwrap();
        ui.window().set_size(slint::LogicalSize::new(460.0, 400.0));
        ui.show().unwrap();
        let slider = ElementHandle::find_by_accessible_label(&ui, label).find(|e| e.accessible_value_maximum().is_some()).expect("the slider");
        double_click(ui.window(), &slider);
        if name == "bitrate-moved" {
            assert_eq!(slider.accessible_description().as_deref(), Some("Bitrate. All the way right is Dynamic. Double-click to reset"), "its own tip stays, with what a double-click does");
        }
    }
    let moved = moved.borrow();
    assert_eq!(moved.iter().rev().find(|(n, _)| n == "quality-moved").map(|m| m.1), Some(85.0));
    assert_eq!(moved.iter().rev().find(|(n, _)| n == "bitrate-moved").map(|m| m.1), Some(0.0), "0 is Auto");
}

/// The card drawn from the real plugin, with what's done on it going back to the plugin as in the window.
struct Drawn {
    plugin: StreamOutputPlugin,
    host: FakeHost,
    ui: ComponentInstance,
    done: Rc<RefCell<Vec<(String, Json)>>>,
}

impl Drawn {
    fn new() -> Drawn {
        i_slint_backend_testing::init_no_event_loop();
        let mut plugin = StreamOutputPlugin::new(true);
        let card = plugin.card().expect("a card").ui;
        let SlintSource::Text(source) = card.source else { panic!("source text") };
        let def = telescope_shell::compile::compile("stream_output.slint", &source, vec![], &card.component).expect("the card compiles");
        let ui = def.create().unwrap();
        let done: Rc<RefCell<Vec<(String, Json)>>> = Rc::default();
        for name in ["quality-moved", "bitrate-moved", "format-picked"] {
            let (d, n) = (done.clone(), name.to_string());
            ui.set_callback(name, move |args| {
                match args.first() {
                    Some(Value::Number(v)) => d.borrow_mut().push((n.clone(), json!(v))),
                    Some(Value::Bool(b)) => d.borrow_mut().push((n.clone(), json!(b))),
                    _ => {}
                }
                Value::Void
            }).unwrap();
        }
        ui.window().set_size(slint::LogicalSize::new(460.0, 400.0));
        ui.show().unwrap();
        let mut host = FakeHost::new();
        host.focused = Some("phone-a".into());
        let mut drawn = Drawn { plugin, host, ui, done };
        drawn.plugin.on_event(&mut drawn.host, &Event::PhonesChanged { count: 1 });
        drawn.plugin.on_stream_start(&mut drawn.host, &StreamInfo { source_id: "phone-a".into(), url: "url".into(), session: 1 });
        drawn.draw();
        drawn
    }

    fn draw(&self) {
        for (name, value) in self.plugin.props().as_object().unwrap() {
            if let Ok(like) = self.ui.get_property(name) {
                self.ui.set_property(name, to_slint(value, &like)).unwrap();
            }
        }
    }

    /// What was done on the card reaches the plugin, and the card shows the outcome.
    fn settle(&mut self) {
        for (callback, value) in self.done.take() {
            self.plugin.on_ui(&mut self.host, &UiEvent { ui: "card".into(), callback, args: vec![value] });
        }
        self.draw();
    }

    fn slider(&self, label: &str) -> Option<i_slint_backend_testing::ElementHandle> {
        ElementHandle::find_by_accessible_label(&self.ui, label).find(|e| e.accessible_value_maximum().is_some())
    }

    fn option(&self, label: &str) -> ElementHandle {
        ElementHandle::find_by_accessible_label(&self.ui, &format!("Format: {label}")).find(|e| e.accessible_checked().is_some()).expect("the button")
    }

    fn click(&mut self, label: &str) {
        let e = self.option(label);
        let (p, s) = (e.absolute_position(), e.size());
        let position = LogicalPosition::new(p.x + s.width / 2.0, p.y + s.height / 2.0);
        let window = self.ui.window();
        window.dispatch_event(WindowEvent::PointerMoved { position });
        window.dispatch_event(WindowEvent::PointerPressed { position, button: PointerEventButton::Left });
        window.dispatch_event(WindowEvent::PointerReleased { position, button: PointerEventButton::Left });
        self.settle();
    }

    /// Where the slider's handle sits at value v (track.inset in slider.slint).
    fn at(slider: &ElementHandle, v: f32) -> LogicalPosition {
        let (p, s) = (slider.absolute_position(), slider.size());
        let (low, high) = (slider.accessible_value_minimum().unwrap(), slider.accessible_value_maximum().unwrap());
        LogicalPosition::new(p.x + 7.0 + (v - low) / (high - low) * (s.width - 14.0), p.y + s.height / 2.0)
    }

    /// Takes the handle at `from` and lets go of it at `to`; the value the plugin last heard.
    fn drag(&mut self, slider: &ElementHandle, from: f32, to: f32) -> Json {
        let window = self.ui.window();
        let (start, end) = (Self::at(slider, from), Self::at(slider, to));
        window.dispatch_event(WindowEvent::PointerMoved { position: start });
        window.dispatch_event(WindowEvent::PointerPressed { position: start, button: PointerEventButton::Left });
        window.dispatch_event(WindowEvent::PointerMoved { position: end });
        window.dispatch_event(WindowEvent::PointerReleased { position: end, button: PointerEventButton::Left });
        let last = self.done.borrow().last().expect("the slider moved").1.clone();
        self.settle();
        last
    }

    fn sent(&self) -> Vec<Json> {
        self.host.calls.iter().filter_map(|c| c.strip_prefix("phone_control ")).map(|c| serde_json::from_str(c.split_once(' ').unwrap().1).unwrap()).collect()
    }

    fn reconnects(&self) -> usize {
        self.host.calls.iter().filter(|c| *c == "reconnect_stream").count()
    }

    fn says(&self, prop: &str) -> Json {
        self.plugin.props()[prop].clone()
    }
}

#[test]
fn bitrate_goes_up_to_100_mbps() {
    let mut c = Drawn::new();
    c.ui.invoke("bitrate-moved", &[Value::Number(100.0)]).unwrap();
    c.settle();
    assert_eq!(c.sent().last().unwrap(), &json!({"action": "bitrate", "value": 100_000_000}));
    assert_eq!(c.says("bitrate-readout"), "100 Mbps");
    assert_eq!(c.slider("Bitrate").expect("the slider").accessible_value_maximum(), Some(101.0), "then Dynamic");
    c.plugin.set_config(&json!({"bitrate_mbps": 80})).unwrap();
    assert_eq!(c.says("bitrate-readout"), "80 Mbps");
}

#[test]
fn jpeg_quality_stops_at_95_and_marks_the_recommended_spot() {
    let mut c = Drawn::new();
    c.click("Heavy");
    let slider = c.slider("JPEG quality").expect("the slider");
    assert_eq!(slider.accessible_value_maximum(), Some(95.0));
    assert!(slider.accessible_description().unwrap().contains("recommended"), "{:?}", slider.accessible_description());
    // A drag sticks to the mark from close by, and only there
    assert_eq!(c.drag(&slider, 40.0, 84.0), json!(85.0));
    assert_eq!(c.drag(&slider, 20.0, 70.0), json!(70.0));
    c.plugin.set_config(&json!({"jpeg_quality": 100})).unwrap(); // saved before the cap
    assert_eq!(c.plugin.get_config()["jpeg_quality"], 95);
}

#[test]
fn light_is_the_default_and_switching_reconnects() {
    let mut c = Drawn::new();
    assert_eq!(c.plugin.stream_format(), "h264");
    let (light, heavy) = (c.option("Light"), c.option("Heavy"));
    assert_eq!(light.accessible_checked(), Some(true));
    assert_eq!(c.says("h264-enabled"), true, "offered before the phone has reported");
    assert!(c.says("light-tip").as_str().unwrap().contains("H.264"));
    assert!(c.says("heavy-tip").as_str().unwrap().contains("MJPEG"));
    assert_eq!(heavy.accessible_checked(), Some(false));
    assert!(c.slider("Bitrate").is_some() && c.slider("JPEG quality").is_none());

    c.plugin.on_phone_state(&mut c.host, &json!({"cameras": [], "codecs": ["mjpeg", "h264"]}));
    assert_eq!(c.plugin.stream_format(), "h264");
    assert_eq!(c.reconnects(), 0);

    c.click("Heavy");
    assert_eq!(c.plugin.stream_format(), "mjpeg");
    assert_eq!(c.reconnects(), 1);
    assert_eq!(c.plugin.get_config()["format"], "mjpeg");
    assert!(c.slider("JPEG quality").is_some() && c.slider("Bitrate").is_none());

    c.click("Light");
    assert_eq!(c.plugin.stream_format(), "h264");
    assert_eq!(c.reconnects(), 2);
}

// dim_until_paired disables the card, not only its look
#[test]
fn without_a_phone_the_card_takes_no_clicks() {
    let mut c = Drawn::new();
    c.plugin.on_event(&mut c.host, &Event::PhonesChanged { count: 0 });
    c.draw();
    c.click("Heavy");
    assert_eq!(c.plugin.stream_format(), "h264");
    c.plugin.on_event(&mut c.host, &Event::PhonesChanged { count: 1 });
    c.draw();
    c.click("Heavy");
    assert_eq!(c.plugin.stream_format(), "mjpeg");
}
