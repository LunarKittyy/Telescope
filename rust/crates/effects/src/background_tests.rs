//! test_background.py, by name, and the pictures Python recorded (recorded from the Python app).

use super::*;
use crate::imgops::blur_f32;
use ring::digest::{digest, SHA256};
use telescope_core::frame::PixelFormat;
use telescope_core::testing::FakeHost;
use telescope_core::vision::{self, VisionPack};

pub(crate) struct Ctx(pub Option<Gray>, pub std::cell::Cell<u32>);

impl FrameContext for Ctx {
    fn source_id(&self) -> &str {
        "phone"
    }
    fn person_mask(&self, frame: &Frame) -> Option<Arc<Vec<u8>>> {
        self.1.set(self.1.get() + 1);
        let m = self.0.as_ref()?;
        // cut to the frame, as the Python test's host does
        let (w, h) = (frame.width as usize, frame.height as usize);
        Some(Arc::new((0..h).flat_map(|y| m.data[y * m.w..y * m.w + w].to_vec()).collect()))
    }
    fn colour_preview(&self, frame: &Frame) -> Frame {
        frame.clone()
    }
}

fn frame_of(rgb: &Rgb) -> Frame {
    let data = rgb.data.as_chunks::<3>().0.iter().flat_map(|p| [p[0], p[1], p[2], 255]).collect();
    Frame::new(rgb.w as u32, rgb.h as u32, PixelFormat::Rgba, data)
}

fn half_mask(w: usize, h: usize) -> Gray {
    // the person on the left half
    Gray { w, h, data: (0..w * h).map(|i| if i % w < w / 2 { 255 } else { 0 }).collect() }
}

fn stripes(w: usize, h: usize) -> Rgb {
    let mut f = Rgb::new(w, h);
    for y in (0..h).step_by(2) {
        f.data[y * w * 3..(y + 1) * w * 3].fill(255);
    }
    f
}

/// One colour as a picture file would give it, by path.
fn reader_of(rgb: [u8; 3]) -> PictureReader {
    Arc::new(move |path: &str| {
        if !path.ends_with("beach.png") {
            return Err("no such file".to_string());
        }
        Ok(Frame::new(20, 10, PixelFormat::Rgba, [rgb[0], rgb[1], rgb[2], 255].repeat(200)))
    })
}

struct Rig {
    plugin: BackgroundPlugin,
    host: FakeHost,
    ctx: Ctx,
}

impl Rig {
    fn new(mask: Option<Gray>, state: &str) -> Rig {
        Rig::with(BackgroundPlugin::new(), mask, state)
    }

    fn with(plugin: BackgroundPlugin, mask: Option<Gray>, state: &str) -> Rig {
        let mut rig = Rig { plugin, host: FakeHost::new(), ctx: Ctx(mask, Default::default()) };
        rig.host.vision = Some(state.into());
        rig.plugin.setup(&mut rig.host);
        rig.event(Event::PhonesChanged { count: 1 });
        rig.plugin.set_config(&json!({})).unwrap();
        rig
    }

    fn event(&mut self, e: Event) {
        self.plugin.on_event(&mut self.host, &e);
    }

    fn ui(&mut self, callback: &str, args: Vec<Value>) {
        self.plugin.on_ui(&mut self.host, &UiEvent { ui: "card".into(), callback: callback.into(), args });
    }

    fn set_mode(&mut self, mode: &str) {
        let i = MODES.iter().position(|(k, _)| *k == mode).unwrap();
        self.ui("mode-picked", vec![json!(i)]);
    }

    /// The newest value the card was given for a property.
    fn shown(&self, key: &str) -> Value {
        self.host.calls.iter().rev().filter_map(|c| c.strip_prefix("set_ui_props card ")).filter_map(|j| serde_json::from_str::<Value>(j).ok())
            .find_map(|v| v.get(key).cloned()).unwrap_or(Value::Null)
    }

    /// process_frame: the frame after the step as it is now.
    fn process(&mut self, rgb: &Rgb) -> Rgb {
        let mut frame = frame_of(rgb);
        if let Some(FrameStep::Cpu(mut step)) = self.plugin.frame_step() {
            step.process(&mut frame, &self.ctx);
        }
        Rgb::from_frame(&frame).unwrap()
    }
}

fn column(rgb: &Rgb, x0: usize, x1: usize) -> Vec<u8> {
    (0..rgb.h).flat_map(|y| rgb.data[(y * rgb.w + x0) * 3..(y * rgb.w + x1) * 3].to_vec()).collect()
}

