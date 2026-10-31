//! The Fun pack card with the real vision service behind it and a fake MediaPipe, whose pack gets downloaded part way through
//! (test_fun_pack.py's _VisionHost).

use serde_json::{json, Value};
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use telescope_core::frame::{Frame, FrameContext, FrameStep, PixelFormat};
use telescope_core::plugin::Plugin;
use telescope_core::pose::Person;
use telescope_core::testing::FakeHost;
use telescope_core::ui::UiEvent;
use telescope_core::vision::{VisionPack, VisionService, MISSING, READY};
use telescope_core::{Event, EventBus};
use telescope_effects::fun_pack::{FunPackPlugin, EFFECTS};
use telescope_vision::ops::Rgb;
use telescope_vision::pack::PackInfo;
use telescope_vision::service::{Backend, Gray, LoadError, PoseModel, Segmenter, VisionError};
use telescope_vision::VisionHost;

const W: u32 = 160;
const H: u32 = 90;

struct Segmenting;
struct Everyone;

impl Segmenter for Everyone {
    fn segment(&mut self, frame: &Rgb, _ts_ms: i64) -> Result<Gray, VisionError> {
        Ok(Gray { w: frame.w, h: frame.h, data: vec![200; frame.w * frame.h] })
    }
}

impl PoseModel for Everyone {
    fn detect(&mut self, _frame: &Rgb, _ts_ms: i64) -> Result<Vec<Person>, VisionError> {
        Ok(Vec::new())
    }
}

impl Backend for Segmenting {
    fn version(&self) -> String {
        "test".into()
    }
    fn segmenter(&self) -> Result<Box<dyn Segmenter>, VisionError> {
        Ok(Box::new(Everyone))
    }
    fn pose(&self) -> Result<Box<dyn PoseModel>, VisionError> {
        Ok(Box::new(Everyone))
    }
}

/// A stream's frames, which remember what the vision service said about the last one.
struct Ctx {
    vision: VisionHost,
    masked: Cell<Option<bool>>,
}

impl FrameContext for Ctx {
    fn source_id(&self) -> &str {
        "phone"
    }
    fn person_mask(&self, frame: &Frame) -> Option<Arc<Vec<u8>>> {
        let mask = self.vision.person_mask("phone", frame);
        self.masked.set(Some(mask.is_some()));
        mask
    }
    fn colour_preview(&self, frame: &Frame) -> Frame {
        frame.clone()
    }
}

struct Rig {
    plugin: FunPackPlugin,
    host: FakeHost,
    bus: EventBus,
    vision: VisionHost,
    there: Arc<AtomicBool>,
    number: u64,
}

impl Rig {
    fn new() -> Rig {
        let there = Arc::new(AtomicBool::new(false));
        let (found, is_there) = (there.clone(), there.clone());
        let vision = VisionHost::with(
            Box::new(move || if found.load(Ordering::SeqCst) { Ok(Arc::new(Segmenting) as Arc<dyn Backend>) } else { Err(LoadError::Missing("pack not downloaded".into())) }),
            Some(Box::new(move || is_there.load(Ordering::SeqCst))),
            telescope_core::testing::scratch_dir("fun-pack-vision"),
            PackInfo::current(),
            Box::new(|_, _| Err(std::io::Error::other("no downloads in tests"))),
        );
        let bus = EventBus::new();
        vision.attach(bus.sender());
        let mut host = FakeHost::new();
        host.focused = Some("phone".into());
        host.pack = VisionPack { available: true, size_mb: 47, ..VisionPack::default() };
        let mut plugin = FunPackPlugin::new();
        plugin.setup(&mut host);
        plugin.set_config(&json!({})).unwrap();
        plugin.on_event(&mut host, &Event::PhonesChanged { count: 1 });
        Rig { plugin, host, bus, vision, there, number: 0 }
    }

    /// Hands the plugin what the service said, and the service what the plugin wants, as the window does.
    fn pump(&mut self) {
        self.host.vision = Some(VisionService::state(&self.vision));
        while let Some(event) = self.bus.next() {
            self.host.vision = Some(VisionService::state(&self.vision));
            self.plugin.on_event(&mut self.host, &event);
        }
    }

    fn pick(&mut self, effect: &str) {
        let before = self.host.calls.len();
        let i = EFFECTS.iter().position(|(k, _)| *k == effect).unwrap();
        self.plugin.on_ui(&mut self.host, &UiEvent { ui: "card".into(), callback: "effect-picked".into(), args: vec![json!(i)] });
        let wanted: Vec<bool> = self.host.calls[before..].iter().filter_map(|c| c.strip_prefix("want_people fun_pack ")).map(|on| on == "true").collect();
        for on in wanted {
            VisionService::want_people(&self.vision, "fun_pack", on);
        }
        self.pump();
    }

    fn shown(&self, key: &str) -> Value {
        self.host.calls.iter().rev().filter_map(|c| c.strip_prefix("set_ui_props card ")).filter_map(|j| serde_json::from_str::<Value>(j).ok())
            .find_map(|v| v.get(key).cloned()).unwrap_or(Value::Null)
    }

    fn status(&self) -> String {
        self.shown("vision-text").as_str().unwrap().to_string()
    }

    fn until(&mut self, what: &str, done: impl Fn(&mut Rig) -> bool) {
        let end = Instant::now() + Duration::from_secs(2);
        loop {
            self.pump();
            if done(self) {
                return;
            }
            assert!(Instant::now() < end, "timed out: {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Whether the shown effect got a mask for one more frame.
    fn masked(&mut self) -> bool {
        let Some(FrameStep::Cpu(mut step)) = self.plugin.frame_step() else { panic!("a CPU step") };
        let ctx = Ctx { vision: self.vision.clone(), masked: Cell::new(None) };
        self.number += 1;
        let mut frame = Frame::new(W, H, PixelFormat::Rgba, vec![0; (W * H * 4) as usize]);
        frame.number = self.number;
        step.process(&mut frame, &ctx);
        ctx.masked.get().expect("the effect asked for a mask")
    }
}

#[test]
fn the_card_uses_the_vision_pack_as_soon_as_it_is_downloaded() {
    for download_from in ["this card", "another card"] {
        let mut r = Rig::new();
        // Wanting people looks for the pack and finds none
        r.pick("stick");
        r.until("the pack found missing", |r| r.vision.vision().state() == MISSING && r.status().contains("download"));
        r.pick("cloak");
        assert!(r.status().contains("download"), "{download_from}: {}", r.status());
        assert!(!r.masked(), "{download_from}");
        if download_from == "this card" {
            r.pick("stick");
        }
        // What the window does when the download finishes, whichever card started it
        r.there.store(true, Ordering::SeqCst);
        r.vision.vision().pack_downloaded();
        r.host.events.clear();
        r.bus.emit(Event::VisionDownloadFinished { error: String::new(), switch_on: String::new() });
        r.until("the note gone", |r| !r.status().contains("download"));
        r.pick("cloak");
        r.until("a mask", |r| r.masked());
        assert_eq!(VisionService::state(&r.vision), READY, "{download_from}");
        r.pump();
        assert_eq!(r.status(), "", "{download_from}: nothing to say once it's there");
        r.vision.vision().shutdown();
    }
}
