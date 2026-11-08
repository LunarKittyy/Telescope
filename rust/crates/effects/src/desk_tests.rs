use super::*;
use crate::sheet::tests::{desk_frame, CORNERS, H, W};
use telescope_core::frame::PixelFormat;
use telescope_core::host::{Pick, PickKind};
use telescope_core::testing::FakeHost;

struct Ctx;

impl FrameContext for Ctx {
    fn source_id(&self) -> &str {
        ""
    }
    fn person_mask(&self, _frame: &Frame) -> Option<Arc<Vec<u8>>> {
        None
    }
    fn colour_preview(&self, frame: &Frame) -> Frame {
        frame.clone()
    }
}

struct Rig {
    plugin: DeskPlugin,
    host: FakeHost,
}

fn full(points: &[P]) -> Vec<Value> {
    points.iter().map(|[x, y]| json!([x / W as f64, y / H as f64])).collect()
}

fn corners_json() -> Value {
    Value::Array(full(&CORNERS))
}

fn frame() -> Frame {
    let rgb = desk_frame();
    let mut data = Vec::new();
    for p in rgb.chunks(3) {
        data.extend([p[0], p[1], p[2], 255]);
    }
    Frame::new(W as u32, H as u32, PixelFormat::Rgba, data)
}

impl Rig {
    fn new(can_pick: bool) -> Rig {
        let mut host = FakeHost::new();
        host.can_pick = can_pick;
        let mut rig = Rig { plugin: DeskPlugin::new(), host };
        rig.plugin.setup(&mut rig.host);
        rig.event(Event::PhonesChanged { count: 1 });
        rig.plugin.set_config(&json!({})).unwrap();
        rig.event(Event::PhonesChanged { count: 1 });
        rig
    }

    fn event(&mut self, e: Event) {
        self.plugin.on_event(&mut self.host, &e);
    }

    fn ui(&mut self, callback: &str, args: Vec<Value>) {
        self.plugin.on_ui(&mut self.host, &UiEvent { ui: "card".into(), callback: callback.into(), args });
    }

    fn config(&mut self, cfg: Value) {
        self.plugin.set_config(&cfg).unwrap();
        self.event(Event::PhonesChanged { count: 1 });
    }

    fn shown(&self, key: &str) -> Value {
        self.host.calls.iter().rev().filter_map(|c| c.strip_prefix("set_ui_props card ")).filter_map(|j| serde_json::from_str::<Value>(j).ok())
            .find_map(|v| v.get(key).cloned()).unwrap_or(Value::Null)
    }

    fn status(&self) -> String {
        self.shown("status").as_str().unwrap().to_string()
    }

    fn saves(&self) -> usize {
        self.host.calls.iter().filter(|c| *c == "schedule_save").count()
    }

    fn pick(&mut self, points: &[P]) {
        let pick = Pick { kind: PickKind::Quad, shown: vec![], full: points.iter().map(|[x, y]| [(x / W as f64) as f32, (y / H as f64) as f32]).collect(), in_picture: vec![], picture: None };
        self.event(Event::PreviewPicked { owner: NAME.into(), pick: Some(pick) });
    }

    /// The step run on a frame, with the posts it made delivered back to the plugin.
    fn run(&mut self, mut f: Frame) -> Frame {
        if let Some(FrameStep::Cpu(mut step)) = self.plugin.frame_step() {
            step.process(&mut f, &Ctx);
        }
        let posts: Vec<(String, Value)> = self.host.posted.lock().unwrap().drain(..).collect();
        for (token, data) in posts {
            self.event(Event::Posted { owner: NAME.into(), token, data });
        }
        f
    }
}

