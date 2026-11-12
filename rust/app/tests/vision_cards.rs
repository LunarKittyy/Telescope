//! Background and Privacy blur on what the real models say about the astronaut, when a pack is there (TELESCOPE_VISION_PACK, or this version's pack in the Python app's folder). The plugins' own tests cover every setting against Python's pictures.

use serde_json::json;
use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use telescope_core::frame::{Frame, FrameContext, FrameStep, PixelFormat};
use telescope_core::people::{PeopleTracker, Track};
use telescope_core::plugin::Plugin;
use telescope_core::testing::FakeHost;
use telescope_core::ui::UiEvent;
use telescope_core::vision::VisionService;
use telescope_core::Event;
use telescope_effects::background::BackgroundPlugin;
use telescope_effects::privacy_blur::PrivacyBlurPlugin;
use telescope_vision::mediapipe::MediaPipe;
use telescope_vision::ops::Rgb;
use telescope_vision::pack::{self, PackInfo};
use telescope_vision::service::Backend;
use telescope_vision::VisionHost;

const SIZE: (usize, usize) = (384, 216);

fn folder() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("TELESCOPE_VISION_PACK") {
        return Some(PathBuf::from(dir));
    }
    let (dir, info) = (pack::default_dir(), PackInfo::current());
    info.installed(&dir).then(|| info.folder(&dir))
}

fn astronaut() -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../crates/vision/tests/fixtures/astronaut.rgb.z");
    let mut data = Vec::new();
    flate2::read::ZlibDecoder::new(std::fs::File::open(path).unwrap()).read_to_end(&mut data).unwrap();
    data
}

fn frame() -> Frame {
    let data = astronaut().as_chunks::<3>().0.iter().flat_map(|p| [p[0], p[1], p[2], 255]).collect();
    Frame::new(SIZE.0 as u32, SIZE.1 as u32, PixelFormat::Rgba, data)
}

struct Ctx(Arc<VisionHost>);

impl FrameContext for Ctx {
    fn source_id(&self) -> &str {
        "phone"
    }
    fn person_mask(&self, frame: &Frame) -> Option<Arc<Vec<u8>>> {
        self.0.person_mask("phone", frame)
    }
    fn colour_preview(&self, frame: &Frame) -> Frame {
        frame.clone()
    }
}

