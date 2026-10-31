use super::*;
use telescope_core::frame::PixelFormat;
use telescope_core::testing::FakeHost;

struct Ctx;

impl FrameContext for Ctx {
    fn source_id(&self) -> &str {
        ""
    }
    fn person_mask(&self, _frame: &Frame) -> Option<std::sync::Arc<Vec<u8>>> {
        None
    }
    fn colour_preview(&self, frame: &Frame) -> Frame {
        frame.clone()
    }
}

struct Rig {
    plugin: NameTagPlugin,
    host: FakeHost,
}

impl Rig {
    fn new() -> Rig {
        let mut rig = Rig { plugin: NameTagPlugin::new(), host: FakeHost::new() };
        rig.plugin.setup(&mut rig.host);
        rig.plugin.set_config(&json!({})).unwrap();
        rig.plugin.on_event(&mut rig.host, &Event::PhonesChanged { count: 1 });
        rig
    }

    fn ui(&mut self, callback: &str, args: Vec<Value>) {
        self.plugin.on_ui(&mut self.host, &UiEvent { ui: "card".into(), callback: callback.into(), args });
    }

    fn count(&self, call: &str) -> usize {
        self.host.calls.iter().filter(|c| *c == call).count()
    }

    fn shown(&self, key: &str) -> Value {
        self.host.calls.iter().rev().filter_map(|c| c.strip_prefix("set_ui_props card ")).filter_map(|j| serde_json::from_str::<Value>(j).ok())
            .find_map(|v| v.get(key).cloned()).unwrap_or(Value::Null)
    }
}

fn frame(w: u32, h: u32, grey: u8) -> Frame {
    Frame::new(w, h, PixelFormat::Rgba, [grey, grey, grey, 255].repeat((w * h) as usize))
}

fn run(step: Option<FrameStep>, mut f: Frame) -> Frame {
    if let Some(FrameStep::Cpu(mut step)) = step {
        step.process(&mut f, &Ctx);
    }
    f
}

fn have_fonts() -> bool {
    let here = tag::Fonts::system().is_some();
    if !here {
        eprintln!("no font on this machine: the name tag drawing is skipped");
    }
    here
}

#[test]
fn off_by_default_and_until_there_is_text() {
    if !have_fonts() {
        return;
    }
    let mut r = Rig::new();
    assert!(r.plugin.frame_step().is_none());
    assert_eq!(run(r.plugin.frame_step(), frame(640, 360, 70)).data, frame(640, 360, 70).data);
    r.plugin.set_config(&json!({"on": true})).unwrap();
    assert!(r.plugin.frame_step().is_none());
    r.ui("text-edited", vec![json!(0), json!("Luna")]);
    assert!(r.plugin.frame_step().is_some());
    assert_ne!(run(r.plugin.frame_step(), frame(640, 360, 70)).data, frame(640, 360, 70).data);
}

#[test]
fn settings_round_trip_through_presets() {
    let mut r = Rig::new();
    let cfg = json!({"on": true, "name": "Luna", "pronouns": "she/her", "second": "Host", "style": "pill", "position": "top_right", "size": "large"});
    r.plugin.apply_preset(&mut r.host, &cfg).unwrap();
    assert_eq!(r.plugin.get_config(), cfg);
    let info = r.plugin.info();
    assert!(info.in_presets && info.optional && info.device_local && info.frame_stage == FrameStage::Overlay);
}

#[test]
fn bad_settings_fall_back_to_the_defaults() {
    let mut r = Rig::new();
    r.plugin.set_config(&json!({"on": "yes", "name": 5, "style": "neon", "position": "middle", "size": "huge", "second": "x".repeat(500)})).unwrap();
    let cfg = r.plugin.get_config();
    assert_eq!(cfg["on"], json!(false));
    assert_eq!(cfg["name"], json!(""));
    assert_eq!(cfg["second"].as_str().unwrap().len(), MAX_CHARS);
    assert_eq!((&cfg["style"], &cfg["position"], &cfg["size"]), (&json!(DEFAULT_STYLE), &json!(DEFAULT_POSITION), &json!(DEFAULT_SIZE)));
}

#[test]
fn a_size_that_cannot_be_compared_is_refused_like_python_does() {
    let mut r = Rig::new();
    assert!(r.plugin.set_config(&json!({"size": ["large"]})).is_err());
    assert!(r.plugin.set_config(&json!({"style": ["bar"]})).is_ok());
}

#[test]
fn typing_saves_and_hands_the_frames_a_new_step() {
    let mut r = Rig::new();
    let (saves, steps) = (r.count("schedule_save"), r.count("refresh_frame_step"));
    r.ui("text-edited", vec![json!(0), json!("Luna")]);
    assert_eq!((r.count("schedule_save"), r.count("refresh_frame_step")), (saves + 1, steps + 1));
    assert_eq!(r.plugin.get_config()["name"], json!("Luna"));
    r.ui("choice-picked", vec![json!(0), json!(2)]);
    assert_eq!(r.plugin.get_config()["style"], json!("light"));
    r.ui("choice-picked", vec![json!(1), json!(4)]);
    assert_eq!(r.plugin.get_config()["position"], json!("top_right"));
    r.ui("choice-picked", vec![json!(2), json!(2)]);
    assert_eq!(r.plugin.get_config()["size"], json!("large"));
}