#[test]
fn switching_on_without_corners_asks_for_them_and_a_pick_turns_it_on() {
    let mut r = Rig::new(true);
    r.ui("switch-toggled-by-hand", vec![json!(true)]);
    assert_eq!(r.host.picks, vec![None]);
    assert!(r.host.calls.contains(&"pick_on_preview desk Quad".to_string()));
    r.event(Event::PreviewPickChanged { owner: NAME.into() });
    assert_eq!(r.shown("picking"), json!(true));
    // the real picture to place corners on
    assert!(r.plugin.frame_step().is_none());
    let before = frame();
    assert_eq!(r.run(before.clone()).data, before.data);
    r.pick(&CORNERS);
    r.event(Event::PreviewPickChanged { owner: String::new() });
    assert_eq!(r.shown("on"), json!(true));
    assert_eq!(r.plugin.get_config()["on"], json!(true));
    assert_ne!(r.run(before.clone()).data, before.data);
    r.ui("mark-clicked", vec![]);
    // marking again starts from where they were
    let start = r.host.picks.last().unwrap().unwrap();
    for (at, [x, y]) in start.iter().zip(CORNERS) {
        assert!((at[0] as f64 - x / W as f64).abs() < 1e-6 && (at[1] as f64 - y / H as f64).abs() < 1e-6, "{start:?}");
    }
}

#[test]
fn a_cancelled_pick_changes_nothing() {
    let mut r = Rig::new(true);
    r.ui("mark-clicked", vec![]);
    r.event(Event::PreviewPicked { owner: NAME.into(), pick: None });
    assert_eq!(r.plugin.get_config()["corners"], Value::Null);
}

#[test]
fn corners_that_make_no_sheet_are_refused_and_the_card_says_why() {
    let mut r = Rig::new(true);
    r.config(json!({"on": true, "corners": corners_json()}));
    r.ui("mark-clicked", vec![]);
    r.event(Event::PreviewPickChanged { owner: NAME.into() });
    r.event(Event::PreviewPickChanged { owner: String::new() });
    r.pick(&[[32.0, 90.0], [128.0, 90.0], [192.0, 90.0], [288.0, 90.0]]);
    // kept what it had
    assert_eq!(r.plugin.get_config()["corners"], corners_json());
    assert_eq!(r.status(), "Sheet unclear");
    r.ui("mark-clicked", vec![]);
    r.event(Event::PreviewPickChanged { owner: NAME.into() });
    assert_ne!(r.status(), "Sheet unclear");
    // saved ones are checked too
    r.config(json!({"on": true, "corners": [[0.1, 0.5], [0.4, 0.5], [0.6, 0.5], [0.9, 0.5]]}));
    assert_eq!(r.plugin.get_config()["corners"], Value::Null);
}

#[test]
fn without_a_stream_the_card_says_to_start_one() {
    let mut r = Rig::new(false);
    r.ui("mark-clicked", vec![]);
    assert_eq!(r.status(), "Not streaming");
}

#[test]
fn settings_round_trip_and_bad_corners_are_dropped() {
    let mut r = Rig::new(true);
    let corners = json!([[0.1, 0.2], [0.9, 0.2], [0.9, 0.8], [0.1, 0.8]]);
    let cfg = json!({"on": true, "corners": corners, "cleanup": "bw", "shape": "a4", "stretch": [80, 130]});
    r.config(cfg.clone());
    assert_eq!(r.plugin.get_config(), cfg);
    assert_eq!(r.shown("clean-current"), json!(2));
    assert_eq!(r.shown("shape-current"), json!(1));
    assert_eq!(r.shown("stretch"), json!([80, 130]));
    assert_eq!(r.shown("stretch-text"), json!(["80%", "130%"]));
    r.config(json!({"on": true, "corners": [[0, 0], [1, "x"]], "cleanup": "sepia", "shape": "oval", "stretch": [500, true]}));
    assert_eq!(r.plugin.get_config(), json!({"on": false, "corners": null, "cleanup": "off", "shape": "auto", "stretch": [100, 100]}));
    // out of range, and an early build's switch
    r.config(json!({"stretch": [10, 900], "keep_shape": false}));
    assert_eq!(r.plugin.get_config()["stretch"], json!([50, 200]));
    assert_eq!(r.plugin.get_config()["shape"], json!("fill"));
    // a list can't be looked up in a dict
    assert!(r.plugin.set_config(&json!({"cleanup": ["bw"]})).is_err());
}

