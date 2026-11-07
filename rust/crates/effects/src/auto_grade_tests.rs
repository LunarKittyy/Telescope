//! Auto colour grading through the Colour grading plugin: the button, the frame step looking at generated frames, the sliders going in at once, and the baked result against the maths.

use crate::auto_scene::*;
use crate::colour_grading::ColourGradingPlugin;
use crate::cpu::Rgb;
use crate::grade::Grade;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use telescope_core::colour::bake;
use telescope_core::event::Event;
use telescope_core::frame::{CpuStep, Frame, FrameContext, FrameStep, PixelFormat, Touch};
use telescope_core::plugin::Plugin;
use telescope_core::testing::FakeHost;
use telescope_core::ui::UiEvent;

const SOURCE: &str = "phone";

struct Ctx {
    source: String,
    mask: Option<Arc<Vec<u8>>>,
}

impl FrameContext for Ctx {
    fn source_id(&self) -> &str {
        &self.source
    }
    fn person_mask(&self, _frame: &Frame) -> Option<Arc<Vec<u8>>> {
        self.mask.clone()
    }
    fn colour_preview(&self, frame: &Frame) -> Frame {
        frame.clone()
    }
}

struct Rig {
    plugin: ColourGradingPlugin,
    host: FakeHost,
    clock: Arc<AtomicU64>,
    step: Box<dyn CpuStep>,
    ctx: Ctx,
    frames_sent: u32,
}

impl Rig {
    fn new(vision: &str) -> Rig {
        let mut host = FakeHost::new();
        host.streaming = true;
        host.focused = Some(SOURCE.into());
        host.vision = Some(vision.into());
        let mut plugin = ColourGradingPlugin::new();
        let clock = Arc::new(AtomicU64::new(1_000_000_000));
        let c = clock.clone();
        plugin.set_auto_clock(Arc::new(move || c.load(Ordering::SeqCst)));
        plugin.setup(&mut host);
        plugin.set_config(&json!({})).unwrap();
        let Some(FrameStep::Cpu(step)) = plugin.frame_step() else { panic!("a frame step") };
        let mut rig = Rig { plugin, host, clock, step, ctx: Ctx { source: SOURCE.into(), mask: None }, frames_sent: 0 };
        rig.event(Event::PhonesChanged { count: 1 });
        rig
    }

    fn event(&mut self, e: Event) {
        self.plugin.on_event(&mut self.host, &e);
    }

    fn ui(&mut self, callback: &str, args: Vec<Value>) {
        self.plugin.on_ui(&mut self.host, &UiEvent { ui: "card".into(), callback: callback.into(), args });
    }

    fn shown(&self, key: &str) -> Value {
        self.host.calls.iter().rev().filter_map(|c| c.strip_prefix("set_ui_props card ")).filter_map(|j| serde_json::from_str::<Value>(j).ok())
            .find_map(|v| v.get(key).cloned()).unwrap_or(Value::Null)
    }

    fn note(&self) -> String {
        self.plugin.auto_note().to_string()
    }

    fn calls(&self, what: &str) -> usize {
        self.host.calls.iter().filter(|c| c.contains(what)).count()
    }

    /// The vision service says who's in the scene.
    fn people_seen(&mut self, scene: &Scene) {
        let people: Vec<Value> = scene.people().iter().map(|p| p.to_value()).collect();
        self.event(Event::PeopleSeen { source_id: SOURCE.into(), people });
    }