#[test]
fn the_person_stays_and_the_rest_is_the_background() {
    let frame = Rgb::filled(64, 36, [200, 200, 200]);
    let background = Rgb::new(64, 36);
    let out = compose(&frame, &half_mask(64, 36), &background);
    assert!(column(&out, 0, 32).iter().all(|v| *v == 200) && column(&out, 32, 64).iter().all(|v| *v == 0));
    let halfway = compose(&frame, &Gray::filled(64, 36, 128), &background);
    assert!((halfway.at(0, 0)[0] as i32 - 100).abs() <= 1);
}

#[test]
fn a_picture_covers_the_frame_and_is_cut_evenly() {
    // only the middle 100 rows are black
    let mut tall = Rgb::new(100, 400);
    tall.data[..150 * 100 * 3].fill(255);
    let covered = cover(&tall, 160, 90);
    assert_eq!((covered.w, covered.h), (160, 90));
    assert!((covered.data.iter().map(|v| *v as f64).sum::<f64>() / covered.data.len() as f64) < 255.0 * 0.5);
}

#[test]
fn a_picture_with_a_non_ascii_path_opens() {
    let dir = telescope_core::testing::scratch_dir("background");
    let path = dir.join("Bakgrund åäö.png");
    std::fs::write(&path, b"a picture").unwrap();
    // Paths reach the reader as text, and shared::picture_reader reads them as bytes
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = seen.clone();
    let reader: PictureReader = Arc::new(move |p: &str| {
        log.lock().unwrap().push(std::fs::read(p).ok());
        if p.ends_with(".png") && std::fs::metadata(p).is_ok() { Ok(Frame::new(20, 10, PixelFormat::Rgba, vec![7; 800])) } else { Err("no".to_string()) }
    });
    let mut rig = Rig::with(BackgroundPlugin::with_reader(reader), None, vision::READY);
    rig.plugin.load_picture(path.to_str().unwrap());
    assert_eq!(rig.plugin.picture.as_ref().map(|p| (p.w, p.h)), Some((20, 10)));
    assert_eq!(seen.lock().unwrap()[0].as_deref(), Some(&b"a picture"[..]));
    rig.plugin.load_picture(dir.join("missing.png").to_str().unwrap());
    assert!(rig.plugin.picture.is_none());
    // A file that isn't a picture is the reader's answer too
    let broken: PictureReader = Arc::new(|_| Err("not a picture".to_string()));
    let mut rig = Rig::with(BackgroundPlugin::with_reader(broken), None, vision::READY);
    rig.plugin.load_picture("broken.png");
    assert!(rig.plugin.picture.is_none());
}

#[test]
fn softer_edges_feather_the_mask_and_zero_leaves_it() {
    let mask = half_mask(256, 144);
    assert_eq!(mask.soften(0), mask);
    let soft = mask.soften(8);
    assert!(soft.data[128] > 0 && soft.data[128] < 255 && soft.data[0] == 255 && soft.data[255] == 0);
}

#[test]
fn off_leaves_frames_alone_without_asking_for_a_mask() {
    let mut rig = Rig::new(Some(half_mask(64, 36)), vision::READY);
    let frame = Rgb::filled(64, 36, [200, 200, 200]);
    assert_eq!(rig.process(&frame), frame);
    assert_eq!(rig.ctx.1.get(), 0);
    assert!(rig.plugin.frame_step().is_none());
}

#[test]
fn blur_blurs_only_behind_the_person() {
    let mut rig = Rig::new(Some(half_mask(320, 180)), vision::READY);
    rig.set_mode("blur");
    rig.ui("edges-moved", vec![json!(0)]);
    // stripes: a blur turns them grey
    let frame = stripes(320, 180);
    let out = rig.process(&frame);
    assert_eq!(column(&out, 0, 150), column(&frame, 0, 150));
    let behind: Vec<f64> = column(&out, 200, 320).iter().map(|v| *v as f64).collect();
    let mean = behind.iter().sum::<f64>() / behind.len() as f64;
    let std = (behind.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / behind.len() as f64).sqrt();
    assert!(mean > 60.0 && mean < 200.0 && std < 40.0, "{mean} {std}");
    let plain = blurred(&frame, 6, None);
    assert_eq!((plain.w, plain.h), (frame.w, frame.h));
}