#[test]
fn the_stretch_sliders_save_and_reset_and_wait_while_the_sheet_fills_the_picture() {
    let mut r = Rig::new(true);
    assert_eq!(r.shown("reset-shown"), json!(false));
    let saves = r.saves();
    r.ui("stretch-moved", vec![json!(0), json!(150.0)]);
    assert_eq!(r.plugin.get_config()["stretch"], json!([150, 100]));
    assert!(r.saves() > saves);
    assert_eq!((r.shown("stretch-text"), r.shown("reset-shown")), (json!(["150%", "100%"]), json!(true)));
    r.ui("reset-clicked", vec![]);
    assert_eq!(r.plugin.get_config()["stretch"], json!([100, 100]));
    assert_eq!(r.shown("reset-shown"), json!(false));
    r.ui("stretch-moved", vec![json!(1), json!(70.0)]);
    r.ui("shape-picked", vec![json!(4)]);
    assert_eq!(r.plugin.get_config()["shape"], json!("fill"));
    assert_eq!((r.shown("stretch-enabled"), r.shown("reset-shown")), (json!(false), json!(false)));
}

#[test]
fn a_stream_behind_the_panels_keeps_the_straightening_it_had() {
    let mut r = Rig::new(true);
    assert!(r.plugin.frame_step().is_none());
    r.config(json!({"on": true, "corners": corners_json()}));
    let step = r.plugin.frame_step();
    r.config(json!({}));
    let mut f = frame();
    let Some(FrameStep::Cpu(mut step)) = step else { panic!("a step") };
    step.process(&mut f, &Ctx);
    assert!(f.data[(10 * W + W / 2) * 4] > 200);
}

#[test]
fn the_preview_learns_the_warp_to_map_clicks_back() {
    let mut r = Rig::new(true);
    r.config(json!({"on": true, "corners": corners_json(), "shape": "fill"}));
    r.run(frame());
    let m = r.host.warp.expect("a warp");
    let (x, y) = (CORNERS[0][0] as f32 / W as f32, CORNERS[0][1] as f32 / H as f32);
    let d = m[6] * x + m[7] * y + m[8];
    let (tx, ty) = ((m[0] * x + m[1] * y + m[2]) / d, (m[3] * x + m[4] * y + m[5]) / d);
    // the sheet's corner lands in the picture's corner
    assert!(tx.abs() < 0.03 && ty.abs() < 0.03, "{tx} {ty}");
    r.config(json!({}));
    assert!(r.host.warp.is_none());
}

#[test]
fn switching_off_and_on_again_tells_the_preview_the_warp_again() {
    let mut r = Rig::new(true);
    r.config(json!({"on": true, "corners": corners_json(), "shape": "fill"}));
    r.run(frame());
    assert!(r.host.warp.is_some());
    r.ui("switch-toggled-by-hand", vec![json!(false)]);
    r.run(frame());
    assert!(r.host.warp.is_none());
    // the same sheet, so the same warp
    r.ui("switch-toggled-by-hand", vec![json!(true)]);
    r.run(frame());
    // or clicks and picks land beside what's shown
    assert!(r.host.warp.is_some());
}

#[test]
fn a_sheet_pushed_off_the_picture_says_so() {
    let mut r = Rig::new(true);
    r.config(json!({"on": true, "corners": corners_json()}));
    assert_eq!(r.status(), "");
    r.event(Event::ViewChanged { view: View { zoom: 3.0, ..View::default() } });
    assert_eq!(r.status(), "Sheet cut off");
}

