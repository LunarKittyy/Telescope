use super::*;
use telescope_core::frame::{FrameStage, PixelFormat};
use telescope_core::media::{Media, StagedStep};
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
    let make = r.host.packets["fun_pack"].clone().expect("a packet step");
    // the step it makes takes packets: a parameter set goes straight through
    assert_eq!(make.make().step(&[0, 0, 0, 1, 0x67, 1, 2]), vec![vec![0, 0, 0, 1, 0x67, 1, 2]]);
    // it's all in the packets
    assert!(r.plugin.frame_step().is_none());
    r.pick("vhs");
    assert_eq!(r.calls("change_packets fun_pack false"), 1);
    assert!(r.host.packets["fun_pack"].is_none());
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
    let before = r.host.calls.len();
    r.pick("mosh");
    r.ui("clean-clicked", vec![]);
    let calls: Vec<&String> = r.host.calls[before..].iter().filter(|c| c.starts_with("change_packets")).collect();
    // off and on again, which asks the phone for a keyframe
    assert_eq!(calls, ["change_packets fun_pack true", "change_packets fun_pack false", "change_packets fun_pack true"]);
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
    // the real want_people loads MediaPipe, and what that says reaches a plugin once its hook has returned
    r.event(Event::VisionStateChanged { state: "loading".into() });
    r.event(Event::StreamStopped);
    r.event(Event::StreamStopped);
    assert_eq!(r.calls("want_people fun_pack true"), 1);
}