#[test]
fn without_a_mask_yet_the_frame_goes_out_as_it_is() {
    let mut rig = Rig::new(None, vision::LOADING);
    rig.set_mode("blur");
    let frame = Rgb::filled(64, 36, [200, 200, 200]);
    assert_eq!(rig.process(&frame), frame);
    assert_eq!(rig.shown("vision-text"), json!("Getting ready…"));
}

#[test]
fn a_mask_that_goes_away_leaves_the_room_hidden() {
    let mut rig = Rig::new(Some(half_mask(320, 180)), vision::READY);
    rig.set_mode("blur");
    let frame = stripes(320, 180);
    rig.process(&frame);
    rig.ctx.0 = None;
    assert_eq!(rig.process(&frame), blurred(&frame, 6, None), "a new step knows too, and the person goes with it");
    let mut rig = Rig::with(BackgroundPlugin::with_reader(reader_of([0, 0, 255])), Some(half_mask(64, 36)), vision::READY);
    rig.plugin.load_picture("/pics/beach.png");
    rig.set_mode("picture");
    let frame = Rgb::filled(64, 36, [200, 200, 200]);
    rig.process(&frame);
    rig.ctx.0 = None;
    assert!(rig.process(&frame).data.as_chunks::<3>().0.iter().all(|p| *p == [0, 0, 255]));
}

#[test]
fn a_frozen_step_keeps_its_settings() {
    let mut rig = Rig::new(Some(Gray::new(64, 36)), vision::READY);
    rig.set_mode("blur");
    let Some(FrameStep::Cpu(mut step)) = rig.plugin.frame_step() else { panic!("a CPU step") };
    rig.set_mode("off");
    let source = stripes(64, 36);
    let mut frame = frame_of(&source);
    step.process(&mut frame, &rig.ctx);
    assert_ne!(Rgb::from_frame(&frame).unwrap(), source);
}

#[test]
fn picture_mode_puts_the_picture_behind_and_survives_a_bad_file() {
    let mut rig = Rig::with(BackgroundPlugin::with_reader(reader_of([0, 0, 255])), Some(half_mask(64, 36)), vision::READY);
    rig.plugin.load_picture("/pics/beach.png");
    rig.set_mode("picture");
    rig.ui("edges-moved", vec![json!(0)]);
    let out = rig.process(&Rgb::filled(64, 36, [200, 200, 200]));
    assert!(column(&out, 40, 64).as_chunks::<3>().0.iter().all(|p| *p == [0, 0, 255]) && column(&out, 0, 30).iter().all(|v| *v == 200));
    assert_eq!(rig.plugin.get_config()["picture"], json!("/pics/beach.png"));
    rig.plugin.set_config(&json!({"mode": "picture", "picture": "/pics/gone.png"})).unwrap();
    let frame = Rgb::filled(64, 36, [200, 200, 200]);
    assert_eq!(rig.process(&frame), frame);
    rig.event(Event::StreamStopped);
    assert_eq!(rig.shown("vision-text"), json!("Can't open"));
}

#[test]
fn settings_round_trip_and_damaged_ones_fall_back() {
    let mut rig = Rig::new(None, vision::READY);
    rig.plugin.set_config(&json!({"mode": "blur", "blur": 9, "edges": 0})).unwrap();
    assert_eq!(rig.plugin.get_config(), json!({"mode": "blur", "blur": 9, "edges": 0, "picture": ""}));
    rig.event(Event::StreamStopped);
    assert_eq!((rig.shown("mode"), rig.shown("blur")), (json!(1), json!(9)));
    rig.plugin.set_config(&json!({"mode": "sparkles", "blur": "lots", "edges": 99, "picture": 3})).unwrap();
    assert_eq!(rig.plugin.get_config(), json!({"mode": "off", "blur": 6, "edges": 10, "picture": ""}));
}

#[test]
fn without_the_download_the_card_offers_it() {
    let mut rig = Rig::new(None, vision::MISSING);
    rig.host.pack = VisionPack { available: true, size_mb: 47, ..VisionPack::default() };
    rig.set_mode("blur");
    assert!(rig.shown("vision-text").as_str().unwrap().starts_with("Needs a one-time download (about "));
    assert_eq!(rig.shown("vision-download"), json!(true));
    rig.ui("download-clicked", vec![]);
    assert!(rig.host.calls.iter().any(|c| c == "download_vision"));
    rig.host.pack.downloading = true;
    rig.host.pack.size = 100;
    rig.host.pack.done = 50;
    rig.event(Event::VisionDownloadProgress { done: 50, size: 100 });
    assert_eq!(rig.shown("vision-text"), json!("Downloading… 50%"));
}