/// The real vision service's mask for the astronaut, once its models have loaded.
fn mask_of(ctx: &Ctx) -> Arc<Vec<u8>> {
    let started = Instant::now();
    let mut number = 0;
    loop {
        number += 1;
        let mut f = frame();
        f.number = number;
        if let Some(mask) = ctx.person_mask(&f) {
            return mask;
        }
        assert!(started.elapsed() < Duration::from_secs(60), "no mask: {}", ctx.0.state());
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn through(step: Option<FrameStep>, ctx: &Ctx) -> Frame {
    let Some(FrameStep::Cpu(mut step)) = step else { panic!("a CPU step") };
    let mut out = frame();
    out.number = 1_000_000;
    step.process(&mut out, ctx);
    out
}

/// How many levels the mask's person and the room moved on average: (person, room).
fn changed(out: &Frame, mask: &[u8], person_from: u8) -> (f64, f64) {
    let before = frame();
    let (mut person, mut room) = ((0u64, 0u64), (0u64, 0u64));
    for (i, m) in mask.iter().enumerate() {
        let moved: u64 = (0..3).map(|c| out.data[i * 4 + c].abs_diff(before.data[i * 4 + c]) as u64).sum();
        let tally = if *m >= person_from { &mut person } else if *m == 0 { &mut room } else { continue };
        tally.0 += moved;
        tally.1 += 3;
    }
    (person.0 as f64 / person.1 as f64, room.0 as f64 / room.1 as f64)
}

#[test]
fn background_and_privacy_blur_work_on_the_real_models() {
    let Some(folder) = folder() else {
        eprintln!("no vision pack here: set TELESCOPE_VISION_PACK to a pack folder");
        return;
    };
    let backend: Arc<MediaPipe> = Arc::new(MediaPipe::load(&folder).unwrap_or_else(|e| panic!("{e:?}")));
    let found = backend.clone();
    let dir = telescope_core::testing::scratch_dir("vision-cards");
    let host = VisionHost::with(
        Box::new(move || Ok(found.clone() as Arc<dyn Backend>)),
        Some(Box::new(|| true)),
        dir,
        PackInfo::current(),
        Box::new(|_, _| Err(std::io::Error::other("no downloads in tests"))),
    );
    host.vision().wait_for_masks();
    let ctx = Ctx(Arc::new(host));
    let mask = mask_of(&ctx);
    assert_eq!(mask.len(), SIZE.0 * SIZE.1);
    assert!(mask.iter().filter(|m| **m > 200).count() > 2000, "the astronaut is someone");

    let mut host = FakeHost::new();
    let mut background = BackgroundPlugin::new();
    background.set_config(&json!({"mode": "blur", "blur": 6, "edges": 0})).unwrap();
    let out = through(background.frame_step(), &ctx);
    let (person, room) = changed(&out, &mask, 255);
    assert_eq!(person, 0.0, "the person isn't blurred");
    assert!(room > 1.0, "the room is: {room}");

    // Privacy blur by hand, turned on from its card: the whole picture is blurred
    let people = backend.pose().unwrap().detect(&Rgb::new(SIZE.0, SIZE.1, astronaut()), 1).unwrap();
    assert!(!people.is_empty());
    let mut tracker = PeopleTracker::new();
    tracker.update(&people, 0.0);
    let tracks = tracker.update(&people, 0.1);
    let mut privacy = PrivacyBlurPlugin::new();
    host.focused = Some("phone".into());
    privacy.setup(&mut host);
    privacy.on_ui(&mut host, &UiEvent { ui: "card".into(), callback: "now-clicked".into(), args: vec![] });
    let records = tracks.iter().map(Track::to_value).collect();
    privacy.on_event(&mut host, &Event::PeopleTracked { source_id: "phone".into(), tracks: records, count: tracks.len() as u32 });
    assert!(host.calls.iter().any(|c| c == "want_people Privacy blur true"));
    let out = through(privacy.frame_step(), &ctx);
    let (person, room) = changed(&out, &mask, 250);
    eprintln!("Privacy blur: the person moved {person} levels and the room {room}");
    assert!(room > 1.0 && person > 1.0, "the person and the room are both blurred: {person} and {room}");
}

/// The astronaut slid x pixels to the right, what leaves on one side coming back on the other.
fn slid(x: usize) -> Frame {
    let mut picture = frame();
    let row = SIZE.0 * 4;
    let data = picture.data_mut();
    data.chunks_exact_mut(row).for_each(|line| line.rotate_right(x * 4));
    picture
}

/// Where the mask's person is, left to right.
fn middle(mask: &[u8]) -> f64 {
    let (mut sum, mut count) = (0u64, 0u64);
    for (i, m) in mask.iter().enumerate() {
        if *m > 128 {
            sum += (i % SIZE.0) as u64;
            count += 1;
        }
    }
    sum as f64 / count.max(1) as f64
}

#[test]
fn the_real_models_mask_keeps_up_with_someone_moving_across() {
    let Some(folder) = folder() else {
        eprintln!("no vision pack here: set TELESCOPE_VISION_PACK to a pack folder");
        return;
    };
    let backend: Arc<MediaPipe> = Arc::new(MediaPipe::load(&folder).unwrap_or_else(|e| panic!("{e:?}")));
    let host = VisionHost::with(
        Box::new(move || Ok(backend.clone() as Arc<dyn Backend>)),
        Some(Box::new(|| true)),
        telescope_core::testing::scratch_dir("vision-moving"),
        PackInfo::current(),
        Box::new(|_, _| Err(std::io::Error::other("no downloads in tests"))),
    );
    let ctx = Ctx(Arc::new(host));
    mask_of(&ctx);
    let still = mask_of(&ctx);
    let start = middle(&still);
    // A step a frame of 1/64 of the picture is someone crossing it in about two seconds
    const STEP: usize = 6;
    let mut behind = Vec::new();
    for (i, number) in (0..24).zip(2_000_000u64..) {
        let mut f = slid(i * STEP);
        f.number = number;
        let began = Instant::now();
        if let Some(mask) = ctx.person_mask(&f).filter(|_| i >= 8) {
            behind.push((start + (i * STEP) as f64 - middle(&mask)) / STEP as f64);
        }
        std::thread::sleep(Duration::from_millis(33).saturating_sub(began.elapsed()));
    }
    let worst = behind.iter().cloned().fold(0.0, f64::max);
    eprintln!("Frames behind, moving {STEP} of {} pixels a frame: {behind:.1?}", SIZE.0);
    assert!(worst < 3.0, "the mask is {worst:.1} frames behind");
}