#[test]
fn it_is_an_optional_camera_card_in_the_geometry_stage() {
    let info = DeskPlugin::new().info();
    assert_eq!((info.name.as_str(), info.category.as_str(), info.icon.as_str()), ("desk", "camera", "pencil"));
    assert!(info.optional && info.in_presets && info.device_local && info.frame_stage == FrameStage::Geometry);
}

#[test]
fn the_shortcuts_switch_mark_and_pick_the_writing_and_shape() {
    let mut r = Rig::new(true);
    r.config(json!({"corners": corners_json()}));
    assert_eq!(r.plugin.run_action(&mut r.host, "desk.on", &json!({"mode": "on"})), Some("Desk mode: on".into()));
    assert!(r.plugin.get_config()["on"] == json!(true));
    assert!(r.plugin.run_action(&mut r.host, "desk.mark", &json!({})).is_some());
    assert_eq!(r.host.picks.len(), 1);
    r.plugin.run_action(&mut r.host, "desk.writing", &json!({"op": "set", "option": "Contrast"}));
    assert_eq!(r.plugin.get_config()["cleanup"], json!("contrast"));
    r.plugin.run_action(&mut r.host, "desk.shape", &json!({"op": "next"}));
    assert_eq!(r.plugin.get_config()["shape"], json!("a4"));
    let ids: Vec<String> = DeskPlugin::new().actions().into_iter().map(|a| a.id).collect();
    assert_eq!(ids, ["desk.on", "desk.mark", "desk.writing", "desk.shape"]);
}

#[test]
fn the_warp_is_worked_out_once_until_something_changes() {
    let mut r = Rig::new(true);
    assert!(r.plugin.frame_step().is_none());
    r.config(json!({"on": true, "corners": corners_json()}));
    let Some(FrameStep::Cpu(mut step)) = r.plugin.frame_step() else { panic!("a step") };
    let posts = |r: &Rig| r.host.posted.lock().unwrap().len();
    step.process(&mut frame(), &Ctx);
    step.process(&mut frame(), &Ctx);
    assert_eq!(posts(&r), 1);
    // another size of picture: a warp of its own
    let small = Frame::new(160, 90, PixelFormat::Rgba, [40, 40, 40, 255].repeat(160 * 90));
    step.process(&mut small.clone(), &Ctx);
    assert_eq!(posts(&r), 2);
    // a different shape is a different step with a warp worked out afresh; off and unmarked have none
    let matrix = |r: &Rig| r.host.posted.lock().unwrap().first().unwrap().1["matrix"].clone();
    let first = matrix(&r);
    r.ui("shape-picked", vec![json!(4)]);
    r.host.posted.lock().unwrap().clear();
    let Some(FrameStep::Cpu(mut filled)) = r.plugin.frame_step() else { panic!("a step") };
    let mut out = frame();
    filled.process(&mut out, &Ctx);
    assert_eq!(posts(&r), 1);
    assert_ne!(matrix(&r), first);
    let mut same = frame();
    step.process(&mut same, &Ctx);
    assert_ne!(out.data, same.data, "the step from before the change still has the old shape");
    r.config(json!({"on": false, "corners": corners_json()}));
    assert!(r.plugin.frame_step().is_none());
    r.config(json!({"on": true}));
    assert!(r.plugin.frame_step().is_none());
}

#[test]
fn a_frame_too_small_to_hold_a_sheet_passes_through() {
    let mut r = Rig::new(true);
    r.config(json!({"on": true, "corners": corners_json()}));
    let Some(FrameStep::Cpu(mut step)) = r.plugin.frame_step() else { panic!("a step") };
    for (w, h) in [(0, 0), (1, 1), (1, 40)] {
        let mut f = Frame::new(w, h, PixelFormat::Rgba, vec![9; (w * h * 4) as usize]);
        step.process(&mut f, &Ctx);
        assert!(f.data.iter().all(|&b| b == 9));
    }
}
