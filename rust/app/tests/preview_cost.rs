// What the preview and the stream thumbnails cost the UI thread, headless: `cargo test --release -p telescope-app --test preview_cost -- --ignored --nocapture`.
// Every scenario prints its mean and best time and a hash of the pictures it handed Slint, so a change can be checked byte for byte against an older run.
mod rig;

use i_slint_backend_testing::ElementHandle;
use rig::Rig;
use slint::platform::{PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition};
use slint_interpreter::{ComponentInstance, Value as SValue};
use std::sync::Arc;
use std::time::Instant;
use telescope_core::frame::{Frame, PixelFormat};
use telescope_core::host::HostPreview;
use telescope_core::media::Media;
use telescope_shell::worker::VideoWorker;
use telescope_core::plugin::{Plugin, PluginInfo};
use telescope_core::preview::PreviewPass;
use telescope_effects::cinema_paint::CinemaLayer;
use telescope_effects::cinema_tools::Look;
use telescope_shell::plugins::preview::Preview;

struct Holder;

impl Plugin for Holder {
    fn info(&self) -> PluginInfo {
        PluginInfo { name: "holder".into(), title: "Holder".into(), ..Default::default() }
    }
}

/// An I420 picture with gradients, edges and a little noise, so zebras, peaking and the scopes all have something to find.
fn picture(w: u32, h: u32, seed: u32) -> Frame {
    let (cw, ch) = (w.div_ceil(2) as usize, h.div_ceil(2) as usize);
    let mut data = Vec::with_capacity((w * h) as usize + 2 * cw * ch);
    let mut noise = seed.wrapping_mul(2654435761).max(1);
    for y in 0..h {
        for x in 0..w {
            noise ^= noise << 13;
            noise ^= noise >> 17;
            noise ^= noise << 5;
            let base = (x * 255 / w + y * 90 / h) as u8;
            let edge = if (x / 64 + y / 64) % 5 == 0 { 40 } else { 0 };
            data.push(16u8.saturating_add(base.saturating_add(edge)).saturating_add((noise & 7) as u8).min(235));
        }
    }
    for i in 0..2 * cw * ch {
        data.push(if i < cw * ch { (90 + (i % cw) * 70 / cw) as u8 } else { (150 - (i % cw) * 60 / cw) as u8 });
    }
    Frame::new(w, h, PixelFormat::I420, data)
}

fn fnv(hash: &mut u64, bytes: impl IntoIterator<Item = u8>) {
    for b in bytes {
        *hash ^= b as u64;
        *hash = hash.wrapping_mul(0x100000001b3);
    }
}

/// A hash of an image's colour, however Slint holds it (alpha counts when there is some).
fn image_hash(hash: &mut u64, image: &slint::Image) {
    if let Some(rgb) = image.to_rgb8() {
        fnv(hash, rgb.as_slice().iter().flat_map(|p| [p.r, p.g, p.b]));
    } else if let Some(rgba) = image.to_rgba8() {
        fnv(hash, rgba.as_slice().iter().flat_map(|p| [p.r, p.g, p.b, p.a]));
    }
}

struct Stage {
    r: Rig,
}

impl Stage {
    fn on_air(name: &str) -> Stage {
        let r = Rig::with(name, |_, _| vec![Box::new(Holder) as Box<dyn Plugin>, Box::new(Preview::default())]);
        r.w.ui.window().set_size(slint::LogicalSize::new(1200.0, 800.0));
        r.w.ui.show().unwrap();
        r.start();
        r.worker(0).says("ok", "Streaming");
        r.pump();
        let s = Stage { r };
        assert!(s.r.w.call_ui("preview", "card", "smooth-clicked"));
        s.r.pump();
        s
    }

    fn prefix(&self) -> String {
        let id = self.r.w.ui.definition().properties().map(|p| p.0).find(|n| n.ends_with("-has-frame")).expect("the preview card");
        id.trim_end_matches("has-frame").to_string()
    }

    fn click(&self, position: LogicalPosition) {
        let w = self.r.w.ui.window();
        w.dispatch_event(WindowEvent::PointerMoved { position });
        w.dispatch_event(WindowEvent::PointerPressed { position, button: PointerEventButton::Left });
        w.dispatch_event(WindowEvent::PointerReleased { position, button: PointerEventButton::Left });
        self.r.w.process();
    }

    fn pop_out(&self, w: u32, h: u32) -> ComponentInstance {
        let e = ElementHandle::find_by_accessible_label(&self.r.w.ui, "Pop out").next().expect("the Pop out button");
        let (p, size) = (e.absolute_position(), e.size());
        self.click(LogicalPosition::new(p.x + size.width / 2.0, p.y + size.height / 2.0));
        self.r.w.paint_preview();
        let popout = self.r.w.popout_window().expect("the pop-out");
        popout.window().set_size(slint::PhysicalSize::new(w, h));
        popout
    }

