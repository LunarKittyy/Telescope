use super::*;
use telescope_core::frame::PixelFormat;
use telescope_core::testing::FakeHost;

const W: usize = 160;
const H: usize = 90;

fn room() -> Rgb {
    let mut r = Rgb::new(W, H);
    for y in 0..H {
        for x in 0..W {
            r.set(x, y, [70, (60.0 + 40.0 * y as f32 / (H - 1) as f32) as u8, (40.0 + 80.0 * x as f32 / (W - 1) as f32) as u8]);
        }
    }
    r
}

fn with_person(room: &Rgb, x0: usize, x1: usize) -> Rgb {
    let mut f = room.clone();
    for y in 20..80 {
        for x in x0..x1 {
            f.set(x, y, [240, 220, 20]);
        }
    }
    f
}

fn frame_of(rgb: &Rgb) -> Frame {
    let mut f = Frame::new(rgb.w as u32, rgb.h as u32, PixelFormat::Rgba, vec![0; rgb.w * rgb.h * 4]);
    rgb.write_to(&mut f);
    f
}

struct Ctx(&'static str);

impl FrameContext for Ctx {
    fn source_id(&self) -> &str {
        self.0
    }
    fn person_mask(&self, _frame: &Frame) -> Option<Arc<Vec<u8>>> {
        None
    }
    fn colour_preview(&self, frame: &Frame) -> Frame {
        frame.clone()
    }
}

struct Rig {
    plugin: FunPackPlugin,
    host: FakeHost,
}

impl Rig {
    /// A card with a phone paired and the saved settings loaded, as the window does at startup.
    fn new() -> Rig {
        let mut rig = Rig { plugin: FunPackPlugin::new(), host: FakeHost::new() };
        rig.host.focused = Some("phone".into());
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

    fn pick(&mut self, effect: &str) {
        let i = EFFECTS.iter().position(|(k, _)| *k == effect).unwrap();
        self.ui("effect-picked", vec![json!(i)]);
    }

    fn shown(&self, key: &str) -> Value {
        self.host.calls.iter().rev().filter_map(|c| c.strip_prefix("set_ui_props card ")).filter_map(|j| serde_json::from_str::<Value>(j).ok())
            .find_map(|v| v.get(key).cloned()).unwrap_or(Value::Null)
    }

    fn calls(&self, what: &str) -> usize {
        self.host.calls.iter().filter(|c| *c == what).count()
    }

    fn look(&self) -> Arc<Look> {
        self.plugin.hub.state.lock().unwrap().look.clone()
    }

    /// What the shown stream's step does with a frame, and what the plugin hears about it after.
    fn stream(&mut self, source: &'static str, rgb: &Rgb) -> Rgb {
        let Some(FrameStep::Cpu(mut step)) = self.plugin.frame_step() else { panic!("a CPU step") };
        let mut f = frame_of(rgb);
        step.process(&mut f, &Ctx(source));
        for (token, data) in self.host.take_posted() {
            self.event(Event::Posted { owner: NAME.into(), token, data });
        }
        Rgb::from_frame(&f).unwrap()
    }

    fn count_down(&mut self, what: &str, ticks: usize) {
        self.ui(if what == "plate" { "plate-clicked" } else { "clone-clicked" }, vec![]);
        for _ in 0..ticks {
            self.event(Event::TimerFired { owner: NAME.into(), token: "tick".into() });
        }
    }

    fn take_room(&mut self, room: &Rgb) {
        self.count_down("plate", 3);
        self.stream("phone", room);
        assert!(self.plugin.plate.is_some());
    }
}

#[test]
fn off_by_default_with_nothing_on_the_stream() {
    let mut r = Rig::new();
    assert_eq!(r.plugin.get_config()["effect"], json!("off"));
    assert!(r.plugin.frame_step().is_none());
    assert!(!r.host.calls.iter().any(|c| c == "change_packets fun_pack true" || c == "want_people fun_pack true"));
}

#[test]
fn datamosh_puts_a_packet_step_on_and_takes_it_off() {
    let mut r = Rig::new();
    r.pick("mosh");
    assert_eq!(r.calls("change_packets fun_pack true"), 1);
    // it's all in the packets
    assert!(r.plugin.frame_step().is_none());
    r.pick("vhs");
    assert_eq!(r.calls("change_packets fun_pack false"), 1);
}

#[test]
fn datamosh_on_heavy_says_why_and_changes_no_packets() {
    let mut r = Rig::new();
    r.host.packets_problem = "Needs Light".into();
    r.pick("mosh");
    assert_eq!(r.calls("change_packets fun_pack true"), 0);
    assert_eq!((r.shown("problem"), r.shown("show-clean")), (json!("Needs Light"), json!(false)));
    r.host.packets_problem = String::new();
    r.event(Event::StreamTransportChanged);
    assert_eq!(r.calls("change_packets fun_pack true"), 1);
    assert_eq!(r.shown("problem"), json!(""));
}

#[test]
fn clean_up_lets_a_keyframe_through() {
    let mut r = Rig::new();
    r.pick("mosh");
    let before = r.host.calls.len();
    r.ui("clean-clicked", vec![]);
    let calls: Vec<&String> = r.host.calls[before..].iter().filter(|c| c.starts_with("change_packets")).collect();
    // off and on again, which asks the phone for a keyframe
    assert_eq!(calls, ["change_packets fun_pack false", "change_packets fun_pack true"]);
}

#[test]
fn the_stick_figure_asks_for_people_and_follows_them() {
    let mut r = Rig::new();
    r.pick("stick");
    assert_eq!(r.calls("want_people fun_pack true"), 1);
    let person = json!({"points": {"left_shoulder": [0.4, 0.4, 1.0], "right_shoulder": [0.6, 0.4, 1.0]}});
    r.event(Event::PeopleSeen { source_id: "other".into(), people: vec![person.clone()] });
    assert!(r.look().lines.is_empty());
    r.event(Event::PeopleSeen { source_id: "phone".into(), people: vec![person] });
    assert_eq!(r.look().lines.len(), 1);
    r.pick("off");
    assert_eq!(r.calls("want_people fun_pack false"), 1);
}

#[test]
fn shutting_down_lets_go_of_packets_and_people() {
    let mut r = Rig::new();
    r.pick("mosh");
    r.plugin.shutdown(&mut r.host);
    assert_eq!(r.calls("change_packets fun_pack false"), 1);
    let mut r = Rig::new();
    r.pick("stick");
    r.plugin.shutdown(&mut r.host);
    assert_eq!(r.calls("want_people fun_pack false"), 1);
}

#[test]
fn asking_for_people_only_once() {
    let mut r = Rig::new();
    r.pick("stick");
    r.event(Event::StreamStopped);
    r.event(Event::VisionStateChanged { state: "loading".into() });
    assert_eq!(r.calls("want_people fun_pack true"), 1);
}

#[test]
fn the_empty_room_and_a_clone_are_taken_from_the_next_frame() {
    let mut r = Rig::new();
    r.pick("clones");
    let room = room();
    r.ui("plate-clicked", vec![]);
    assert_eq!(r.shown("plate-text"), json!("Taking it in 3…"));
    assert!(r.host.is_scheduled("tick"));
    for _ in 0..3 {
        r.event(Event::TimerFired { owner: NAME.into(), token: "tick".into() });
    }
    assert!(r.plugin.hub.state.lock().unwrap().grab.is_some());
    r.stream("phone", &room);
    assert!(r.look().plate.is_some());
    assert_eq!(r.shown("plate-text"), json!("Take the empty room again"));
    r.count_down("clone", 3);
    r.stream("phone", &with_person(&room, 20, 50));
    assert_eq!(r.look().clones.as_ref().map(|c| c.count), Some(1));
    let out = r.stream("phone", &room);
    // the clone stands there now
    assert!((out.at(35, 50)[0] as i32 - room.at(35, 50)[0] as i32).abs() > 50);
    r.ui("clear-clicked", vec![]);
    assert!(r.look().clones.is_none());
}

#[test]
fn switching_camera_calls_off_taking_the_empty_room() {
    for ticks in [1, 3] {
        let mut r = Rig::new();
        r.pick("cloak");
        r.count_down("plate", ticks);
        r.event(Event::DeviceChanged { source_id: "other".into() });
        for _ in 0..3 - ticks {
            r.event(Event::TimerFired { owner: NAME.into(), token: "tick".into() });
        }
        r.stream("phone", &room());
        assert!(r.look().plate.is_none(), "{ticks}");
        assert_eq!(r.shown("plate-text"), json!("Take the empty room"));
    }
}

#[test]
fn a_clone_without_the_empty_room_or_vision_says_what_to_do() {
    let mut r = Rig::new();
    r.pick("clones");
    r.count_down("clone", 3);
    r.stream("phone", &room());
    assert!(r.look().clones.is_none());
    assert_eq!(r.shown("problem"), json!("Take the empty room first, so you can be cut out."));
}

#[test]
fn a_frozen_step_keeps_its_settings() {
    let mut r = Rig::new();
    r.pick("game_boy");
    assert!(matches!(r.plugin.frame_step(), Some(FrameStep::Shader(_))));
    r.pick("vhs");
    let Some(FrameStep::Cpu(mut step)) = r.plugin.frame_step() else { panic!("a CPU step") };
    r.pick("off");
    assert!(r.plugin.frame_step().is_none());
    // the stream it was for goes on with the effect it was given
    let frame = rgb_big();
    let mut f = frame_of(&frame);
    step.process(&mut f, &Ctx("elsewhere"));
    assert_ne!(Rgb::from_frame(&f).unwrap(), frame);
}

fn rgb_big() -> Rgb {
    with_person(&room(), 60, 100).nearest(W * 2, H * 2)
}

#[test]
fn settings_come_back() {
    let mut r = Rig::new();
    r.plugin.set_config(&json!({"effect": "slit", "spread": 30, "across": true, "fade": 99})).unwrap();
    r.event(Event::PhonesChanged { count: 1 });
    let cfg = r.plugin.get_config();
    assert_eq!((cfg["effect"].clone(), cfg["spread"].clone(), cfg["across"].clone(), cfg["fade"].clone()), (json!("slit"), json!(30), json!(true), json!(10)));
    assert_eq!(cfg.to_string(), r#"{"effect":"slit","across":true,"edges":3,"fade":10,"brightness":6,"spread":30,"smear":0}"#, "the keys Python saves, in its order");
    r.plugin.set_config(&json!({"effect": "nonsense"})).unwrap();
    assert_eq!(r.plugin.get_config()["effect"], json!("off"));
    r.plugin.set_config(&json!({"edges": "x", "smear": 7, "spread": 1.9, "across": 1})).unwrap();
    let cfg = r.plugin.get_config();
    assert_eq!((cfg["edges"].clone(), cfg["smear"].clone(), cfg["spread"].clone(), cfg["across"].clone()), (json!(3), json!(3), json!(5), json!(false)));
}

#[test]
fn the_empty_room_is_dropped_when_the_picture_changes_under_it() {
    for change in ["source", "device", "view", "shape"] {
        let mut r = Rig::new();
        r.pick("cloak");
        let room = room();
        r.take_room(&room);
        let view = r.host.view.clone();
        r.plugin.clones = Some(Arc::new(Clones::add(None, &room, &Gray::new(W, H))));
        // the same view again: kept
        r.event(Event::ViewChanged { view: view.clone() });
        r.stream("phone", &room);
        assert!(r.plugin.plate.is_some(), "{change}");
        match change {
            "source" => r.event(Event::SourceSelected { source_id: "other".into() }),
            "device" => r.event(Event::DeviceChanged { source_id: "other".into() }),
            "view" => {
                r.host.view = View { flip_h: true, ..view };
                let v = r.host.view.clone();
                r.event(Event::ViewChanged { view: v });
            }
            _ => {
                r.stream("phone", &Rgb::new(W, H * 2));
            }
        }
        assert!(r.plugin.plate.is_none() && r.plugin.clones.is_none(), "{change}");
        assert!(r.shown("problem").as_str().unwrap().contains("again"), "{change}");
        r.take_room(&room);
        assert_eq!(r.shown("problem"), json!(""), "{change}: nothing to ask for once the room is taken again");
    }
}

#[test]
fn background_streams_get_no_stick_figure_or_empty_room() {
    let mut r = Rig::new();
    for effect in ["stick", "cloak", "clones"] {
        r.pick(effect);
        r.take_room(&room());
        let frame = with_person(&room(), 60, 100);
        assert_eq!(r.stream("elsewhere", &frame), frame, "{effect}");
    }
}

#[test]
fn another_stream_starts_without_the_last_ones_history() {
    let mut r = Rig::new();
    r.pick("slit");
    for _ in 0..30 {
        r.stream("phone", &Rgb::filled(W, H, [200; 3]));
    }
    r.event(Event::SourceSelected { source_id: "other".into() });
    // no band from the stream before
    assert!(r.stream("phone", &Rgb::filled(W, H, [10; 3])).data.iter().all(|&v| v < 50));
}

#[test]
fn stick_figure_lines_come_from_the_pose_through_the_view() {
    let person = Person::from_value(&json!({"points": {
        "left_shoulder": [0.4, 0.4, 1.0], "right_shoulder": [0.6, 0.4, 1.0], "left_hip": [0.42, 0.7, 1.0], "nose": [0.5, 0.25, 1.0],
        "left_eye": [0.48, 0.24, 1.0], "right_eye": [0.52, 0.24, 1.0], "right_hip": [0.58, 0.7, 0.1]}})).unwrap();
    let (lines, heads) = stick_figure(&[person.clone()], &|x, y| (x, y), 16.0 / 9.0);
    assert!(lines.contains(&((0.4, 0.4), (0.6, 0.4))) && lines.contains(&((0.4, 0.4), (0.42, 0.7))));
    // not sure enough about that hip
    assert!(!lines.iter().any(|l| l.0 == (0.58, 0.7) || l.1 == (0.58, 0.7)));
    assert!(heads.len() == 1 && heads[0].0 == (0.5, 0.25));
    let (flipped, _) = stick_figure(&[person], &|x, y| (1.0 - x, y), 16.0 / 9.0);
    assert!(flipped.contains(&((0.6, 0.4), (0.4, 0.4))));
}

#[test]
fn the_shortcuts_pick_the_effect_and_run_the_buttons() {
    let mut r = Rig::new();
    let run = |r: &mut Rig, id: &str, p: Value| r.plugin.run_action(&mut r.host, id, &p);
    assert_eq!(run(&mut r, "fun_pack.effect", json!({"op": "set", "option": "Clone party"})).as_deref(), Some("Fun pack effect: Clone party"));
    assert_eq!(r.plugin.effect(), "clones");
    assert_eq!(run(&mut r, "fun_pack.plate", json!({})).as_deref(), Some("Take the empty room"));
    // counting already: not again
    assert_eq!(run(&mut r, "fun_pack.clone", json!({})), None);
    // the datamosh ones only work with datamosh on
    assert_eq!(run(&mut r, "fun_pack.clean", json!({})), None);
    assert_eq!(run(&mut r, "fun_pack.smear", json!({"op": "set", "value": 2})), None);
    run(&mut r, "fun_pack.effect", json!({"op": "set", "option": "Datamosh"}));
    assert_eq!(run(&mut r, "fun_pack.smear", json!({"op": "set", "value": 2})).as_deref(), Some("Datamosh smear: 2"));
    assert_eq!(r.plugin.get_config()["smear"], json!(2));
    assert_eq!(run(&mut r, "fun_pack.clean", json!({})).as_deref(), Some("Clean up the datamosh"));
    let ids: Vec<String> = r.plugin.actions().into_iter().map(|a| a.id).collect();
    assert_eq!(ids, ["fun_pack.effect", "fun_pack.plate", "fun_pack.clone", "fun_pack.clean", "fun_pack.smear"]);
    assert_eq!(FunPackPlugin::new().run_action(&mut r.host, "fun_pack.plate", &json!({})), None);
}

#[test]
fn it_says_what_it_is() {
    let info = FunPackPlugin::new().info();
    assert_eq!((info.name.as_str(), info.category.as_str(), info.icon.as_str()), ("fun_pack", "look", "sparkle"));
    assert!(info.optional && info.in_presets && info.device_local);
    assert_eq!(FunPackPlugin::new().card().unwrap().region, "right");
    assert_eq!(FunPackPlugin::new().diagnostics(), vec![("Fun pack".to_string(), "off".to_string())]);
    assert_eq!(english_name("mosh"), "Datamosh");
}

#[test]
fn an_effect_that_is_off_or_not_a_picture_step_forgets_the_trail() {
    for between in ["off", "mosh", "game_boy"] {
        let mut r = Rig::new();
        r.pick("light");
        let dark = Rgb::filled(W, H, [10; 3]);
        let mut lit = dark.clone();
        lit.set(80, 45, [255; 3]);
        r.stream("phone", &lit);
        assert!(r.stream("phone", &dark).at(80, 45)[0] > 100, "the trail shows before");
        r.pick(between);
        r.pick("light");
        assert!(r.stream("phone", &dark) == dark, "{between}: the old trail is gone");
    }
}

#[test]
fn a_clone_is_not_cut_out_against_an_empty_room_of_another_shape() {
    let mut r = Rig::new();
    r.pick("clones");
    r.take_room(&room());
    r.count_down("clone", 3);
    // the picture turned portrait since the room was taken: stretching it would only bend it
    r.stream("phone", &with_person(&room(), 20, 50).nearest(H, W));
    assert!(r.look().clones.is_none());
}