#[test]
fn the_empty_room_and_a_clone_are_taken_from_the_next_frame() {
    let mut r = Rig::new();
    r.pick("clones");
    let room = room();
    r.ui("plate-clicked", vec![]);
    assert_eq!(r.shown("plate-text"), json!("Taking the empty room in 3…"));
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
fn off_and_datamosh_leave_frames_alone() {
    let frame = room();
    for effect in ["off", "mosh"] {
        assert_eq!(Runner::default().run(&frame, &Look { effect: effect.into(), ..Look::default() }, None), frame, "{effect}");
    }
    // Neither has a frame step, and one handed out earlier stops changing the shown stream too
    let mut r = Rig::new();
    r.pick("vhs");
    let Some(FrameStep::Cpu(mut step)) = r.plugin.frame_step() else { panic!("a CPU step") };
    for effect in ["off", "mosh"] {
        r.pick(effect);
        assert!(r.plugin.frame_step().is_none(), "{effect}");
        let mut f = frame_of(&frame);
        let before = f.data.clone();
        step.process(&mut f, &Ctx("phone"));
        assert_eq!(f.data, before, "{effect}");
    }
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
    let (lines, heads) = stick_figure(std::slice::from_ref(&person), &|x, y| (x, y), 16.0 / 9.0);
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


#[test]
fn the_card_uses_the_vision_pack_as_soon_as_it_is_downloaded() {
    use telescope_core::vision::{VisionPack, MISSING, READY};
    for download_from in ["this card", "another card"] {
        let mut r = Rig::new();
        r.host.vision = Some(MISSING.into());
        r.host.pack = VisionPack { available: true, size_mb: 47, ..VisionPack::default() };
        let status = |r: &Rig| (r.shown("vision-text").as_str().unwrap().to_string(), r.shown("vision-download"));
        // Wanting people looks for the pack and finds none
        r.pick("stick");
        assert_eq!(status(&r), ("Needs a one-time download (about 47 MB).".to_string(), json!(true)), "{download_from}");
        r.pick("cloak");
        assert_eq!(status(&r), ("Cleaner edges with a one-time download (about 47 MB).".to_string(), json!(true)), "the cloak works without it");
        if download_from == "this card" {
            r.pick("stick");
            r.ui("download-clicked", vec![]);
            assert!(r.host.calls.iter().any(|c| c == "download_vision"), "{:?}", r.host.calls);
        }
        // What the window does when the download finishes, whichever card started it
        r.host.vision = Some(READY.into());
        r.event(Event::VisionDownloadFinished { error: String::new(), switch_on: String::new() });
        r.event(Event::VisionStateChanged { state: READY.into() });
        assert_eq!(status(&r).0, "", "nothing to say once it's there");
        r.pick("cloak");
        assert_eq!(status(&r).0, "");
    }
}

#[test]
fn a_clone_or_cloak_with_nothing_taken_leaves_the_frame_where_it_is() {
    use telescope_core::frame::Touch;
    let mut r = Rig::new();
    for effect in ["clones", "cloak"] {
        r.pick(effect);
        let Some(FrameStep::Cpu(step)) = r.plugin.frame_step() else { panic!("a CPU step") };
        assert_eq!(step.touch(&Ctx("phone")), Touch::Nothing, "{effect} with nothing taken");
    }
    // Taking the empty room needs the picture, and with it taken both have something to do
    r.pick("cloak");
    r.count_down("plate", 3);
    let Some(FrameStep::Cpu(step)) = r.plugin.frame_step() else { panic!("a CPU step") };
    assert_eq!(step.touch(&Ctx("phone")), Touch::Changes, "the room is about to be taken");
    r.stream("phone", &room());
    assert!(r.plugin.plate.is_some());
    for effect in ["cloak", "clones"] {
        r.pick(effect);
        let Some(FrameStep::Cpu(step)) = r.plugin.frame_step() else { panic!("a CPU step") };
        assert_eq!(step.touch(&Ctx("phone")), Touch::Changes, "{effect} with the room taken");
    }
    // A stream behind the panels has no room or figure of its own
    for effect in ["stick", "cloak", "clones"] {
        r.pick(effect);
        let Some(FrameStep::Cpu(step)) = r.plugin.frame_step() else { panic!("a CPU step") };
        assert_eq!(step.touch(&Ctx("elsewhere")), Touch::Nothing, "{effect} elsewhere");
    }
    r.pick("vhs");
    let Some(FrameStep::Cpu(step)) = r.plugin.frame_step() else { panic!("a CPU step") };
    assert_eq!(step.touch(&Ctx("elsewhere")), Touch::Changes);
}

#[test]
fn the_effects_that_need_a_plane_work_on_an_i420_frame_without_an_rgb_picture() {
    let big = rgb_big();
    for effect in ["vhs", "ascii", "stick"] {
        let mut r = Rig::new();
        r.pick(effect);
        let Some(FrameStep::Cpu(mut step)) = r.plugin.frame_step() else { panic!("a CPU step") };
        let mut f = Frame::new(big.w as u32, big.h as u32, PixelFormat::I420, vec![0; PixelFormat::I420.size(big.w as u32, big.h as u32)]);
        big.write_to(&mut f);
        let before = f.data.clone();
        step.process(&mut f, &Ctx("phone"));
        assert_eq!((f.width as usize, f.height as usize, f.format, f.data.len()), (big.w, big.h, PixelFormat::I420, before.len()), "{effect}");
        assert_ne!(f.data, before, "{effect} changed the picture");
    }
}

// ── Game Boy: two pixels a tap, against one pixel a tap ──

const GAME_BOY_BEFORE_LOOP: &str = r#"
    var sum = vec3<f32>(0.0);
    var total = 0.0;
    for (var y = first.y; y < last.y; y++) {
        let wy = min(f32(y + 1), hi.y) - max(f32(y), lo.y);
        for (var x = first.x; x < last.x; x++) {
            let wx = min(f32(x + 1), hi.x) - max(f32(x), lo.x);
            sum += sample((vec2<f32>(f32(x), f32(y)) + 0.5) / size).rgb * (wx * wy);
            total += wx * wy;
        }
    }
"#;

/// The shader as it was written before its cell average took two pixels a read.
fn game_boy_before() -> String {
    let now = GAME_BOY;
    let (from, to) = (now.find("    var sum = vec3<f32>(0.0);").unwrap(), now.find("    let rgb = round(sum / total * 255.0);").unwrap());
    format!("{}{}{}", &now[..from], GAME_BOY_BEFORE_LOOP.trim_start_matches('\n'), &now[to..])
}

fn shaded(wgsl: &str) -> impl FnMut(Frame) -> Frame {
    let mut pipeline = telescope_media::FfmpegMedia.pipeline().expect("a frame pipeline");
    let step = FrameStep::Shader(ShaderStep { wgsl: wgsl.into(), uniforms: json!({}), textures: Vec::new() });
    pipeline.set_steps(vec![StagedStep { owner: NAME.into(), stage: FrameStage::Effects, step }]);
    move |frame| pipeline.process(frame, &Ctx("phone"))
}

fn photo_frame(w: usize, h: usize) -> Frame {
    let mut rgb = Rgb::new(w, h);
    let mut n = 99u32;
    for y in 0..h {
        for x in 0..w {
            n = n.wrapping_mul(1664525).wrapping_add(1013904223);
            let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
            let inside = ((fx - 0.5).powi(2) + (fy - 0.6).powi(2)).sqrt() < 0.22;
            let base = if inside { [215.0, 170.0, 60.0 + 80.0 * fx] } else { [90.0 + 120.0 * fy, 140.0 + 80.0 * fy, 230.0 - 60.0 * fy] };
            rgb.set(x, y, base.map(|c| (c + (n >> 29) as f32 - 3.0).clamp(0.0, 255.0) as u8));
        }
    }
    frame_of(&rgb)
}

#[test]
fn the_game_boy_with_two_pixels_a_tap_draws_the_same_four_shades() {
    if telescope_media::gpu::Gpu::shared().is_none() {
        eprintln!("no graphics card here: the shader comparison is skipped");
        return;
    }
    // 1080 / 144 is 7.5 pixels a cell, so the cells' edges fall inside pixels; 2160 / 144 is 15, an odd number of whole pixels
    for (w, h) in [(1920usize, 1080usize), (3840, 2160), (1280, 720)] {
        let frame = photo_frame(w, h);
        let (before, now) = (shaded(&game_boy_before())(frame.clone()), shaded(GAME_BOY)(frame));
        let differing = before.data.chunks(4).zip(now.data.chunks(4)).filter(|(a, b)| a != b).count() as f64 / (w * h) as f64;
        println!("game boy {w}x{h}: {:.3}% of pixels land on another shade", differing * 100.0);
        assert!(differing < 0.01, "{w}x{h}: {differing}");
        let shades: std::collections::HashSet<[u8; 4]> = now.data.chunks(4).map(|p| [p[0], p[1], p[2], p[3]]).collect();
        assert!(shades.len() <= 4, "{}", shades.len());
    }
}

#[test]
#[ignore = "timing bench, run with --release -- --ignored --nocapture"]
fn bench_game_boy() {
    if telescope_media::gpu::Gpu::shared().is_none() {
        eprintln!("no graphics card here");
        return;
    }
    for (w, h) in [(1920usize, 1080usize), (3840, 2160)] {
        let frame = photo_frame(w, h);
        for (name, wgsl) in [("before", game_boy_before()), ("after", GAME_BOY.to_string())] {
            let mut run = shaded(&wgsl);
            run(frame.clone());
            let mut times: Vec<f64> = (0..9).map(|_| {
                let input = frame.clone();
                let start = std::time::Instant::now();
                std::hint::black_box(run(input));
                start.elapsed().as_secs_f64() * 1000.0
            }).collect();
            times.sort_by(|a, b| a.partial_cmp(b).unwrap());
            println!("game boy {name} {w}x{h}: {:.2} ms through the pipeline (median of 9)", times[4]);
        }
    }
}