    fn add_pass(&self, pass: Arc<dyn PreviewPass>) {
        let i = self.r.w.state.borrow().index("holder").expect("the plugin");
        self.r.w.state.borrow_mut().call(i, |_, host| host.add_preview_pass("cinema", pass.clone()));
        self.r.w.process();
        self.r.pump();
    }

    /// The pictures the window is showing now, hashed (the stage's and the overlay's, from the card or the pop-out).
    fn shown(&self, popout: &Option<ComponentInstance>) -> u64 {
        let mut hash = 0xcbf29ce484222325;
        let (target, prefix) = match popout {
            Some(p) => (p, String::new()),
            None => (&self.r.w.ui, self.prefix()),
        };
        for name in ["frame", "overlay"] {
            if let Ok(SValue::Image(image)) = target.get_property(&format!("{prefix}{name}")) {
                image_hash(&mut hash, &image);
            }
        }
        hash
    }
}

struct Timing {
    mean: f64,
    best: f64,
}

fn time(runs: usize, mut f: impl FnMut(usize)) -> Timing {
    let mut times = Vec::new();
    for i in 0..runs {
        let t = Instant::now();
        f(i);
        times.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    Timing { mean: times.iter().sum::<f64>() / runs as f64, best: times.iter().cloned().fold(f64::MAX, f64::min) }
}

fn look(f: impl FnOnce(&mut Look)) -> Look {
    let mut l = Look::default();
    f(&mut l);
    l
}

/// Where the picture goes: the card, or a pop-out window of this size.
#[derive(Clone, Copy)]
enum Where {
    Card,
    Popout(u32, u32),
}

struct Case {
    name: &'static str,
    look: Option<Look>,
    split: bool,
}

fn paint_cost(frame: (u32, u32), at: Where, case: &Case) {
    let s = Stage::on_air("preview-cost");
    let layer = Arc::new(CinemaLayer::default());
    if let Some(look) = &case.look {
        layer.set_look(look.clone());
        s.add_pass(layer.clone());
    }
    let popout = match at {
        Where::Card => None,
        Where::Popout(w, h) => Some(s.pop_out(w, h)),
    };
    let frames: Vec<Frame> = (0..4).map(|i| picture(frame.0, frame.1, i + 1)).collect();
    if case.split {
        let before = picture(frame.0, frame.1, 99);
        s.r.w.state.borrow().colour_before.lock().unwrap().insert("pixel".into(), before);
        s.r.w.state.borrow_mut().colour.set_split("grade", Some(0.5));
    }
    let slot = s.r.worker(0).slot();
    let mut paint = |i: usize| {
        slot.publish(frames[i % frames.len()].clone());
        s.r.w.paint_preview();
    };
    for i in 0..3 {
        paint(i);
    }
    // Scopes work out their picture on a thread of their own: let one finish so the overlay has it
    std::thread::sleep(std::time::Duration::from_millis(300));
    let t = time(24, &mut paint);
    // One fixed picture to hash, so a run can be compared with another: each look is left to finish before the next paint
    std::thread::sleep(std::time::Duration::from_millis(500));
    slot.publish(frames[0].clone());
    s.r.w.paint_preview();
    std::thread::sleep(std::time::Duration::from_millis(500));
    slot.publish(frames[0].clone());
    s.r.w.paint_preview();
    let (label, size) = match at {
        Where::Card => ("card".to_string(), String::new()),
        Where::Popout(w, _) => (format!("popout{w}"), String::new()),
    };
    println!("PAINT {}x{} {label:<10}{size} {:<22} mean {:7.2} ms  best {:7.2} ms  hash {:016x}", frame.0, frame.1, case.name, t.mean, t.best, s.shown(&popout));
}

fn cases() -> Vec<Case> {
    vec![
        Case { name: "plain", look: None, split: false },
        Case { name: "plain+split", look: None, split: true },
        Case { name: "zebras", look: Some(look(|l| l.zebras = true)), split: false },
        Case { name: "peaking", look: Some(look(|l| l.peaking = true)), split: false },
        Case { name: "false-colour", look: Some(look(|l| l.false_colour = true)), split: false },
        Case { name: "waveform", look: Some(look(|l| l.scope = "waveform")), split: false },
        Case { name: "guides", look: Some(look(|l| l.guides = "thirds")), split: false },
        Case { name: "zebras+scope+guides", look: Some(look(|l| { l.zebras = true; l.scope = "parade"; l.guides = "thirds"; })), split: false },
    ]
}

#[test]
#[ignore]
fn what_painting_the_preview_costs() {
    for (frame, at) in [
        ((1920, 1080), Where::Card),
        ((1920, 1080), Where::Popout(1920, 1080)),
        ((3840, 2160), Where::Card),
        ((3840, 2160), Where::Popout(3840, 2160)),
    ] {
        for case in cases() {
            paint_cost(frame, at, &case);
        }
    }
}

#[test]
#[ignore]
fn what_the_thumbnails_cost() {
    for (frame, streams) in [((1920, 1080), 2usize), ((1920, 1080), 4), ((3840, 2160), 2), ((3840, 2160), 4)] {
        let r = Rig::new("thumb-cost");
        for (id, name) in [("c", "C"), ("d", "D")] {
            r.link.pair(id, name);
        }
        r.w.ui.window().set_size(slint::LogicalSize::new(1200.0, 800.0));
        r.w.ui.show().unwrap();
        r.start();
        r.worker(0).says("ok", "Streaming");
        r.pump();
        let ids = ["oneplus", "c", "d"];
        for (n, id) in ids.iter().enumerate().take(streams - 1) {
            r.add(id);
            r.worker(n + 1).says("ok", "Streaming");
        }
        r.pump();
        let pictures: Vec<Frame> = (0..streams as u32).map(|i| picture(frame.0, frame.1, i + 1)).collect();
        let publish = || (0..streams).for_each(|n| {
            r.worker(n).slot().publish(pictures[n].clone());
        });
        publish();
        r.w.update_thumbnails();
        // What the UI thread spends on a tick of the preview's timer; the frames in between are the stream's
        let mut spent = Vec::new();
        for _ in 0..20 {
            std::thread::sleep(std::time::Duration::from_millis(40));
            r.pump();
            publish();
            let t = Instant::now();
            r.w.update_thumbnails();
            spent.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        let t = Timing { mean: spent.iter().sum::<f64>() / spent.len() as f64, best: spent.iter().cloned().fold(f64::MAX, f64::min) };
        std::thread::sleep(std::time::Duration::from_millis(300));
        r.pump();
        let mut hash = 0xcbf29ce484222325u64;
        let tiles = match r.w.ui.get_property("tiles") {
            Ok(SValue::Model(m)) => m,
            other => panic!("tiles are {other:?}"),
        };
        use slint::Model;
        for row in 0..tiles.row_count() {
            if let Some(SValue::Struct(tile)) = tiles.row_data(row) {
                if let Some(SValue::Image(image)) = tile.get_field("thumb") {
                    image_hash(&mut hash, image);
                }
            }
        }
        println!("THUMBS {}x{} streams {streams}  mean {:7.2} ms  best {:7.2} ms  hash {hash:016x}", frame.0, frame.1, t.mean, t.best);
    }
}

type Passes = Vec<(String, Arc<dyn PreviewPass>)>;

fn passes_of(layer: &Arc<CinemaLayer>) -> Passes {
    vec![("cinema".to_string(), layer.clone() as Arc<dyn PreviewPass>)]
}

/// The pieces of one paint, each alone, at the size the picture is shown at.
#[test]
#[ignore]
fn what_the_pieces_of_a_paint_cost() {
    use telescope_shell::preview_passes::PassWork;
    for (w, h) in [(1920u32, 1080u32), (3840, 2160)] {
        let frame = picture(w, h, 1);
        let other = picture(w, h, 2);
        let t = time(10, |_| {
            std::hint::black_box(telescope_core::colour::with_split(&other, &frame, 0.5));
        });
        println!("PIECE {w}x{h} with_split I420                  mean {:7.2} ms  best {:7.2} ms", t.mean, t.best);
        let media = telescope_media::FfmpegMedia;
        let mut work = PassWork::default();
        let t = time(10, |_| {
            std::hint::black_box(work.copy(|| Some(media.scaler()), &frame, w, h));
        });
        println!("PIECE {w}x{h} copy (scale to RGBA)             mean {:7.2} ms  best {:7.2} ms", t.mean, t.best);
        let copy = work.copy(|| Some(media.scaler()), &frame, w, h).unwrap();
        let mut hash = 0xcbf29ce484222325u64;
        fnv(&mut hash, copy.data.iter().copied());
        println!("PIECE {w}x{h} copy hash {hash:016x}");
        for (name, look) in [("zebras", look(|l| l.zebras = true)), ("peaking", look(|l| l.peaking = true)), ("false-colour", look(|l| l.false_colour = true))] {
            let layer = Arc::new(CinemaLayer::default());
            layer.set_look(look);
            let all = passes_of(&layer);
            let t = time(10, |_| {
                let mut shown = copy.clone();
                PassWork::tint(&all, &mut shown);
                std::hint::black_box(shown);
            });
            println!("PIECE {w}x{h} tint {name:<13}               mean {:7.2} ms  best {:7.2} ms", t.mean, t.best);
        }
        {
            use telescope_effects::cpu::Rgb;
            let layer = CinemaLayer::default();
            layer.set_look(look(|l| l.zebras = true));
            let t = time(10, |_| {
                std::hint::black_box(Rgb::from_frame(&copy));
            });
            println!("PIECE {w}x{h}   zebras: Rgb::from_frame        mean {:7.2} ms  best {:7.2} ms", t.mean, t.best);
            let mut rgb = Rgb::from_frame(&copy).unwrap();
            let t = time(10, |_| layer.tint_rgb(&mut rgb));
            println!("PIECE {w}x{h}   zebras: the tint itself        mean {:7.2} ms  best {:7.2} ms", t.mean, t.best);
            let mut target = Frame::new(w, h, PixelFormat::Rgba, vec![0; (w * h * 4) as usize]);
            let t = time(10, |_| rgb.write_to(&mut target));
            println!("PIECE {w}x{h}   zebras: Rgb::write_to          mean {:7.2} ms  best {:7.2} ms", t.mean, t.best);
        }
        let t = time(10, |_| {
            std::hint::black_box(telescope_shell::preview::rgb_image(&copy.data, w, h));
        });
        println!("PIECE {w}x{h} RGBA to the Slint RGB buffer     mean {:7.2} ms  best {:7.2} ms", t.mean, t.best);
        for (name, look) in [
            ("guides", look(|l| l.guides = "thirds")),
            ("false-colour legend", look(|l| l.false_colour = true)),
            ("waveform", look(|l| l.scope = "waveform")),
            ("parade", look(|l| l.scope = "parade")),
            ("vectorscope", look(|l| l.scope = "vectorscope")),
            ("histogram", look(|l| l.scope = "histogram")),
            ("guides+waveform", look(|l| { l.guides = "thirds"; l.scope = "waveform"; })),
        ] {
            let layer = Arc::new(CinemaLayer::default());
            layer.set_look(look);
            layer.analyse(&copy);
            let all = passes_of(&layer);
            let mut work = PassWork::default();
            let t = time(6, |_| {
                std::hint::black_box(work.overlay(&all, w, h, 1.0));
            });
            let mut hash = 0xcbf29ce484222325u64;
            if let Some(image) = work.overlay(&all, w, h, 1.0) {
                image_hash(&mut hash, &image);
            }
            println!("PIECE {w}x{h} overlay {name:<19}        mean {:7.2} ms  best {:7.2} ms  hash {hash:016x}", t.mean, t.best);
        }
        let layer = Arc::new(CinemaLayer::default());
        layer.set_look(look(|l| l.scope = "parade"));
        let t = time(6, |_| {
            layer.analyse(&copy);
        });
        println!("PIECE {w}x{h} analyse parade (own thread)      mean {:7.2} ms  best {:7.2} ms", t.mean, t.best);
    }
}

/// What the window spends when a frame wakes it and the card has nothing to paint (a frame the cadence skips, or one already shown), and on a tick of its 10 Hz timer.
#[test]
#[ignore]
fn what_a_wake_and_a_tick_cost() {
    for frame in [(1920, 1080), (3840, 2160)] {
        let s = Stage::on_air("wake-cost");
        let slot = s.r.worker(0).slot();
        let pictures: Vec<Frame> = (0..4).map(|i| picture(frame.0, frame.1, i + 1)).collect();
        slot.publish(pictures[0].clone());
        s.r.w.paint_preview();
        let same = time(2000, |_| s.r.w.frame_ready());
        let mut each = Vec::new();
        for i in 0..200 {
            slot.publish(pictures[i % 4].clone());
            let t = Instant::now();
            s.r.w.frame_ready();
            each.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        let mean = |odd: usize| each.iter().skip(odd).step_by(2).sum::<f64>() / (each.len() / 2) as f64;
        let tick = time(2000, |_| {
            s.r.w.paint_preview();
            s.r.w.paint_layers();
            s.r.w.watch_focus();
            s.r.w.update_thumbnails();
        });
        println!("WAKE {}x{} same frame {:.3} ms, new frames: even {:.3} ms odd {:.3} ms, 10 Hz tick {:.3} ms", frame.0, frame.1, same.mean, mean(0), mean(1), tick.mean);
    }
}