    /// The scene through `camera`, `n` frames at 100 ms, with the people and mask the vision service would give; whatever the step posts is delivered.
    fn show(&mut self, scene: &Scene, camera: &dyn Fn(&[f32]) -> Rgb, n: u32) {
        self.ctx.mask = scene.person.then(|| Arc::new(scene.mask().data.clone()));
        for _ in 0..n {
            let k = self.frames_sent;
            self.frames_sent += 1;
            let now = 1_000_000_000 + k as u64 * 100_000_000;
            self.clock.store(now, Ordering::SeqCst);
            self.people_seen(scene);
            let rgb = camera(&scene.render(k + 1));
            let rgba: Vec<u8> = rgb.data.chunks(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect();
            let mut frame = Frame::new(W as u32, H as u32, PixelFormat::Rgba, rgba);
            frame.time_ns = now;
            self.step.process(&mut frame, &self.ctx);
            let posted: Vec<(String, Value)> = self.host.posted.lock().unwrap().drain(..).collect();
            for (token, data) in posted {
                self.event(Event::Posted { owner: "colour_grading".into(), token, data });
            }
        }
    }

    fn press_and_measure(&mut self, scene: &Scene, camera: &dyn Fn(&[f32]) -> Rgb) {
        self.ui("auto-clicked", vec![]);
        self.show(scene, camera, 16);
    }

    fn published(&self) -> usize {
        self.host.colour_stages.len()
    }
}

fn green() -> Box<dyn Fn(&[f32]) -> Rgb> {
    cast([0.93, 1.14, 0.93])
}

fn right() -> Box<dyn Fn(&[f32]) -> Rgb> {
    graded(Grade::default())
}

#[test]
fn the_colour_grading_plugin_reads_the_clean_picture_before_other_steps() {
    let info = ColourGradingPlugin::new().info();
    assert_eq!(info.frame_stage, telescope_core::frame::FrameStage::Reading);
}

#[test]
fn without_a_stream_the_button_says_so_and_measures_nothing() {
    let mut r = Rig::new("ready");
    r.host.streaming = false;
    r.ui("auto-clicked", vec![]);
    assert!(r.note().starts_with("Start the stream"), "{}", r.note());
    assert_eq!(r.shown("auto-busy"), json!(false));
    assert_eq!(r.calls("want_people"), 0);
    assert!(!r.host.is_scheduled("auto_hold") && !r.host.is_scheduled("auto_timeout"));
    assert_eq!(r.shown("auto-label"), json!("No stream"));
}

#[test]
fn the_step_does_nothing_and_costs_nothing_until_asked() {
    let r = Rig::new("ready");
    assert_eq!(r.step.touch(&r.ctx), Touch::Nothing);
    let mut r = r;
    r.ui("auto-clicked", vec![]);
    r.people_seen(&Scene::default());
    assert_eq!(r.step.touch(&r.ctx), Touch::Reads);
    let other = Ctx { source: "somewhere else".into(), mask: None };
    assert_eq!(r.step.touch(&other), Touch::Nothing);
}

#[test]
fn a_press_measures_a_second_of_frames_then_puts_every_slider_in_at_once() {
    let mut r = Rig::new("ready");
    r.ui("slider-moved", vec![json!(0), json!(12.0)]);
    let before = r.published();
    r.ui("auto-clicked", vec![]);
    assert_eq!(r.shown("auto-busy"), json!(true));
    assert!(r.calls("want_people Auto colour grading true") == 1 && r.host.is_scheduled("auto_hold"));
    r.show(&Scene::default(), green().as_ref(), 16);
    assert_eq!(r.shown("auto-busy"), json!(false));
    assert!(r.calls("want_people Auto colour grading false") == 1 && !r.host.is_scheduled("auto_timeout"));
    let g = r.plugin.grade();
    assert!(g.tint > 5.0, "green is countered: {:?}", g.tint);
    assert_eq!(r.published(), before + 1, "one publish for the whole answer");
    assert!(r.note().starts_with("Auto changed"), "{}", r.note());
    // the card shows the new values
    assert!(r.shown("slider-value").as_array().unwrap()[1].as_f64().unwrap() > 5.0);
}

#[test]
fn auto_leaves_vibrance_alone() {
    let mut r = Rig::new("ready");
    r.ui("slider-moved", vec![json!(5), json!(40.0)]);
    r.press_and_measure(&Scene::default(), green().as_ref());
    assert!(r.plugin.grade().tint > 5.0);
    assert_eq!(r.plugin.grade().vibrance, 40.0);
}

#[test]
fn pressing_twice_gives_the_same_sliders() {
    let mut r = Rig::new("ready");
    r.press_and_measure(&Scene::default(), green().as_ref());
    let first = r.plugin.grade().clone();
    let published = r.published();
    r.press_and_measure(&Scene::default(), green().as_ref());
    let second = r.plugin.grade();
    assert_eq!((first.temperature, first.tint), (second.temperature, second.tint));
    assert!((first.contrast - second.contrast).abs() < 0.02 && (first.saturation - second.saturation).abs() < 0.03);
    assert!(r.published() <= published + 1);
}

#[test]
fn a_picture_that_is_fine_leaves_the_sliders_the_person_set_alone() {
    let mut r = Rig::new("ready");
    r.ui("slider-moved", vec![json!(0), json!(-20.0)]);
    let published = r.published();
    r.press_and_measure(&Scene::default(), right().as_ref());
    assert!(r.note().starts_with("Nothing to change"), "{}", r.note());
    assert_eq!(r.plugin.grade().temperature, -20.0);
    assert_eq!(r.published(), published);
}

#[test]
fn without_a_person_it_says_so_and_still_does_what_it_can() {
    let mut r = Rig::new("missing");
    let scene = Scene { person: false, ..Scene::default() };
    r.ui("auto-clicked", vec![]);
    assert!(r.host.is_scheduled("auto_timeout") && !r.host.is_scheduled("auto_hold"), "no waiting for people that cannot come");
    r.show(&scene, cast([1.15, 1.0, 0.85]).as_ref(), 16);
    assert!(r.note().contains("No face was found"), "{}", r.note());
    assert!(r.plugin.grade().temperature < -3.0, "{}", r.plugin.grade().temperature);
}

#[test]
fn light_that_conflicts_leaves_white_balance_and_says_the_light_is_the_problem() {
    let mut r = Rig::new("ready");
    let scene = Scene { wall: [0.73, 0.64, 0.64], wall_right: [0.64, 0.64, 0.73], ..Scene::default() };
    r.press_and_measure(&scene, green().as_ref());
    assert!(r.note().contains("light is mixed or coloured"), "{}", r.note());
    assert_eq!((r.plugin.grade().temperature, r.plugin.grade().tint), (0.0, 0.0));
}

#[test]
fn a_press_that_never_gets_a_steady_picture_times_out_and_changes_nothing() {
    let mut r = Rig::new("ready");
    r.ui("auto-clicked", vec![]);
    r.event(Event::TimerFired { owner: "colour_grading".into(), token: "auto_timeout".into() });
    assert!(r.note().contains("steady picture"), "{}", r.note());
    assert_eq!(r.shown("auto-busy"), json!(false));
    assert!(r.plugin.grade().is_identity());
}

#[test]
fn a_stream_that_stops_ends_the_measurement() {
    let mut r = Rig::new("ready");
    r.ui("auto-clicked", vec![]);
    r.show(&Scene::default(), green().as_ref(), 3);
    r.plugin.on_stream_stop(&mut r.host);
    assert_eq!(r.shown("auto-busy"), json!(false));
    assert!(r.calls("want_people Auto colour grading false") == 1 && !r.host.is_scheduled("auto_timeout"));
    assert_eq!(r.step.touch(&r.ctx), Touch::Nothing);
    assert!(r.plugin.grade().is_identity());
}

#[test]
fn only_the_stream_being_measured_is_looked_at() {
    let mut r = Rig::new("ready");
    r.ui("auto-clicked", vec![]);
    r.ctx.source = "another phone".into();
    r.show(&Scene::default(), green().as_ref(), 16);
    assert_eq!(r.shown("auto-busy"), json!(true), "frames of another stream don't count");
}

#[test]
fn people_who_are_too_old_for_the_frame_leave_no_face() {
    let mut r = Rig::new("ready");
    r.ui("auto-clicked", vec![]);
    let scene = Scene::default();
    r.ctx.mask = Some(Arc::new(scene.mask().data.clone()));
    // the landmarks come in once and the frames only start two seconds later
    r.people_seen(&scene);
    for k in 0..16u32 {
        let now = 3_000_000_000 + k as u64 * 100_000_000;
        let rgb = green()(&scene.render(k + 1));
        let rgba: Vec<u8> = rgb.data.chunks(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect();
        let mut frame = Frame::new(W as u32, H as u32, PixelFormat::Rgba, rgba);
        frame.time_ns = now;
        r.step.process(&mut frame, &r.ctx);
        let posted: Vec<(String, Value)> = r.host.posted.lock().unwrap().drain(..).collect();
        for (token, data) in posted {
            r.event(Event::Posted { owner: "colour_grading".into(), token, data });
        }
    }
    assert!(r.note().contains("No face was found"), "{}", r.note());
}

#[test]
fn the_baked_result_matches_the_maths_on_the_cpu_path() {
    use crate::auto_fit::Basic;
    // one grade that stays per channel (exact tables) and one that doesn't (the lattice)
    let per_channel = Basic { temperature: 20.0, tint: 15.0, lift: -0.04, gamma: 0.2, gain: 1.1, contrast: 1.2, pivot: 0.5, ..Basic::default() };
    let cube = Basic { highlights: -30.0, saturation: 1.2, ..per_channel };
    for (basic, tolerance) in [(per_channel, 1), (cube, 5)] {
        let mut r = Rig::new("ready");
        let mut grade = Grade::default();
        basic.put(&mut grade);
        r.plugin.set_config(&json!({"temperature": basic.temperature, "tint": basic.tint, "masters": {"lift": basic.lift, "gamma": basic.gamma, "gain": basic.gain},
                                    "contrast": basic.contrast, "pivot": basic.pivot, "highlights": basic.highlights, "saturation": basic.saturation})).unwrap();
        r.event(Event::StreamStopped);
        let stage = r.plugin.stage().expect("a stage");
        assert_eq!(stage.table.is_some(), grade.per_channel());
        let baked = bake(&[&stage], None).expect("baked");
        let scene = Scene::default().render(3);
        let (mut worst, mut n) = (0i32, 0);
        for p in scene.chunks(3).step_by(37) {
            let bytes = [p[0], p[1], p[2]].map(|v| (v * 255.0).round() as u8);
            let (got, want) = (baked.apply(bytes), grade.eval([p[0], p[1], p[2]]).map(|v| (v * 255.0).round() as i32));
            worst = worst.max((0..3).map(|c| (got[c] as i32 - want[c]).abs()).max().unwrap());
            n += 1;
        }
        assert!(n > 100 && worst <= tolerance, "{worst} off at worst over {n} pixels");
        // and through the step the colour stage runs while the card has no room, on a whole frame
        let rgba: Vec<u8> = scene.chunks(3).flat_map(|p| [p[0], p[1], p[2]].map(|v| (v * 255.0).round() as u8).into_iter().chain([255])).collect();
        let mut frame = Frame::new(W as u32, H as u32, PixelFormat::Rgba, rgba.clone());
        telescope_core::colour_cpu::Graded::new(&baked).process(&mut frame, &Ctx { source: SOURCE.into(), mask: None });
        let off = (0..W * H).step_by(41).map(|i| {
            let want = grade.eval([rgba[i * 4], rgba[i * 4 + 1], rgba[i * 4 + 2]].map(|v| v as f32 / 255.0));
            (0..3).map(|c| (frame.data[i * 4 + c] as i32 - (want[c] * 255.0).round() as i32).abs()).max().unwrap()
        }).max().unwrap();
        assert!(off <= tolerance + 1, "the CPU step is {off} off the maths");
    }
}

#[test]
fn a_picture_that_needs_nothing_now_takes_off_what_auto_set_before() {
    let mut r = Rig::new("ready");
    r.press_and_measure(&Scene::default(), green().as_ref());
    assert!(r.plugin.grade().tint > 5.0);
    // the lighting is put right, and Auto is pressed again
    r.press_and_measure(&Scene::default(), right().as_ref());
    assert!(r.note().contains("taken off"), "{}", r.note());
    assert!(r.plugin.grade().is_identity(), "{:?}", r.plugin.grade().tint);
}

#[test]
fn conflicting_light_keeps_the_white_balance_that_was_there() {
    let mut r = Rig::new("ready");
    r.ui("slider-moved", vec![json!(0), json!(12.0)]);
    let scene = Scene { wall: [0.73, 0.64, 0.64], wall_right: [0.64, 0.64, 0.73], ..Scene::default() };
    r.press_and_measure(&scene, green().as_ref());
    assert!(r.note().contains("light is mixed or coloured"), "{}", r.note());
    assert_eq!(r.plugin.grade().temperature, 12.0);
}

#[test]
fn a_measurement_that_was_restarted_while_a_frame_was_looked_at_keeps_the_new_one() {
    struct Restarts(Ctx, Arc<crate::auto_grade::Shared>);
    impl FrameContext for Restarts {
        fn source_id(&self) -> &str {
            self.0.source_id()
        }
        fn person_mask(&self, _frame: &Frame) -> Option<Arc<Vec<u8>>> {
            // the press is cancelled and another one started on the same stream, in the middle of this frame
            self.1.stop();
            self.1.start(SOURCE, telescope_core::view::Geometry::default(), false);
            None
        }
        fn colour_preview(&self, frame: &Frame) -> Frame {
            frame.clone()
        }
    }
    let mut r = Rig::new("ready");
    r.ui("auto-clicked", vec![]);
    let shared = r.plugin.auto_shared();
    let scene = Scene::default();
    let rgb = green()(&scene.render(1));
    let rgba: Vec<u8> = rgb.data.chunks(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect();
    let mut frame = Frame::new(W as u32, H as u32, PixelFormat::Rgba, rgba);
    frame.time_ns = 1_000_000_000;
    r.step.process(&mut frame, &Restarts(Ctx { source: SOURCE.into(), mask: None }, shared.clone()));
    assert!(shared.running());
    let m = shared.take().expect("the new measurement still has its measurer");
    assert_eq!(m.frames, 0, "the old frame was not put in the new run");
}

#[test]
fn fitted_results_run_the_same_through_the_real_pipeline_and_the_cpu_step() {
    use crate::auto_fit::fit;
    use telescope_core::frame::FrameStage;
    use telescope_core::media::{Media, StagedStep};
    if telescope_media::gpu::Gpu::shared().is_none() {
        return eprintln!("no graphics card here: the pipeline check is skipped");
    }
    let dark = graded(Grade { masters: [0.0, -0.8, 0.4, 0.0], ..Grade::default() });
    let flat = graded(Grade { contrast: 0.55, masters: [0.12, 0.0, 1.0, 0.0], ..Grade::default() });
    let cameras: [Box<dyn Fn(&[f32]) -> Rgb>; 3] = [green(), dark, flat];
    for camera in cameras {
        let values = fit(&measure(&Scene::default(), camera.as_ref()), None).values;
        let mut r = Rig::new("ready");
        let mut grade = Grade::default();
        values.put(&mut grade);
        r.plugin.set_config(&json!({"temperature": values.temperature, "tint": values.tint, "masters": {"lift": values.lift, "gamma": values.gamma, "gain": values.gain},
                                    "contrast": values.contrast, "pivot": values.pivot, "highlights": values.highlights, "saturation": values.saturation})).unwrap();
        r.event(Event::StreamStopped);
        let stage = r.plugin.stage().expect("a stage");
        let baked = bake(&[&stage], None).expect("baked");
        let pixels = camera(&Scene::default().render(3));
        let rgba: Vec<u8> = pixels.data.chunks(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect();
        let frame = Frame::new(W as u32, H as u32, PixelFormat::Rgba, rgba);
        let mut cpu = frame.clone();
        telescope_core::colour_cpu::Graded::new(&baked).process(&mut cpu, &Ctx { source: SOURCE.into(), mask: None });
        let mut pipeline = telescope_media::FfmpegMedia.pipeline().expect("a frame pipeline");
        pipeline.set_steps(vec![StagedStep { owner: "colour".into(), stage: FrameStage::Colour, step: baked.frame_step() }]);
        let through = pipeline.process(frame.clone(), &Ctx { source: SOURCE.into(), mask: None });
        let (mut worst_cpu, mut worst_math) = (0i32, 0i32);
        for i in (0..W * H).step_by(53) {
            let want = grade.eval([frame.data[i * 4], frame.data[i * 4 + 1], frame.data[i * 4 + 2]].map(|v| v as f32 / 255.0));
            for c in 0..3 {
                worst_cpu = worst_cpu.max((through.data[i * 4 + c] as i32 - cpu.data[i * 4 + c] as i32).abs());
                worst_math = worst_math.max((through.data[i * 4 + c] as i32 - (want[c] * 255.0).round() as i32).abs());
            }
        }
        assert!(worst_cpu <= 3 && worst_math <= 6, "pipeline against the CPU step {worst_cpu}, against the maths {worst_math}");
    }
}

fn dark_camera() -> Box<dyn Fn(&[f32]) -> Rgb> {
    graded(Grade { masters: [0.0, -0.8, 0.4, 0.0], ..Grade::default() })
}

#[test]
fn it_asks_for_people_even_while_vision_is_off_and_waits_for_the_first_answer() {
    let mut r = Rig::new("off");
    r.ui("auto-clicked", vec![]);
    assert_eq!(r.calls("want_people Auto colour grading true"), 1);
    assert!(r.host.is_scheduled("auto_hold") && !r.host.is_scheduled("auto_timeout"));
    // frames before the first answer are not looked at
    assert_eq!(r.step.touch(&r.ctx), Touch::Nothing);
    r.show(&Scene::default(), green().as_ref(), 1);
    assert!(r.host.is_scheduled("auto_timeout") && !r.host.is_scheduled("auto_hold"));
    assert_eq!(r.step.touch(&r.ctx), Touch::Reads);
    r.show(&Scene::default(), green().as_ref(), 15);
    assert!(r.plugin.last_auto().unwrap().0.face.is_some(), "the face came in once vision answered");
    assert!(r.plugin.grade().tint > 5.0);
}

#[test]
fn vision_that_never_answers_still_grades_from_the_whole_picture() {
    let mut r = Rig::new("off");
    r.ui("auto-clicked", vec![]);
    r.event(Event::TimerFired { owner: "colour_grading".into(), token: "auto_hold".into() });
    assert!(r.host.is_scheduled("auto_timeout") && r.shown("auto-busy") == json!(true));
    let scene = Scene { person: false, ..Scene::default() };
    r.ctx.mask = None;
    for k in 0..16u64 {
        let now = 1_000_000_000 + k * 100_000_000;
        let rgb = cast([1.15, 1.0, 0.85])(&scene.render(k as u32 + 1));
        let rgba: Vec<u8> = rgb.data.chunks(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect();
        let mut frame = Frame::new(W as u32, H as u32, PixelFormat::Rgba, rgba);
        frame.time_ns = now;
        r.step.process(&mut frame, &r.ctx);
        let posted: Vec<(String, Value)> = r.host.posted.lock().unwrap().drain(..).collect();
        for (token, data) in posted {
            r.event(Event::Posted { owner: "colour_grading".into(), token, data });
        }
    }
    assert_eq!(r.shown("auto-busy"), json!(false));
    assert!(r.plugin.grade().temperature < -3.0, "{}", r.plugin.grade().temperature);
}

#[test]
fn a_dark_picture_with_nobody_in_it_is_brought_up() {
    let mut r = Rig::new("ready");
    let scene = Scene { person: false, ..Scene::default() };
    r.press_and_measure(&scene, dark_camera().as_ref());
    let (m, fit) = r.plugin.last_auto().unwrap();
    assert!(m.face.is_none() && m.tone.p50 < 0.2, "{:?}", m.tone);
    assert!(fit.targets.scene_luma.is_some() && fit.moved.exposure, "{:?}", fit.values);
    assert!(r.plugin.grade().masters[1] > 0.1 || r.plugin.grade().masters[2] > 1.05, "{:?}", r.plugin.grade().masters);
}

#[test]
fn a_covered_lens_is_not_chased() {
    let mut r = Rig::new("ready");
    let scene = Scene { person: false, ..Scene::default() };
    r.press_and_measure(&scene, &|_| Rgb::filled(W, H, [2, 2, 2]));
    let (_, fit) = r.plugin.last_auto().unwrap();
    assert!(fit.targets.scene_luma.is_none());
    assert!(r.plugin.grade().is_identity());
}

fn bright_camera() -> Box<dyn Fn(&[f32]) -> Rgb> {
    graded(Grade { masters: [0.0, 0.6, 1.5, 0.0], ..Grade::default() })
}

#[test]
fn an_overexposed_picture_is_brought_down_with_or_without_a_face() {
    for person in [false, true] {
        let mut r = Rig::new(if person { "ready" } else { "off" });
        let scene = Scene { person, ..Scene::default() };
        r.press_and_measure(&scene, bright_camera().as_ref());
        let (m, fit) = r.plugin.last_auto().unwrap();
        assert!(fit.moved.exposure, "person {person}: {:?} {:?}", m.tone, fit.values);
        assert!(r.plugin.grade().masters[1] < -0.05 || r.plugin.grade().masters[2] < 0.95, "{:?}", r.plugin.grade().masters);
    }
}

#[test]
fn no_vision_pack_starts_the_download_once_and_still_grades() {
    let mut r = Rig::new("missing");
    r.host.pack = telescope_core::vision::VisionPack { available: true, size_mb: 47, ..Default::default() };
    let scene = Scene { person: false, ..Scene::default() };
    r.press_and_measure(&scene, dark_camera().as_ref());
    assert_eq!(r.calls("download_vision"), 1);
    assert!(r.plugin.grade().masters[1] > 0.1);
}

#[test]
fn a_press_that_changes_nothing_shows_it_on_the_button_for_a_moment() {
    let mut r = Rig::new("ready");
    r.ui("auto-clicked", vec![]);
    assert_eq!(r.shown("auto-label"), json!("Looking…"));
    r.show(&Scene::default(), right().as_ref(), 16);
    assert_eq!(r.shown("auto-label"), json!("Looks fine"));
    r.event(Event::TimerFired { owner: "colour_grading".into(), token: "auto_label".into() });
    assert_eq!(r.shown("auto-label"), json!(""));
}

#[test]
fn the_pack_download_starts_even_when_asking_for_people_moves_the_state_on() {
    let mut r = Rig::new("missing");
    r.host.vision_after_want = Some("loading".into());
    r.host.pack = telescope_core::vision::VisionPack { available: true, size_mb: 47, ..Default::default() };
    r.ui("auto-clicked", vec![]);
    assert_eq!(r.calls("download_vision"), 1);
}

fn measured(r: &Rig) -> Option<(bool, String, Value)> {
    r.host.events.iter().rev().find_map(|e| match e {
        Event::PictureMeasured { ok, message, data, .. } => Some((*ok, message.clone(), data.clone())),
        _ => None,
    })
}

#[test]
fn a_measure_request_answers_with_the_picture_as_numbers_and_moves_no_slider() {
    let mut r = Rig::new("ready");
    r.ui("slider-moved", vec![json!(0), json!(12.0)]);
    let before = (r.plugin.grade().temperature, r.plugin.grade().tint, r.published());
    r.event(Event::PictureMeasureRequested { token: "m1".into() });
    assert_eq!(r.shown("auto-busy"), json!(true));
    r.show(&Scene::default(), green().as_ref(), 16);
    let (ok, _, data) = measured(&r).expect("an answer");
    assert!(ok);
    assert_eq!(data["source"], json!(SOURCE));
    assert!(data["tone"]["middle"].as_f64().unwrap() > 0.0 && data["frames"].as_u64().unwrap() >= 5, "{data}");
    assert_eq!((r.plugin.grade().temperature, r.plugin.grade().tint, r.published()), before, "Auto's answer is not applied");
    assert_eq!(r.shown("auto-busy"), json!(false));
}

#[test]
fn a_measure_request_without_a_stream_or_while_busy_is_refused_with_a_reason() {
    let mut r = Rig::new("ready");
    r.host.streaming = false;
    r.event(Event::PictureMeasureRequested { token: "m1".into() });
    let (ok, why, _) = measured(&r).unwrap();
    assert!(!ok && why.contains("No stream is running"), "{why}");
    r.host.streaming = true;
    r.ui("auto-clicked", vec![]);
    r.event(Event::PictureMeasureRequested { token: "m2".into() });
    let (ok, why, _) = measured(&r).unwrap();
    assert!(!ok && why.contains("already looking"), "{why}");
}

#[test]
fn a_measure_that_ends_with_the_stream_says_so() {
    let mut r = Rig::new("ready");
    r.event(Event::PictureMeasureRequested { token: "m1".into() });
    r.plugin.on_stream_stop(&mut r.host);
    let (ok, why, _) = measured(&r).unwrap();
    assert!(!ok && why.contains("stream ended"), "{why}");
    assert_eq!(r.shown("auto-busy"), json!(false));
}

#[test]
fn the_grade_declares_its_sliders_with_the_ranges_the_card_has() {
    let specs = ColourGradingPlugin::new().settings();
    let find = |key: &str| specs.iter().find(|s| s.key == key).unwrap_or_else(|| panic!("{key} is declared"));
    assert_eq!(specs.len(), 14);
    assert!(find("contrast").check(&json!(2.5)).is_err() && find("contrast").check(&json!(1.25)).is_ok());
    assert!(find("vibrance").check(&json!(-100)).is_ok() && find("vibrance").check(&json!(101)).is_err());
    assert!(find("masters.gain").check(&json!(1.5)).is_ok() && find("masters.lift").check(&json!(0.6)).is_err());
    // every declared key is one the plugin saves, or one it leaves out while it is at its default
    let config = ColourGradingPlugin::new().get_config();
    for spec in &specs {
        if let Some(v) = telescope_core::settings::get_path(&config, &spec.key) {
            assert_eq!(*v, spec.default, "{} starts at its declared default", spec.key);
        }
    }
}

#[test]
fn changing_settings_through_the_host_reaches_the_card_and_keeps_what_was_not_named() {
    let mut r = Rig::new("ready");
    r.ui("slider-moved", vec![json!(0), json!(12.0)]);
    let changes = json!({"contrast": 1.4, "masters.gain": 0.8});
    let config = r.plugin.get_config();
    let merged = telescope_core::settings::merge_changes(&r.plugin.settings(), &config, changes.as_object().unwrap()).unwrap();
    r.plugin.apply_preset(&mut r.host, &merged).unwrap();
    let g = r.plugin.grade();
    assert_eq!((g.contrast, g.masters[2], g.temperature), (1.4, 0.8, 12.0));
}
