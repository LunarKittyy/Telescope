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
fn without_a_mask_yet_the_whole_frame_is_blurred() {
    let mut rig = Rig::new(None, vision::LOADING);
    rig.set_mode("blur");
    let frame = stripes(64, 40);
    assert_eq!(rig.process(&frame), blurred(&frame, 6, None));
    assert_ne!(rig.process(&frame), frame);
    assert_eq!(rig.shown("vision-text"), json!("Getting ready…"));
}

#[test]
fn without_a_mask_yet_a_picture_covers_the_whole_frame() {
    let mut rig = Rig::with(BackgroundPlugin::with_reader(reader_of([0, 0, 255])), None, vision::LOADING);
    rig.plugin.load_picture("/pics/beach.png");
    rig.set_mode("picture");
    let out = rig.process(&Rgb::filled(64, 36, [200, 200, 200]));
    assert!(out.data.as_chunks::<3>().0.iter().all(|p| *p == [0, 0, 255]));
}

#[test]
fn a_frame_in_yuv_without_a_mask_is_hidden_too() {
    let mut rig = Rig::new(None, vision::LOADING);
    rig.set_mode("blur");
    let source = i420_of(&stripes(64, 40));
    let mut frame = source.clone();
    let Some(FrameStep::Cpu(mut step)) = rig.plugin.frame_step() else { panic!("a CPU step") };
    step.process(&mut frame, &rig.ctx);
    assert_ne!(frame.data, source.data);
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
    rig.ui("edges-moved", vec![json!(0)]);
    let frame = stripes(64, 36);
    let out = rig.process(&frame);
    assert_ne!(out, frame, "a picture that can't be read blurs the room instead of leaving it as it is");
    assert_eq!(column(&out, 0, 20), column(&frame, 0, 20), "the person stays");
    rig.event(Event::StreamStopped);
    assert_eq!(rig.shown("picture-problem"), json!("Can't open that picture"));
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

// ── Masks kept between frames ──

/// A context that hands out the same Arc until the test swaps it, as the vision service does for an unchanged mask.
struct Held(std::cell::RefCell<Arc<Vec<u8>>>);

impl FrameContext for Held {
    fn source_id(&self) -> &str {
        "phone"
    }
    fn person_mask(&self, _: &Frame) -> Option<Arc<Vec<u8>>> {
        Some(self.0.borrow().clone())
    }
    fn colour_preview(&self, frame: &Frame) -> Frame {
        frame.clone()
    }
}

fn step_in(mode: &str, blur: i64, edges: i64) -> Step {
    let mode = MODES.iter().position(|(k, _)| *k == mode).unwrap();
    Step { settings: Settings { mode, blur, edges, cut_in: 0, picture: None, high_quality: false }, covered: None, spares: Vec::new(), covered_yuv: None, derived: None, quarter: None }
}

fn varied_mask(w: usize, h: usize, seed: usize) -> Gray {
    Gray { w, h, data: (0..w * h).map(|i| if (i % w + seed * 5) % 41 < 20 { 255 } else { (i * 7 % 256) as u8 }).collect() }
}

#[test]
fn a_kept_mask_gives_the_same_picture_as_working_it_out_each_frame() {
    let (w, h) = (96, 54);
    let source = stripes(w, h);
    let mut blur_step = step_in("blur", 4, 3);
    let held = Held(Arc::new(varied_mask(w, h, 0).data.clone()).into());
    for round in 0..6 {
        // the mask changes on round 3; the edge setting on rounds 2 and 4
        if round == 3 {
            *held.0.borrow_mut() = Arc::new(varied_mask(w, h, 1).data.clone());
        }
        if round == 2 || round == 4 {
            blur_step.settings.edges = round as i64;
        }
        let mask = Gray { w, h, data: held.0.borrow().to_vec() };
        let want = compose(&source, &mask.soften(blur_step.settings.edges), &blurred(&source, 4, Some(&mask)));
        let mut frame = frame_of(&source);
        blur_step.process(&mut frame, &held);
        assert_eq!(Rgb::from_frame(&frame).unwrap(), Rgb::from_frame(&{ let mut f = frame_of(&source); want.write_to(&mut f); f }).unwrap(), "round {round}");
    }
    // the same size with a new mask Arc of equal bytes is a new mask, and a new frame size starts again
    let held = Held(Arc::new(varied_mask(64, 36, 2).data.clone()).into());
    let small = stripes(64, 36);
    let mask = Gray { w: 64, h: 36, data: held.0.borrow().to_vec() };
    let want = compose(&small, &mask.soften(blur_step.settings.edges), &blurred(&small, 4, Some(&mask)));
    let mut frame = frame_of(&small);
    blur_step.process(&mut frame, &held);
    let mut expected = frame_of(&small);
    want.write_to(&mut expected);
    assert_eq!(frame.data, expected.data);
}

#[test]
#[ignore = "timing bench, run with --release -- --ignored --nocapture"]
fn bench_background_with_an_unchanged_mask() {
    for (w, h) in [(1920usize, 1080usize), (3840, 2160)] {
        let source = stripes(w, h);
        let held = Held(Arc::new(varied_mask(w, h, 0).data.clone()).into());
        let mut step = step_in("blur", 4, 3);
        let best = |f: &mut dyn FnMut()| (0..5).map(|_| {
            let start = std::time::Instant::now();
            f();
            start.elapsed().as_secs_f64() * 1000.0
        }).fold(f64::MAX, f64::min);
        let mut frame = frame_of(&source);
        step.process(&mut frame, &held);
        let new = best(&mut || step.process(&mut frame, &held));
        let old = best(&mut || {
            let mask = Gray { w, h, data: held.0.borrow().to_vec() };
            let edges = mask.soften(3);
            compose(&source, &edges, &blurred(&source, 4, Some(&mask))).write_to(&mut frame);
        });
        println!("background blur {w}x{h}, unchanged mask: old {old:.2} ms, new {new:.2} ms");
    }
}

// ── Frames in YUV ──

/// A frame as the decoder makes it: I420.
fn i420_of(rgb: &Rgb) -> Frame {
    let mut frame = Frame::new(rgb.w as u32, rgb.h as u32, PixelFormat::I420, vec![0; PixelFormat::I420.size(rgb.w as u32, rgb.h as u32)]);
    rgb.write_to(&mut frame);
    frame
}

/// The vision service's way: a small mask made once and handed out at any size, the same Arc for as long as it asks the same size.
struct Served {
    base: Gray,
    made: std::cell::RefCell<Vec<((usize, usize), Arc<Vec<u8>>)>>,
}

impl Served {
    fn new(base: Gray) -> Served {
        Served { base, made: Default::default() }
    }

    fn at(&self, w: usize, h: usize) -> Arc<Vec<u8>> {
        let mut made = self.made.borrow_mut();
        if let Some((_, m)) = made.iter().find(|(size, _)| *size == (w, h)) {
            return m.clone();
        }
        let mask = Arc::new(self.base.linear_exact(w, h).data.clone());
        made.push(((w, h), mask.clone()));
        mask
    }
}

impl FrameContext for Served {
    fn source_id(&self) -> &str {
        "phone"
    }
    fn person_mask(&self, frame: &Frame) -> Option<Arc<Vec<u8>>> {
        Some(self.at(frame.width as usize, frame.height as usize))
    }
    fn person_mask_at(&self, _: &Frame, w: usize, h: usize) -> Option<Arc<Vec<u8>>> {
        Some(self.at(w, h))
    }
    fn colour_preview(&self, frame: &Frame) -> Frame {
        frame.clone()
    }
}


/// (wall ms, cpu ms) a call takes, the best wall of a few runs and the cpu over all of them (every thread of the process, so work spread over the workers counts in full).
pub(crate) fn measure(f: &mut dyn FnMut()) -> (f64, f64) {
    let cpu = || {
        let stat = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
        let fields: Vec<&str> = stat.rsplit_once(')').map_or("", |(_, rest)| rest).split_whitespace().collect();
        let ticks = |i: usize| fields.get(i).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
        (ticks(11) + ticks(12)) / 100.0 * 1000.0
    };
    f();
    let runs = 60;
    let before = cpu();
    let wall = (0..runs).map(|_| {
        let start = std::time::Instant::now();
        f();
        start.elapsed().as_secs_f64() * 1000.0
    }).fold(f64::MAX, f64::min);
    (wall, (cpu() - before) / runs as f64)
}

#[test]
#[ignore = "timing bench, run with --release -- --ignored --nocapture --test-threads 1"]
fn bench_background_in_yuv() {
    for (w, h) in [(1920usize, 1080usize), (3840, 2160)] {
        let (scene, mask) = peek::sample_scene(w, h);
        let served = Served::new(mask.shrink(256, 144));
        let source = i420_of(&scene);
        let mut step = step_in("blur", 6, 3);
        let mut frame = source.clone();
        step.process(&mut frame, &served);
        let (sw, sh) = ((w / BLUR_SMALL).max(1), (h / BLUR_SMALL).max(1));
        let whole = Gray { w, h, data: served.at(w, h).to_vec() };
        let (soft, rest) = (whole.soften(3), whole.shrink(sw, sh));
        // the RGB way as it was, with the mask kept for a steady picture and worked out again for a moved one
        let old_steady = measure(&mut || {
            let mut frame = source.clone();
            let rgb = Rgb::from_frame(&frame).unwrap();
            compose(&rgb, &soft, &blurred_by(&rgb, 6, Some(&rest), w, h)).write_to(&mut frame);
        });
        let old_moved = measure(&mut || {
            let mut frame = source.clone();
            let rgb = Rgb::from_frame(&frame).unwrap();
            let whole = Gray { w, h, data: served.at(w, h).to_vec() };
            let (soft, rest) = (whole.soften(3), whole.shrink(sw, sh));
            compose(&rgb, &soft, &blurred_by(&rgb, 6, Some(&rest), w, h)).write_to(&mut frame);
        });
        let new_steady = measure(&mut || {
            let mut frame = source.clone();
            step.process(&mut frame, &served);
        });
        let new_moved = measure(&mut || {
            let mut frame = source.clone();
            // a mask the model has just made: the same bytes at a new address
            for (_, m) in served.made.borrow_mut().iter_mut() {
                *m = Arc::new(m.to_vec());
            }
            step.process(&mut frame, &served);
        });
        println!("background blur {w}x{h} (wall ms, cpu ms): steady mask old {old_steady:.2?} new {new_steady:.2?}; moved mask old {old_moved:.2?} new {new_moved:.2?}");
    }
}

/// How far two pictures are apart: (PSNR in dB, the share of values more than 3 levels off, the worst gap).
fn apart(a: &Rgb, b: &Rgb) -> (f64, f64, u8) {
    let (mut sum, mut off, mut worst) = (0f64, 0usize, 0u8);
    for (x, y) in a.data.iter().zip(&b.data) {
        let d = x.abs_diff(*y);
        sum += (d as f64).powi(2);
        off += (d > 3) as usize;
        worst = worst.max(d);
    }
    let mse = (sum / a.data.len() as f64).max(1e-9);
    (10.0 * (255.0f64 * 255.0 / mse).log10(), off as f64 / a.data.len() as f64, worst)
}

/// What the RGB way makes of an I420 frame: the whole picture at full size, the mask at full size, composed.
fn in_rgb(frame: &Frame, served: &Served, blur: i64, edges: i64, behind: Option<&Rgb>) -> Rgb {
    let rgb = Rgb::from_frame(frame).unwrap();
    let mask = Gray { w: rgb.w, h: rgb.h, data: served.at(rgb.w, rgb.h).to_vec() };
    let under = match behind {
        Some(picture) => cover(picture, rgb.w, rgb.h),
        None => blurred(&rgb, blur, Some(&mask)),
    };
    // through YUV and back, as a frame goes, so only what the method changes is measured
    let mut out = frame.clone();
    compose(&rgb, &mask.soften(edges), &under).write_to(&mut out);
    Rgb::from_frame(&out).unwrap()
}

#[test]
fn the_blur_on_yuv_planes_looks_like_the_one_in_rgb() {
    let (scene, mask) = peek::sample_scene(960, 540);
    let served = Served::new(mask.shrink(256, 144));
    let frame = i420_of(&scene);
    // (blur, edges, least PSNR, most share of values over 3 levels off), set a little past what each measures
    for (blur, edges, least, share) in [(6, 3, 44.0, 0.02), (1, 0, 39.0, 0.16), (10, 10, 45.0, 0.01), (4, 5, 43.0, 0.045)] {
        let mut step = step_in("blur", blur, edges);
        let mut got = frame.clone();
        step.process(&mut got, &served);
        let want = in_rgb(&frame, &served, blur, edges, None);
        let (psnr, off, worst) = apart(&Rgb::from_frame(&got).unwrap(), &want);
        // the planes are averaged before the colours are worked out, so saturated colours come out a level or two apart
        assert!(psnr > least && off < share, "blur {blur} edges {edges}: {psnr:.1} dB, {:.3}% off by more than 3, worst {worst}", off * 100.0);
    }
}

#[test]
fn a_picture_on_yuv_planes_looks_like_the_one_in_rgb() {
    let (scene, mask) = peek::sample_scene(960, 540);
    let served = Served::new(mask.shrink(256, 144));
    let frame = i420_of(&scene);
    let behind = peek::sample_scene(480, 270).0;
    let mut step = step_in("picture", 6, 3);
    step.settings.picture = Some(Arc::new(behind.clone()));
    let mut got = frame.clone();
    step.process(&mut got, &served);
    let want = in_rgb(&frame, &served, 6, 3, Some(&behind));
    let (psnr, off, worst) = apart(&Rgb::from_frame(&got).unwrap(), &want);
    assert!(psnr > 60.0 && off < 0.0005, "{psnr:.1} dB, {:.3}% off by more than 3, worst {worst}", off * 100.0);
}

#[test]
fn with_no_one_in_the_mask_the_whole_frame_is_the_background() {
    let (scene, _) = peek::sample_scene(960, 540);
    let served = Served::new(Gray::new(256, 144));
    let frame = i420_of(&scene);
    let mut step = step_in("blur", 6, 3);
    let mut got = frame.clone();
    step.process(&mut got, &served);
    let want = in_rgb(&frame, &served, 6, 3, None);
    let (psnr, off, worst) = apart(&Rgb::from_frame(&got).unwrap(), &want);
    assert!(psnr > 44.0 && off < 0.025, "{psnr:.1} dB, {:.3}% off by more than 3, worst {worst}", off * 100.0);
    assert_ne!(got.data, frame.data);
}

#[test]
fn with_everyone_in_the_mask_the_frame_is_left_as_it_was() {
    let (scene, _) = peek::sample_scene(960, 540);
    let served = Served::new(Gray::filled(256, 144, 255));
    let frame = i420_of(&scene);
    let mut step = step_in("blur", 6, 3);
    let mut got = frame.clone();
    step.process(&mut got, &served);
    assert_eq!(got.data, frame.data);
}

#[test]
fn a_frame_in_yuv_is_hidden_until_a_mask_comes_and_again_if_it_goes() {
    let (scene, mask) = peek::sample_scene(480, 270);
    let frame = i420_of(&scene);
    let mut step = step_in("blur", 6, 3);
    let mut got = frame.clone();
    step.process(&mut got, &Ctx(None, Default::default()));
    assert_ne!(got.data, frame.data, "no mask yet: the room stays hidden");
    step.process(&mut got, &Served::new(mask.shrink(256, 144)));
    assert_ne!(got.data, frame.data);
    let mut lost = frame.clone();
    step.process(&mut lost, &Ctx(None, Default::default()));
    assert_ne!(lost.data, frame.data, "the mask went away and the room stays hidden");
}

#[test]
fn odd_sized_frames_in_yuv_come_out_the_same_size() {
    for (w, h) in [(101usize, 57usize), (7, 5), (2, 2), (640, 361)] {
        let (scene, mask) = peek::sample_scene(w, h);
        let served = Served::new(mask.shrink(256.min(w), 144.min(h)));
        for mode in ["blur", "picture"] {
            let mut step = step_in(mode, 5, 4);
            step.settings.picture = Some(Arc::new(Rgb::filled(8, 8, [10, 200, 30])));
            let mut frame = i420_of(&scene);
            step.process(&mut frame, &served);
            assert_eq!((frame.width as usize, frame.height as usize, frame.data.len()), (w, h, PixelFormat::I420.size(w as u32, h as u32)), "{mode} {w}x{h}");
        }
    }
}

/// Writes a picture as a binary PPM, for looking at.
pub(crate) fn dump_ppm(path: &std::path::Path, rgb: &Rgb) {
    let mut bytes = format!("P6\n{} {}\n255\n", rgb.w, rgb.h).into_bytes();
    bytes.extend_from_slice(&rgb.data);
    std::fs::write(path, bytes).unwrap();
}

/// Reads a binary PPM (no comments).
pub(crate) fn read_ppm(path: &str) -> Rgb {
    let bytes = std::fs::read(path).unwrap();
    let mut parts = bytes.splitn(5, |b| b.is_ascii_whitespace());
    assert_eq!(parts.next().unwrap(), b"P6");
    let mut number = || std::str::from_utf8(parts.next().unwrap()).unwrap().parse::<usize>().unwrap();
    let (w, h, _) = (number(), number(), number());
    let data = parts.next().unwrap().to_vec();
    Rgb { w, h, data }
}

/// Old, new and the gap between them times four, written as PPMs for a look.
pub(crate) fn dump_look(name: &str, old: &Rgb, new: &Rgb) {
    let Ok(dir) = std::env::var("LOOK_DIR") else { return };
    let dir = std::path::Path::new(&dir);
    std::fs::create_dir_all(dir).unwrap();
    let diff = Rgb { w: old.w, h: old.h, data: old.data.iter().zip(&new.data).map(|(a, b)| (a.abs_diff(*b) as u32 * 4).min(255) as u8).collect() };
    dump_ppm(&dir.join(format!("{name}_old.ppm")), old);
    dump_ppm(&dir.join(format!("{name}_new.ppm")), new);
    dump_ppm(&dir.join(format!("{name}_diff4x.ppm")), &diff);
}

/// A hard-edged, saturated card: colour bars, a checkerboard and thin lines in pure colours.
pub(crate) fn test_card(w: usize, h: usize) -> Rgb {
    let bars = [[255, 0, 0], [0, 255, 0], [0, 0, 255], [255, 255, 0], [255, 0, 255], [0, 255, 255], [255, 255, 255], [0, 0, 0]];
    let mut data = Vec::with_capacity(w * h * 3);
    for y in 0..h {
        for x in 0..w {
            let p = if y < h / 3 {
                bars[x * bars.len() / w]
            } else if y < 2 * h / 3 {
                if (x / 24 + y / 24) % 2 == 0 { [255, 40, 0] } else { [0, 60, 255] }
            } else if (x / 3) % 2 == 0 {
                bars[(y * 8 / h) % 8]
            } else {
                [20, 200, 40]
            };
            data.extend_from_slice(&p);
        }
    }
    Rgb { w, h, data }
}

/// A centred ellipse as a person mask at the size the model works at.
pub(crate) fn ellipse_mask(w: usize, h: usize) -> Gray {
    let mut data = vec![0u8; w * h];
    for y in 0..h {
        for x in 0..w {
            let (dx, dy) = ((x as f64 + 0.5) / w as f64 - 0.5, (y as f64 + 0.5) / h as f64 - 0.5);
            let d = (dx / 0.18).powi(2) + (dy / 0.38).powi(2);
            data[y * w + x] = (255.0 * (1.5 - d).clamp(0.0, 1.0)) as u8;
        }
    }
    Gray { w, h, data }
}

#[test]
#[ignore = "writes pictures to look at: LOOK_DIR=out LOOK_FRAME=frame.ppm cargo test -- --ignored look_at_background"]
fn look_at_background_blur_old_and_new() {
    let mut pictures = vec![("card", test_card(1920, 1080))];
    if let Ok(path) = std::env::var("LOOK_FRAME") {
        pictures.push(("clip", read_ppm(&path)));
    }
    for (name, scene) in pictures {
        let served = Served::new(ellipse_mask(256, 144));
        let frame = i420_of(&scene);
        let mut step = step_in("blur", 6, 3);
        let mut got = frame.clone();
        step.process(&mut got, &served);
        let want = in_rgb(&frame, &served, 6, 3, None);
        let new = Rgb::from_frame(&got).unwrap();
        let (psnr, off, worst) = apart(&new, &want);
        println!("background blur {name}: {psnr:.1} dB, {:.3}% off by more than 3, worst {worst}", off * 100.0);
        dump_look(&format!("background_{name}"), &want, &new);
    }
}

#[test]
#[ignore = "timing bench, run with --release -- --ignored --nocapture --test-threads 1"]
fn bench_background_at_the_vision_rate() {
    for (w, h) in [(1920usize, 1080usize), (3840, 2160)] {
        let (scene, mask) = peek::sample_scene(w, h);
        let served = Served::new(mask.shrink(256, 144));
        let source = i420_of(&scene);
        let mut step = step_in("blur", 6, 3);
        let mut frame = source.clone();
        step.process(&mut frame, &served);
        // a new mask every third frame, as the vision service makes them
        let mut n = 0usize;
        let mut run = || {
            n += 1;
            if n % 3 == 0 {
                for (_, m) in served.made.borrow_mut().iter_mut() {
                    *m = Arc::new(m.to_vec());
                }
            }
            let mut frame = source.clone();
            step.process(&mut frame, &served);
        };
        run();
        let start = std::time::Instant::now();
        (0..60).for_each(|_| run());
        println!("background blur {w}x{h} vision rate, mean wall ms: {:.2}", start.elapsed().as_secs_f64() * 1000.0 / 60.0);
    }
}

#[test]
fn kept_expanded_masks_and_blur_weights_give_what_a_fresh_step_gives_in_yuv() {
    let (w, h) = (160, 90);
    let (scene, mask) = peek::sample_scene(w, h);
    let served = Served::new(mask.shrink(40, 22));
    let source = i420_of(&scene);
    let mut kept = step_in("blur", 4, 3);
    // frames on the same mask, then the edge setting and the strength move, then a new mask
    for round in 0..6 {
        if round == 2 {
            kept.settings.edges = 6;
        }
        if round == 3 {
            kept.settings.blur = 9;
        }
        if round == 5 {
            for (_, m) in served.made.borrow_mut().iter_mut() {
                *m = Arc::new(m.iter().map(|v| 255 - v).collect());
            }
        }
        let mut fresh = step_in("blur", kept.settings.blur, kept.settings.edges);
        let (mut a, mut b) = (source.clone(), source.clone());
        kept.process(&mut a, &served);
        fresh.process(&mut b, &served);
        assert!(a.data == b.data, "round {round}");
    }
}

/// One quarter-size mask whatever the frame size, as the vision service hands it out when two sizes round to the same quarter.
struct OneMask(Arc<Vec<u8>>);

impl FrameContext for OneMask {
    fn source_id(&self) -> &str {
        "phone"
    }
    fn person_mask(&self, _: &Frame) -> Option<Arc<Vec<u8>>> {
        Some(self.0.clone())
    }
    fn person_mask_at(&self, _: &Frame, _: usize, _: usize) -> Option<Arc<Vec<u8>>> {
        Some(self.0.clone())
    }
    fn colour_preview(&self, frame: &Frame) -> Frame {
        frame.clone()
    }
}

#[test]
fn a_resize_that_keeps_the_quarter_size_mask_gives_what_a_fresh_step_gives() {
    let (scene, mask) = peek::sample_scene(160, 90);
    let served = OneMask(Arc::new(mask.shrink(40, 22).data.clone()));
    let mut kept = step_in("blur", 4, 3);
    // 160x90 to 162x90 keeps the 40x22 mask, then to 163x88 and back
    for (w, h) in [(160, 90), (162, 90), (163, 88), (160, 90), (162, 90)] {
        let (scene, _) = peek::sample_scene(w, h);
        let source = i420_of(&scene);
        let mut fresh = step_in("blur", 4, 3);
        let (mut a, mut b) = (source.clone(), source.clone());
        kept.process(&mut a, &served);
        fresh.process(&mut b, &served);
        assert!(a.data == b.data, "{w}x{h}");
    }
    let _ = scene;
}

// ── The High quality cutout ─────────────────────────────────────────────

/// A normal mask with the person on the left and a high quality one with the person on the right, and how often each is asked for.
struct TwoMasks(Gray, Gray, std::cell::Cell<(u32, u32)>);

impl FrameContext for TwoMasks {
    fn source_id(&self) -> &str {
        "phone"
    }
    fn person_mask(&self, _frame: &Frame) -> Option<Arc<Vec<u8>>> {
        self.2.set((self.2.get().0 + 1, self.2.get().1));
        Some(Arc::new(self.0.data.clone()))
    }
    fn person_mask_hq(&self, _frame: &Frame) -> Option<Arc<Vec<u8>>> {
        self.2.set((self.2.get().0, self.2.get().1 + 1));
        Some(Arc::new(self.1.data.clone()))
    }
    fn person_mask_hq_at(&self, _frame: &Frame, w: usize, h: usize) -> Option<Arc<Vec<u8>>> {
        self.2.set((self.2.get().0, self.2.get().1 + 1));
        Some(Arc::new(self.1.shrink(w, h).data.to_vec()))
    }
    fn colour_preview(&self, frame: &Frame) -> Frame {
        frame.clone()
    }
}

fn right_mask(w: usize, h: usize) -> Gray {
    Gray { w, h, data: (0..w * h).map(|i| if i % w >= w / 2 { 255 } else { 0 }).collect() }
}

#[test]
fn the_high_quality_cutout_is_off_unless_asked_for_and_kept_in_the_settings_only_when_on() {
    let mut rig = Rig::new(None, vision::READY);
    assert_eq!(rig.shown("high-quality"), json!(false));
    assert_eq!(rig.plugin.get_config(), json!({"mode": "off", "blur": 6, "edges": 3, "picture": ""}));
    rig.set_mode("blur");
    rig.ui("high-quality-toggled", vec![json!(true)]);
    assert_eq!(rig.shown("high-quality"), json!(true));
    assert_eq!(rig.plugin.get_config()["high_quality"], json!(true));
    // a saved setting, a preset and a damaged value
    let saved = rig.plugin.get_config();
    let mut other = Rig::new(None, vision::READY);
    other.plugin.set_config(&saved).unwrap();
    assert!(other.plugin.high_quality);
    other.plugin.set_config(&json!({"mode": "blur", "high_quality": "yes"})).unwrap();
    assert!(!other.plugin.high_quality);
    rig.ui("high-quality-toggled", vec![json!(false)]);
    assert!(rig.plugin.get_config().get("high_quality").is_none());
    assert!(rig.host.calls.iter().filter(|c| *c == "refresh_frame_step").count() >= 3, "each change reaches the stream");
}

#[test]
fn the_frame_step_asks_for_the_high_quality_mask_only_with_it_switched_on() {
    let (w, h) = (64, 36);
    let mut rig = Rig::new(None, vision::READY);
    rig.set_mode("blur");
    rig.ui("edges-moved", vec![json!(0)]);
    let ctx = TwoMasks(half_mask(w, h), right_mask(w, h), Default::default());
    let frame = stripes(w, h);
    let run = |rig: &mut Rig| {
        let mut f = frame_of(&frame);
        if let Some(FrameStep::Cpu(mut step)) = rig.plugin.frame_step() {
            step.process(&mut f, &ctx);
        }
        Rgb::from_frame(&f).unwrap()
    };
    let normal = run(&mut rig);
    assert_eq!(ctx.2.get(), (1, 0));
    // the person stays on the left with the normal mask
    assert_eq!(column(&normal, 0, 20), column(&frame, 0, 20));
    rig.ui("high-quality-toggled", vec![json!(true)]);
    let better = run(&mut rig);
    assert_eq!(ctx.2.get(), (1, 1), "the normal one isn't asked for as well");
    assert_eq!(column(&better, 44, 64), column(&frame, 44, 64));
    assert_ne!(column(&better, 0, 20), column(&frame, 0, 20), "and the left is blurred now");
}

#[test]
fn a_frame_in_yuv_gets_the_high_quality_mask_too() {
    let (scene, mask) = peek::sample_scene(480, 270);
    let frame = i420_of(&scene);
    let ctx = TwoMasks(Gray::filled(mask.w, mask.h, 255), mask, Default::default());
    let mut normal = step_in("blur", 6, 3);
    let mut better = step_in("blur", 6, 3);
    better.settings.high_quality = true;
    let (mut a, mut b) = (frame.clone(), frame.clone());
    normal.process(&mut a, &ctx);
    assert_eq!(ctx.2.get(), (1, 0));
    better.process(&mut b, &ctx);
    assert_eq!(ctx.2.get(), (1, 1), "only the high quality one is asked, at the quarter size");
    assert_ne!(a.data, b.data, "the person is cut out differently");
    assert_eq!(a.data, frame.data, "everyone in the normal mask: the frame is left as it was");
}

#[test]
fn a_service_with_no_high_quality_model_gives_the_normal_mask() {
    // Ctx leaves person_mask_hq as it is: the default is person_mask
    let mut rig = Rig::new(Some(half_mask(64, 36)), vision::READY);
    rig.set_mode("blur");
    rig.ui("edges-moved", vec![json!(0)]);
    let before = rig.process(&stripes(64, 36));
    rig.ui("high-quality-toggled", vec![json!(true)]);
    assert_eq!(rig.process(&stripes(64, 36)), before);
    assert_eq!(rig.ctx.1.get(), 2);
}

#[test]
fn with_the_high_quality_cutout_on_and_no_mask_at_all_the_room_stays_hidden() {
    // Neither model has a mask (still loading, downloading, failed): that is the same as having lost track, so nothing clear gets out
    let mut rig = Rig::new(None, vision::LOADING);
    rig.set_mode("blur");
    rig.ui("high-quality-toggled", vec![json!(true)]);
    let frame = stripes(64, 40);
    assert_eq!(rig.process(&frame), blurred(&frame, 6, None));
    let source = i420_of(&frame);
    let mut yuv = source.clone();
    let Some(FrameStep::Cpu(mut step)) = rig.plugin.frame_step() else { panic!("a CPU step") };
    step.process(&mut yuv, &rig.ctx);
    assert_ne!(yuv.data, source.data);
    let mut rig = Rig::with(BackgroundPlugin::with_reader(reader_of([0, 0, 255])), None, vision::LOADING);
    rig.plugin.load_picture("/pics/beach.png");
    rig.set_mode("picture");
    rig.ui("high-quality-toggled", vec![json!(true)]);
    assert!(rig.process(&Rgb::filled(64, 36, [200, 200, 200])).data.as_chunks::<3>().0.iter().all(|p| *p == [0, 0, 255]));
}

#[test]
fn the_card_offers_the_cutouts_download_and_says_the_normal_one_is_used_meanwhile() {
    let mut rig = Rig::new(Some(half_mask(64, 36)), vision::READY);
    rig.host.matting = Some(vision::MISSING.into());
    rig.host.matting_pack = VisionPack { available: true, size_mb: 26, ..VisionPack::default() };
    rig.set_mode("blur");
    rig.ui("edges-moved", vec![json!(0)]);
    assert_eq!(rig.shown("cutout-text"), json!(""), "nothing while the switch is off");
    rig.ui("high-quality-toggled", vec![json!(true)]);
    assert_eq!(rig.shown("cutout-text"), json!("Needs a one-time download (about 26 MB). Until then the normal cutout is used."));
    assert_eq!(rig.shown("cutout-download"), json!(true));
    assert!(rig.shown("quality-tip").as_str().unwrap().contains("40% of a CPU core"));
    // the picture is not held up meanwhile
    let out = rig.process(&stripes(64, 36));
    assert_eq!(column(&out, 0, 20), column(&stripes(64, 36), 0, 20));
    rig.ui("cutout-download-clicked", vec![]);
    assert!(rig.host.calls.iter().any(|c| c == "download_matting"));
    assert!(!rig.host.calls.iter().any(|c| c == "download_vision"), "the vision pack is another download");
    rig.host.matting_pack.downloading = true;
    rig.host.matting_pack.size = 100;
    rig.host.matting_pack.done = 25;
    rig.event(Event::VisionDownloadProgress { done: 25, size: 100 });
    assert_eq!(rig.shown("cutout-text"), json!("Downloading… 25%"));
    rig.host.matting_pack = VisionPack { ready: true, available: true, size_mb: 26, ..VisionPack::default() };
    rig.host.matting = Some(vision::FAILED.into());
    rig.event(Event::MattingStateChanged { state: vision::FAILED.into() });
    assert!(rig.shown("cutout-text").as_str().unwrap().contains("didn't load"));
    rig.host.matting = Some(vision::READY.into());
    rig.event(Event::MattingStateChanged { state: vision::READY.into() });
    assert_eq!(rig.shown("cutout-text"), json!(""));
}

// ── Cut-in ──────────────────────────────────────────────────────────────

fn person_area(mask: &Gray) -> u64 {
    mask.data.iter().map(|&v| v as u64).sum()
}

#[test]
fn cut_in_zero_leaves_the_mask_and_a_positive_one_shrinks_the_person() {
    let mask = soft_mask(256, 144);
    for edges in [0, 3, 8] {
        assert_eq!(mask.soften_cut(edges, 0), mask.soften(edges));
    }
    assert_eq!(mask.cut_in(0), mask);
    let (mut last, mut last_soft) = (person_area(&mask), person_area(&mask.soften(3)));
    for cut in 1..=10 {
        let area = person_area(&mask.cut_in(cut));
        let soft = person_area(&mask.soften_cut(3, cut));
        assert!(area < last && soft < last_soft, "cut-in {cut}: {area} vs {last}, {soft} vs {last_soft}");
        (last, last_soft) = (area, soft);
    }
    // the border of a picture isn't a person's edge
    let whole = Gray::filled(32, 18, 255);
    assert_eq!(whole.cut_in(5), whole);
}

#[test]
fn cut_in_is_saved_only_when_set_and_clamped_and_reaches_the_stream() {
    let mut rig = Rig::new(None, vision::READY);
    rig.set_mode("blur");
    assert_eq!(rig.shown("cut-in"), json!(0));
    assert!(rig.plugin.get_config().get("cut_in").is_none());
    rig.ui("cut-in-moved", vec![json!(4.4)]);
    assert_eq!((rig.shown("cut-in"), rig.shown("cut-in-text")), (json!(4), json!("4")));
    assert_eq!(rig.plugin.get_config()["cut_in"], json!(4));
    let saved = rig.plugin.get_config();
    let mut other = Rig::new(None, vision::READY);
    other.plugin.set_config(&saved).unwrap();
    assert_eq!(other.plugin.cut_in, 4);
    other.plugin.set_config(&json!({"mode": "blur", "cut_in": 99})).unwrap();
    assert_eq!(other.plugin.cut_in, 10);
    other.plugin.set_config(&json!({"mode": "blur", "cut_in": "x"})).unwrap();
    assert_eq!(other.plugin.cut_in, 0);
    assert!(rig.host.calls.iter().any(|c| c == "refresh_frame_step"));
}

#[test]
fn cut_in_pulls_the_cutout_in_with_either_mask() {
    let (w, h) = (96, 54);
    let source = stripes(w, h);
    let ctx = TwoMasks(half_mask(w, h), half_mask(w, h), Default::default());
    for high in [false, true] {
        let mut plain = step_in("blur", 4, 0);
        let mut cut = step_in("blur", 4, 0);
        (plain.settings.high_quality, cut.settings.high_quality, cut.settings.cut_in) = (high, high, 3);
        let (mut a, mut b) = (frame_of(&source), frame_of(&source));
        plain.process(&mut a, &ctx);
        cut.process(&mut b, &ctx);
        let (a, b) = (Rgb::from_frame(&a).unwrap(), Rgb::from_frame(&b).unwrap());
        // the person is the left half: the columns just inside its edge are the room's now
        assert_eq!(column(&a, 44, 46), column(&source, 44, 46));
        assert_ne!(column(&b, 44, 46), column(&source, 44, 46), "high quality {high}");
        assert_eq!(column(&b, 0, 20), column(&source, 0, 20), "the middle of the person stays");
    }
}

#[test]
fn cut_in_on_yuv_planes_looks_like_the_one_in_rgb() {
    let (scene, mask) = peek::sample_scene(960, 540);
    let served = Served::new(mask.shrink(256, 144));
    let frame = i420_of(&scene);
    for cut in [2, 5, 10] {
        let mut step = step_in("blur", 6, 3);
        step.settings.cut_in = cut;
        let mut got = frame.clone();
        step.process(&mut got, &served);
        let rgb = Rgb::from_frame(&frame).unwrap();
        let whole = Gray { w: rgb.w, h: rgb.h, data: served.at(rgb.w, rgb.h).to_vec() };
        let mut want = frame.clone();
        compose(&rgb, &whole.soften_cut(3, cut), &blurred(&rgb, 6, Some(&whole))).write_to(&mut want);
        let (psnr, off, worst) = apart(&Rgb::from_frame(&got).unwrap(), &Rgb::from_frame(&want).unwrap());
        assert!(psnr > 44.0 && off < 0.03, "cut-in {cut}: {psnr:.1} dB, {:.3}% off by more than 3, worst {worst}", off * 100.0);
    }
}