#[test]
fn a_box_is_held_to_eighty_letters() {
    let mut r = Rig::new();
    r.ui("text-edited", vec![json!(1), json!("x".repeat(120))]);
    assert_eq!(r.plugin.get_config()["pronouns"].as_str().unwrap().len(), MAX_CHARS);
    assert_eq!(r.shown("pronouns").as_str().unwrap().len(), MAX_CHARS);
}

#[test]
fn typing_saves_and_swaps_the_settings_whole() {
    let mut r = Rig::new();
    r.plugin.set_config(&json!({"on": true})).unwrap();
    let before = r.plugin.step().spec;
    let saves = r.count("schedule_save");
    r.ui("text-edited", vec![json!(0), json!("Luna")]);
    assert!(r.count("schedule_save") > saves);
    assert_eq!((before.name.as_str(), r.plugin.step().spec.name.as_str()), ("", "Luna"));
    let frozen = r.plugin.step();
    r.ui("choice-picked", vec![json!(0), json!(2)]);
    // a frozen step keeps its look
    assert_eq!((frozen.spec.style.as_str(), r.plugin.step().spec.style.as_str()), ("bar", "light"));
}

#[test]
fn a_frozen_step_keeps_its_look() {
    if !have_fonts() {
        return;
    }
    let mut r = Rig::new();
    r.plugin.set_config(&json!({"on": true, "name": "Luna"})).unwrap();
    let before = run(r.plugin.frame_step(), frame(640, 360, 70));
    let frozen = r.plugin.frame_step();
    r.ui("choice-picked", vec![json!(0), json!(2)]);
    let after = run(r.plugin.frame_step(), frame(640, 360, 70));
    assert_ne!(before.data, after.data);
    assert_eq!(run(frozen, frame(640, 360, 70)).data, before.data);
}

#[test]
fn mirroring_the_picture_doesnt_mirror_the_tag() {
    if !have_fonts() {
        return;
    }
    // The window's view (Transforms' flip) runs before every plugin, so the tag lands on the picture as sent
    let mut r = Rig::new();
    r.plugin.set_config(&json!({"on": true, "name": "Luna", "position": "bottom_left"})).unwrap();
    let side = |left: u8, right: u8| {
        let mut f = frame(640, 360, left);
        for row in f.data_mut().chunks_mut(640 * 4) {
            for px in row[320 * 4..].chunks_mut(4) {
                px[..3].fill(right);
            }
        }
        f
    };
    let plain = run(r.plugin.frame_step(), side(200, 0));
    let flipped = run(r.plugin.frame_step(), side(0, 200));
    let tag = tag::render(&Spec { name: "Luna".into(), ..Default::default() }, 640, 360).unwrap();
    assert!(tag.x < 320);
    let region = |f: &Frame| -> Vec<u8> {
        (tag.y..tag.y + tag.height).flat_map(|y| f.data[(y * 640 + tag.x) * 4..(y * 640 + tag.x + tag.width) * 4].to_vec()).collect()
    };
    let mut on_200 = frame(640, 360, 200);
    tag::apply(&mut on_200, &tag);
    let mut on_0 = frame(640, 360, 0);
    tag::apply(&mut on_0, &tag);
    // drawn the right way round on the original, and on the flipped picture the same text, not mirrored
    assert_eq!(region(&plain), region(&on_200));
    assert_eq!(region(&flipped), region(&on_0));
}

#[test]
fn it_runs_after_the_background_plugin() {
    assert!(NameTagPlugin::new().info().frame_stage > FrameStage::Background);
    assert_eq!(NameTagPlugin::new().info().frame_stage, FrameStage::Overlay);
}

#[test]
fn a_stream_behind_the_panels_keeps_a_cache_of_its_own() {
    if !have_fonts() {
        return;
    }
    let mut r = Rig::new();
    r.plugin.set_config(&json!({"on": true, "name": "Luna"})).unwrap();
    let mut other = r.plugin.step();
    let mut live = r.plugin.step();
    live.process(&mut frame(1280, 720, 0), &Ctx);
    other.process(&mut frame(640, 360, 0), &Ctx);
    live.process(&mut frame(1280, 720, 0), &Ctx);
    // the other size didn't throw it out
    assert_eq!(live.cache.renders, 1);
}

#[test]
fn the_switch_is_a_shortcut_that_toggles_holds_and_says_so() {
    let mut r = Rig::new();
    assert_eq!(r.plugin.run_action(&mut r.host, "name_tag.on", &json!({})), Some("Name tag: on".into()));
    assert!(r.plugin.get_config()["on"] == json!(true));
    assert_eq!(r.plugin.run_action(&mut r.host, "name_tag.on", &json!({"mode": "off"})), Some("Name tag: off".into()));
    r.plugin.run_action(&mut r.host, "name_tag.on", &json!({"mode": "hold"}));
    assert!(r.plugin.get_config()["on"] == json!(true));
    r.plugin.release_action(&mut r.host, "name_tag.on", &json!({"mode": "hold"}));
    assert!(r.plugin.get_config()["on"] == json!(false));
}

#[test]
fn the_card_hears_the_settings_that_were_loaded() {
    let mut r = Rig::new();
    r.plugin.set_config(&json!({"on": true, "name": "Luna", "style": "light", "size": "large"})).unwrap();
    r.plugin.on_event(&mut r.host, &Event::PhonesChanged { count: 1 });
    assert_eq!((r.shown("on"), r.shown("name"), r.shown("style-current"), r.shown("size-current")), (json!(true), json!("Luna"), json!(2), json!(2)));
}