#[test]
fn the_person_doesnt_smear_into_a_halo_around_them() {
    // a dark person in the middle of a bright room
    let mut frame = Rgb::filled(320, 180, [240, 240, 240]);
    let mut mask = Gray::new(320, 180);
    for y in 0..180 {
        for x in 100..220 {
            frame.set(x, y, [10, 10, 10]);
            mask.data[y * 320 + x] = 255;
        }
    }
    let (plain, without) = (blurred(&frame, 8, None), blurred(&frame, 8, Some(&mask)));
    // just outside: the plain blur pulls the person in
    assert!(plain.at(90, 90)[0] < 200);
    assert!(without.at(90, 90)[0] > 225);
}

// ── Beyond Python's tests ─────────────────────────────────────────────

#[test]
fn the_card_and_keys_match_the_python_actions() {
    let mut rig = Rig::new(Some(half_mask(64, 36)), vision::READY);
    let ids: Vec<String> = rig.plugin.actions().iter().map(|a| a.id.clone()).collect();
    assert_eq!(ids, ["background.mode", "background.blur"]);
    // no Blur row, so its key has nothing to do
    assert!(rig.plugin.run_action(&mut rig.host, "background.blur", &json!({"op": "up", "amount": 1})).is_none());
    assert_eq!(rig.plugin.run_action(&mut rig.host, "background.mode", &json!({"value": "blur"})).unwrap(), "Background: Blur");
    assert_eq!(rig.plugin.get_config()["mode"], json!("blur"));
    assert!(rig.plugin.run_action(&mut rig.host, "background.blur", &json!({"op": "set", "value": 8})).is_some());
    assert_eq!(rig.plugin.get_config()["blur"], json!(8));
    assert_eq!(rig.plugin.diagnostics(), vec![("Background".to_string(), "blur".to_string())]);
    // dimmed until a phone is paired
    rig.event(Event::PhonesChanged { count: 0 });
    assert!(rig.plugin.run_action(&mut rig.host, "background.mode", &json!({"value": "off"})).is_none());
}

#[test]
fn choosing_a_picture_opens_at_home_and_a_bad_one_keeps_the_mode() {
    let mut rig = Rig::with(BackgroundPlugin::with_reader(reader_of([1, 2, 3])), None, vision::READY);
    rig.ui("picture-clicked", vec![]);
    assert!(rig.host.calls.iter().any(|c| c == "pick_file Open Choose a picture"));
    let request = rig.plugin.asked.unwrap();
    rig.event(Event::RequestDone { request, ok: true, message: String::new(), data: json!({"path": "/pics/nope.png"}) });
    assert_eq!(rig.plugin.get_config()["mode"], json!("off"));
    rig.ui("picture-clicked", vec![]);
    let request = rig.plugin.asked.unwrap();
    rig.event(Event::RequestDone { request, ok: true, message: String::new(), data: json!({"path": "/pics/beach.png"}) });
    assert_eq!(rig.plugin.get_config()["mode"], json!("picture"));
    assert_eq!(rig.shown("picture-text"), json!("beach.png"));
}

/// The test pattern both sides make: every channel different, with fine and coarse detail.
pub(crate) fn pattern(w: usize, h: usize) -> Rgb {
    let mut data = Vec::with_capacity(w * h * 3);
    for y in 0..h {
        for x in 0..w {
            for c in 0..3 {
                data.push(((x * 7 + y * 13 + c * 50 + ((x * y) % 31) * 3) % 256) as u8);
            }
        }
    }
    Rgb { w, h, data }
}

/// The mask both sides make: nobody on the left, a ramp, then the person, paler along the top.
pub(crate) fn soft_mask(w: usize, h: usize) -> Gray {
    let mut data = Vec::with_capacity(w * h);
    for y in 0..h {
        for x in 0..w {
            let m = if x < w / 4 { 0 } else if x >= w / 2 { 255 } else { (x - w / 4) * 255 / (w / 4).max(1) };
            data.push((if y < h / 8 { m / 2 } else { m }) as u8);
        }
    }
    Gray { w, h, data }
}

