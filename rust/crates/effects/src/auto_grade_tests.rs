//! Auto colour grading through the Colour grading plugin: the button, the frame step looking at generated frames, the sliders going in at once, Undo and the nudge, and the baked result against the maths.

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
        self.shown("auto-note").as_str().unwrap_or("").to_string()
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
    assert!(r.host.scheduled.is_empty());
}

#[test]
fn the_step_does_nothing_and_costs_nothing_until_asked() {
    let r = Rig::new("ready");
    assert_eq!(r.step.touch(&r.ctx), Touch::Nothing);
    let mut r = r;
    r.ui("auto-clicked", vec![]);
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
    assert!(r.calls("want_people Auto colour grading true") == 1 && r.host.is_scheduled("auto_timeout"));
    r.show(&Scene::default(), green().as_ref(), 16);
    assert_eq!(r.shown("auto-busy"), json!(false));
    assert!(r.calls("want_people Auto colour grading false") == 1 && !r.host.is_scheduled("auto_timeout"));
    let g = r.plugin.grade();
    assert!(g.tint > 5.0, "green is countered: {:?}", g.tint);
    assert_eq!(r.published(), before + 1, "one publish for the whole answer");
    assert!(r.note().starts_with("Auto changed"), "{}", r.note());
    assert_eq!(r.shown("auto-undo"), json!(true));
    assert_eq!(r.shown("auto-nudge"), json!(true));
    // the card shows the new values
    assert!(r.shown("slider-value").as_array().unwrap()[1].as_f64().unwrap() > 5.0);
}

#[test]
fn undo_puts_the_old_sliders_back_and_the_nudge_moves_brightness() {
    let mut r = Rig::new("ready");
    r.ui("slider-moved", vec![json!(0), json!(12.0)]);
    let old = (r.plugin.grade().temperature, r.plugin.grade().tint, r.plugin.grade().masters[1]);
    r.press_and_measure(&Scene::default(), green().as_ref());
    assert!(r.plugin.grade().tint != old.1);
    r.ui("auto-nudged", vec![json!(1)]);
    let brighter = r.plugin.grade().masters[1];
    r.ui("auto-nudged", vec![json!(-1)]);
    r.ui("auto-nudged", vec![json!(-1)]);
    assert!(brighter > r.plugin.grade().masters[1] && (brighter - r.plugin.grade().masters[1] - 0.16).abs() < 1e-9);
    r.ui("auto-undo-clicked", vec![]);
    let g = r.plugin.grade();
    assert_eq!((g.temperature, g.tint, g.masters[1]), old);
    assert_eq!(r.shown("auto-undo"), json!(false));
}

#[test]
fn auto_and_its_undo_leave_vibrance_alone() {
    let mut r = Rig::new("ready");
    r.ui("slider-moved", vec![json!(5), json!(40.0)]);
    r.press_and_measure(&Scene::default(), green().as_ref());
    assert!(r.plugin.grade().tint > 5.0);
    assert_eq!(r.plugin.grade().vibrance, 40.0);
    r.ui("auto-undo-clicked", vec![]);
    assert_eq!(r.plugin.grade().vibrance, 40.0);
}

#[test]
fn pressing_twice_gives_the_same_sliders_and_one_undo_back_to_the_start() {
    let mut r = Rig::new("ready");
    r.press_and_measure(&Scene::default(), green().as_ref());
    let first = r.plugin.grade().clone();
    let published = r.published();
    r.press_and_measure(&Scene::default(), green().as_ref());
    let second = r.plugin.grade();
    assert_eq!((first.temperature, first.tint), (second.temperature, second.tint));
    assert!((first.contrast - second.contrast).abs() < 0.02 && (first.saturation - second.saturation).abs() < 0.03);
    assert!(r.published() <= published + 1);
    r.ui("auto-undo-clicked", vec![]);
    assert!(r.plugin.grade().is_identity(), "one Undo goes back to before the first press");
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
    assert_eq!(r.shown("auto-undo"), json!(false));
}

#[test]
fn without_a_person_it_says_so_and_still_does_what_it_can() {
    let mut r = Rig::new("missing");
    let scene = Scene { person: false, ..Scene::default() };
    r.press_and_measure(&scene, cast([1.15, 1.0, 0.85]).as_ref());
    assert_eq!(r.calls("want_people Auto colour grading true"), 0, "never asked for people it cannot get");
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
    r.ui("auto-undo-clicked", vec![]);
    assert!(r.plugin.grade().tint > 5.0, "Undo brings the earlier answer back");
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
            self.1.start(SOURCE, telescope_core::view::Geometry::default());
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