pub(crate) fn sha(data: &[u8]) -> String {
    digest(&SHA256, data).as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

pub(crate) fn recorded() -> Value {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/vision_cards.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn the_helpers_match_opencv_to_the_byte() {
    let v = recorded();
    for case in v["blur8"].as_array().unwrap() {
        let (w, h, sigma) = (case["w"].as_u64().unwrap() as usize, case["h"].as_u64().unwrap() as usize, case["sigma"].as_f64().unwrap());
        assert_eq!(sha(&pattern(w, h).blur(sigma).data), case["sha"], "{case}");
        assert_eq!(sha(&soft_mask(w, h).blur(sigma).data), case["mask_sha"], "{case}");
    }
    for case in v["blur32"].as_array().unwrap() {
        let (w, h, sigma) = (case["w"].as_u64().unwrap() as usize, case["h"].as_u64().unwrap() as usize, case["sigma"].as_f64().unwrap());
        let source: Vec<f32> = pattern(w, h).data.iter().map(|v| *v as f32 / 7.0).collect();
        let bytes: Vec<u8> = blur_f32(&source, w, h, 3, sigma).iter().flat_map(|v| v.to_le_bytes()).collect();
        assert_eq!(sha(&bytes), case["sha"], "{case}");
    }
    for case in v["soften"].as_array().unwrap() {
        let (w, h, edges) = (case["w"].as_u64().unwrap() as usize, case["h"].as_u64().unwrap() as usize, case["edges"].as_i64().unwrap());
        assert_eq!(sha(&soft_mask(w, h).soften(edges).data), case["sha"], "{case}");
    }
}

/// blurred the way Python's floats do it, for what the quicker one is held against.
fn blurred_as_python(frame: &Rgb, strength: i64, mask: &Gray) -> Rgb {
    let (w, h) = (frame.w, frame.h);
    let (sw, sh) = ((w / BLUR_SMALL).max(1), (h / BLUR_SMALL).max(1));
    let small = frame.shrink(sw, sh);
    let sigma = (1.0 + strength as f64 * 1.2) * SMALL / BLUR_SMALL as f64;
    let plain = small.blur(sigma);
    let rest: Vec<f32> = mask.shrink(sw, sh).data.iter().map(|m| 1.0 - *m as f32 / 255.0).collect();
    let spread = blur_f32(&rest, sw, sh, 1, sigma);
    let kept: Vec<f32> = small.data.as_chunks::<3>().0.iter().zip(&rest).flat_map(|(p, r)| p.iter().map(move |v| *v as f32 * r)).collect();
    let total = blur_f32(&kept, sw, sh, 3, sigma);
    let mut out = Rgb::new(sw, sh);
    for ((i, o), v) in out.data.iter_mut().enumerate().zip(&total) {
        let s = spread[i / 3];
        let weight = (s * 4.0).clamp(0.0, 1.0);
        *o = (v / s.max(1e-3) * weight + plain.data[i] as f32 * (1.0 - weight)).clamp(0.0, 255.0) as u8;
    }
    out.linear_exact(w, h)
}

/// The quicker blur's sums aren't OpenCV's to the last bit, and that much it may be off by.
const BLUR_LEVELS: u8 = 1;

#[test]
fn each_setting_makes_the_picture_python_makes() {
    let v = recorded();
    let (bw, bh) = (v["wide"]["w"].as_u64().unwrap() as usize, v["wide"]["h"].as_u64().unwrap() as usize);
    let (sw, sh) = (v["small"]["w"].as_u64().unwrap() as usize, v["small"]["h"].as_u64().unwrap() as usize);
    let reader: PictureReader = Arc::new(move |path: &str| {
        let (w, h) = if path == "wide" { (bw, bh) } else { (sw, sh) };
        let rgb = pattern(w, h);
        Ok(frame_of(&rgb))
    });
    for case in v["background"].as_array().unwrap() {
        let (w, h) = (case["w"].as_u64().unwrap() as usize, case["h"].as_u64().unwrap() as usize);
        let mut rig = Rig::with(BackgroundPlugin::with_reader(reader.clone()), Some(soft_mask(w, h)), vision::READY);
        rig.plugin.set_config(&case["config"]).unwrap();
        let got = rig.process(&pattern(w, h));
        if case["config"]["mode"] == "blur" {
            let (frame, mask) = (pattern(w, h), soft_mask(w, h));
            let (blur, edges) = (case["config"]["blur"].as_i64().unwrap(), case["config"]["edges"].as_i64().unwrap());
            let want = compose(&frame, &mask.soften(edges), &blurred_as_python(&frame, blur, &mask));
            assert_eq!(sha(&want.data), case["sha"], "{case}");
            let worst = got.data.iter().zip(&want.data).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
            assert!(worst <= BLUR_LEVELS, "{worst} levels off: {case}");
        } else {
            assert_eq!(sha(&got.data), case["sha"], "{case}");
        }
    }
}

// ── The blur bubble ───────────────────────────────────────────────────

fn samples(r: &Rig) -> usize {
    r.host.calls.iter().filter(|c| c.starts_with("set_ui_image card blur-picture")).count()
}

#[test]
fn the_blur_bubble_shows_until_the_preview_can_show_the_blur_itself() {
    let mut r = Rig::new(None, vision::READY);
    r.set_mode("blur");
    // nothing streams: the preview has no picture to show it on
    assert_eq!(r.shown("blur-peek"), json!("picture"));
    r.host.streaming = true;
    r.event(Event::StreamStarted { url: String::new() });
    assert_eq!(r.shown("blur-peek"), json!(""), "streaming with vision ready: the preview blurs");
    r.host.streaming = false;
    r.event(Event::StreamStopped);
    assert_eq!(r.shown("blur-peek"), json!("picture"));
    // streaming but the person can't be told from the room yet
    r.host.streaming = true;
    r.host.vision = Some(vision::LOADING.into());
    r.event(Event::VisionStateChanged { state: vision::LOADING.into() });
    assert_eq!(r.shown("blur-peek"), json!("picture"));
    r.host.vision = Some(vision::READY.into());
    r.event(Event::VisionStateChanged { state: vision::READY.into() });
    assert_eq!(r.shown("blur-peek"), json!(""));
    // the effect itself is off
    r.set_mode("off");
    assert_eq!(r.shown("blur-peek"), json!("picture"));
    r.set_mode("picture");
    assert_eq!(r.shown("blur-peek"), json!("picture"));
}

#[test]
fn the_blur_sample_is_made_only_while_the_bubble_is_up_and_follows_the_strength() {
    let mut r = Rig::new(None, vision::READY);
    r.set_mode("blur");
    r.ui("blur-moved", vec![json!(3)]);
    assert_eq!(samples(&r), 0, "closed: nothing is made");
    r.ui("blur-peek-open", vec![json!(true)]);
    assert_eq!(samples(&r), 1);
    let key = ("card".to_string(), "blur-picture".to_string());
    let sharpness = |r: &Rig| {
        let f = &r.host.images[&key];
        assert_eq!((f.width as usize, f.height as usize), (peek::WIDTH, peek::HEIGHT));
        // how much the wall changes from pixel to pixel along a row of the top shelf, away from the person
        let row = 10 * f.width as usize * 4;
        (0..60).map(|x| (f.data[row + x * 4] as i64 - f.data[row + (x + 1) * 4] as i64).abs()).sum::<i64>()
    };
    let mild = sharpness(&r);
    r.ui("blur-moved", vec![json!(10)]);
    assert_eq!(samples(&r), 2, "moving the strength makes a new sample");
    let strong = sharpness(&r);
    r.ui("blur-moved", vec![json!(1)]);
    let faint = sharpness(&r);
    assert!(strong < mild && mild < faint, "{faint} {mild} {strong}");
    r.ui("blur-peek-open", vec![json!(false)]);
    r.ui("blur-moved", vec![json!(5)]);
    assert_eq!(samples(&r), 3, "closed again: nothing more is made");
}

#[test]
fn the_blur_sample_leaves_the_person_sharp_as_the_effect_does() {
    let mut r = Rig::new(None, vision::READY);
    r.set_mode("blur");
    r.ui("blur-moved", vec![json!(10)]);
    r.ui("blur-peek-open", vec![json!(true)]);
    let (scene, _) = peek::sample_scene(peek::WIDTH, peek::HEIGHT);
    // the first pick of the bubble made the sample once and kept it
    assert!(std::ptr::eq(peek::blur_scene(), peek::blur_scene()));
    let f = &r.host.images[&("card".to_string(), "blur-picture".to_string())];
    // the middle of the shirt is the scene's own colour, the wall is not
    let at = |x: usize, y: usize| [f.data[(y * f.width as usize + x) * 4], f.data[(y * f.width as usize + x) * 4 + 1], f.data[(y * f.width as usize + x) * 4 + 2]];
    let shirt = (peek::WIDTH / 2, (peek::HEIGHT as f64 * 0.85) as usize);
    let (mine, theirs) = (at(shirt.0, shirt.1), scene.at(shirt.0, shirt.1));
    assert!((0..3).all(|c| (mine[c] as i32 - theirs[c] as i32).abs() <= 3), "{mine:?} {theirs:?}");
    let wall = (20, 40);
    assert_ne!(at(wall.0, wall.1), scene.at(wall.0, wall.1));
}
