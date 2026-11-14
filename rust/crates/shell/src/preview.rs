//! The preview card's picture: the focused stream's newest frame, drawn into a texture on the window's own graphics
//! card when Slint renders with wgpu (the painter comes from the app's wiring, so core never names wgpu), or scaled on
//! the CPU otherwise.

pub use slint::wgpu_30::wgpu;
use std::sync::{Arc, OnceLock};
use telescope_core::media::Scaler;
use telescope_core::Frame;

/// Draws frames for the preview at the size asked for.
pub trait Painter {
    /// The picture, or None when it can't be shown (yet).
    fn paint(&mut self, frame: &Frame, width: u32, height: u32) -> Option<slint::Image>;

    /// A finished picture is waiting to be taken in, so the window paints again whatever the cadence says.
    fn ready(&self) -> bool {
        false
    }

    /// Takes in a finished picture of this size without asking for any new one (the paint cadence says no new job yet).
    fn paint_ready(&mut self, _width: u32, _height: u32) -> Option<slint::Image> {
        None
    }

    /// A different stream: what was made for the last one isn't shown.
    fn restart(&mut self) {}

    /// What the frames of the next paints are derived from besides their own data (Colour Compare's line and its before picture),
    /// so a frame made again from the same source is still the same job.
    fn derived_from(&mut self, _tag: u64) {}
}

/// Draws frames into textures on the device it was made on (media's scaler, through the app's wiring).
pub trait TexturePainter {
    fn paint(&mut self, frame: &Frame, width: u32, height: u32) -> Option<wgpu::Texture>;
}

/// Makes a texture painter on the device and queue Slint renders with.
pub type GpuPainterFactory = Arc<dyn Fn(wgpu::Device, wgpu::Queue) -> Box<dyn TexturePainter> + Send + Sync>;

/// A texture painter's textures, handed to Slint as they are.
pub(crate) struct GpuPainter(pub Box<dyn TexturePainter>);

impl Painter for GpuPainter {
    fn paint(&mut self, frame: &Frame, width: u32, height: u32) -> Option<slint::Image> {
        slint::Image::try_from(self.0.paint(frame, width, height)?).map_err(|e| log::warn!("Slint didn't take the preview's texture: {e}")).ok()
    }
}

static GPU_PAINTER: OnceLock<GpuPainterFactory> = OnceLock::new();

/// The binary hands over how to paint on the window's card before the window opens.
pub fn use_gpu_painter(factory: GpuPainterFactory) {
    let _ = GPU_PAINTER.set(factory);
}

/// Opens a graphics card for Slint's wgpu renderer when there's a GPU painter to share it with, and makes the painter on
/// the same device: every window (the pop-out too) then renders with it, so their textures go anywhere. None keeps the
/// default renderer (no painter, TELESCOPE_NO_GPU set, or no card wgpu can use).
pub(crate) fn select_wgpu_backend() -> Option<Box<dyn Painter>> {
    let factory = GPU_PAINTER.get()?;
    if std::env::var_os("TELESCOPE_NO_GPU").is_some() {
        return None;
    }
    let opened = (|| {
        // Not GL: looking for it loads Mesa's big GL libraries, about 25 MB, and a GL adapter found here can't present to the window
        let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
        desc.backends = wgpu::Backends::PRIMARY;
        let instance = wgpu::Instance::new(desc.with_env());
        let adapter = block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            // None keeps the OS order (Windows per-app choice, Mesa default); WGPU_POWER_PREF still overrides
            power_preference: wgpu::PowerPreference::from_env().unwrap_or_default(),
            ..Default::default()
        })).map_err(|e| e.to_string())?;
        // WGPU_BACKEND can still pick it
        if adapter.get_info().backend == wgpu::Backend::Gl {
            return Err("GL can't draw this window".to_string());
        }
        let (device, queue) = block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("telescope window"),
            required_limits: adapter.limits(),
            ..Default::default()
        })).map_err(|e| e.to_string())?;
        let config = slint::wgpu_30::WGPUConfiguration::Manual { instance, adapter, device: device.clone(), queue: queue.clone() };
        crate::parent::selector().require_wgpu_30(config).select().map_err(|e| e.to_string())?;
        Ok::<_, String>((device, queue))
    })();
    match opened {
        Ok((device, queue)) => {
            log::info!("The preview paints on the window's graphics card");
            Some(Box::new(GpuPainter(factory(device, queue))))
        }
        Err(e) => {
            log::warn!("The window can't render with wgpu, so the preview is scaled on the CPU: {e}");
            None
        }
    }
}

/// wgpu's requests finish at once on desktop drivers; this waits for one without an async runtime.
fn block_on<F: std::future::Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    loop {
        if let std::task::Poll::Ready(out) = future.as_mut().poll(&mut cx) {
            return out;
        }
        std::thread::yield_now();
    }
}

/// The CPU fallback: media's scaler to RGB, straight into the buffer Slint uploads from.
pub struct CpuPainter(pub Box<dyn Scaler>);

impl Painter for CpuPainter {
    fn paint(&mut self, frame: &Frame, width: u32, height: u32) -> Option<slint::Image> {
        let mut buffer = slint::SharedPixelBuffer::<slint::Rgb8Pixel>::new(width, height);
        self.0.rgb_into(frame, width, height, buffer.make_mut_bytes()).then(|| slint::Image::from_rgb8(buffer))
    }
}

/// Which picture a threaded painter has: the frame's number, the size and what the frame is derived from.
type PaintKey = (u64, u32, u32, u64);

struct PaintJob {
    frame: Frame,
    key: PaintKey,
    epoch: u64,
}

type Made = (PaintKey, slint::SharedPixelBuffer<slint::Rgb8Pixel>);

/// The CPU fallback off the window's thread: the scaler (which may wait on a graphics card's readback) runs on a thread of its own, and the
/// window gets the picture when it's made (`wake` asks it to paint again). The newest picture made for the stream at the size asked for is
/// shown even when a newer frame has come since, so a scaler slower than the frames still puts pictures up at its own pace; a newer frame
/// replaces one still waiting to be made.
pub struct ThreadedPainter {
    scaler: Option<Box<dyn Scaler>>,
    wake: Arc<dyn Fn() + Send + Sync>,
    worker: Option<crate::preview_passes::Worker<PaintJob>>,
    made: Arc<std::sync::Mutex<Option<Made>>>,
    asked: Option<PaintKey>,
    tag: u64,
    // Counts streams, so a picture still being made for the last one is dropped
    epoch: Arc<std::sync::atomic::AtomicU64>,
}

impl ThreadedPainter {
    /// Wakes the window's own event loop when a picture is made.
    pub fn for_window(scaler: Box<dyn Scaler>) -> ThreadedPainter {
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {
            let _ = slint::invoke_from_event_loop(|| crate::window::with_window(|w| w.frame_ready()));
        });
        ThreadedPainter::new(scaler, wake)
    }

    /// The thread starts with the first frame to paint.
    pub fn new(scaler: Box<dyn Scaler>, wake: Arc<dyn Fn() + Send + Sync>) -> ThreadedPainter {
        ThreadedPainter { scaler: Some(scaler), wake, worker: None, made: Arc::default(), asked: None, tag: 0, epoch: Arc::default() }
    }

    fn ask(&mut self, frame: &Frame, key: PaintKey) {
        if self.worker.is_none() {
            let Some(mut scaler) = self.scaler.take() else { return };
            let (made, wake, epoch) = (self.made.clone(), self.wake.clone(), self.epoch.clone());
            self.worker = Some(crate::preview_passes::Worker::start("preview-paint", move |job: PaintJob| {
                let (width, height) = (job.key.1, job.key.2);
                let mut buffer = slint::SharedPixelBuffer::<slint::Rgb8Pixel>::new(width, height);
                if scaler.rgb_into(&job.frame, width, height, buffer.make_mut_bytes()) {
                    let mut slot = made.lock().unwrap_or_else(|e| e.into_inner());
                    if job.epoch == epoch.load(std::sync::atomic::Ordering::SeqCst) {
                        *slot = Some((job.key, buffer));
                        drop(slot);
                        wake();
                    }
                }
            }));
        }
        self.asked = Some(key);
        if let Some(worker) = &self.worker {
            worker.put(PaintJob { frame: frame.clone(), key, epoch: self.epoch.load(std::sync::atomic::Ordering::SeqCst) });
        }
    }
}

impl Drop for ThreadedPainter {
    fn drop(&mut self) {
        // The scaler may be holding a graphics card's readback: it ends before the painter's owner goes on
        if let Some(worker) = self.worker.take() {
            worker.finish();
        }
    }
}

impl Painter for ThreadedPainter {
    fn paint(&mut self, frame: &Frame, width: u32, height: u32) -> Option<slint::Image> {
        let key = (frame.number, width, height, self.tag);
        let ready = {
            let mut made = self.made.lock().unwrap_or_else(|e| e.into_inner());
            // A picture of another size is no use for this one
            made.take_if(|(k, _)| (k.1, k.2) != (width, height));
            made.take()
        };
        if let Some((made_key, buffer)) = ready {
            if made_key == key {
                // Painted again later (a repaint of the same frame), it's made again
                self.asked = None;
            } else if self.asked != Some(key) {
                // An older frame's picture goes up while the newest is made
                self.ask(frame, key);
            }
            return Some(slint::Image::from_rgb8(buffer));
        }
        if self.asked != Some(key) {
            self.ask(frame, key);
        }
        None
    }

    fn paint_ready(&mut self, width: u32, height: u32) -> Option<slint::Image> {
        let (key, buffer) = {
            let mut made = self.made.lock().unwrap_or_else(|e| e.into_inner());
            made.take_if(|(k, _)| (k.1, k.2) != (width, height));
            made.take()?
        };
        // What was asked for has come, so painting that frame again makes it again
        if self.asked == Some(key) {
            self.asked = None;
        }
        Some(slint::Image::from_rgb8(buffer))
    }

    fn ready(&self) -> bool {
        self.made.lock().unwrap_or_else(|e| e.into_inner()).is_some()
    }

    fn restart(&mut self) {
        let mut made = self.made.lock().unwrap_or_else(|e| e.into_inner());
        self.epoch.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        *made = None;
        self.asked = None;
    }

    fn derived_from(&mut self, tag: u64) {
        self.tag = tag;
    }
}

/// What a painter's `derived_from` is given: what the picture besides the frame's own data comes from (Colour Compare) and the slot's
/// count, since a stalled stream's frame replaced in place keeps its own number but is another picture.
pub fn paint_tag(derived: u64, revision: u64) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    (derived, revision).hash(&mut hasher);
    hasher.finish()
}

/// What one look at the preview does about its painter.
#[derive(Debug, PartialEq)]
pub enum Step {
    /// Nothing to paint now.
    Skip,
    /// Only a picture that has finished goes up; no new one is asked for.
    Take,
    /// Paint the frame, asking for its picture when it isn't made yet.
    Paint,
}

/// `due` is the paint cadence (or the same frame again), `ready` a finished picture waiting, `tints` a pass painting the picture itself.
pub fn step(due: bool, ready: bool, tints: bool) -> Step {
    match (due, ready) {
        (true, _) => Step::Paint,
        (false, false) => Step::Skip,
        // A tinting pass paints the picture itself, so a fallback picture waiting is of no use
        (false, true) if tints => Step::Skip,
        (false, true) => Step::Take,
    }
}

/// Whether a pass asked for the overlay to be drawn again and it hasn't been yet: the picture it goes on may land later than the ask.
#[derive(Default)]
pub struct OverlayAsk(std::cell::Cell<bool>);

impl OverlayAsk {
    /// A look at the preview: `asked` is a new ask since the last. Returns whether one is still waiting.
    pub fn look(&self, asked: bool) -> bool {
        if asked {
            self.0.set(true);
        }
        self.0.get()
    }

    /// The overlay was drawn.
    pub fn drawn(&self) {
        self.0.set(false);
    }
}

/// An RGBA picture width x height as the RGB image Slint shows, filled in one pass.
pub fn rgb_image(rgba: &[u8], width: u32, height: u32) -> slint::Image {
    let mut buffer = slint::SharedPixelBuffer::<slint::Rgb8Pixel>::new(width, height);
    telescope_core::image::drop_alpha(rgba, width as usize, buffer.make_mut_bytes());
    slint::Image::from_rgb8(buffer)
}

/// _fit_within: the frame's size scaled down to fit inside the stage, keeping its shape, never up.
pub fn fit_within(frame: (u32, u32), stage: (u32, u32)) -> (u32, u32) {
    let (w, h) = (frame.0.max(1) as f64, frame.1.max(1) as f64);
    let scale = (stage.0 as f64 / w).min(stage.1 as f64 / h).min(1.0);
    (((w * scale).round() as u32).max(1), ((h * scale).round() as u32).max(1))
}

/// Where the picture lands on the stage, in px: fit_within without the cap, since a small frame is drawn bigger. Overlays are
/// painted at this size, so their lines and text stay the same on any stream (the rect PreviewLayer.paint gets).
pub fn shown_size(frame: (u32, u32), stage: (u32, u32)) -> (u32, u32) {
    let (w, h) = (frame.0.max(1) as f64, frame.1.max(1) as f64);
    let scale = (stage.0 as f64 / w).min(stage.1 as f64 / h);
    (((w * scale).round() as u32).max(1), ((h * scale).round() as u32).max(1))
}

/// process_frame's gate: the pop-out always shows; the card only while it's shown in a window someone can see.
pub fn wants_frames(popout: bool, card: bool, window_seen: bool) -> bool {
    popout || (card && window_seen)
}

/// Which of a stream's new frames the window's card paints: every second one, counted in frames so the gaps stay even at any
/// rate, or all of them.
#[derive(Default)]
pub struct Cadence {
    seen: u64,
    skip_next: bool,
}

impl Cadence {
    /// Whether to paint the newest frame. `always` is for a frame that has to be shown whatever the count, or all of them.
    pub fn due(&mut self, number: u64, every_frame: bool, always: bool) -> bool {
        if every_frame || always {
            self.seen = number;
            self.skip_next = true;
            return true;
        }
        if number == self.seen {
            return false;
        }
        self.seen = number;
        self.skip_next = !self.skip_next;
        self.skip_next
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Frames as a timer that looks several times sees them: each new one a few times
    fn painted(cadence: &mut Cadence, frames: std::ops::Range<u64>, every_frame: bool) -> Vec<u64> {
        frames.filter(|&n| (0..3).map(|_| cadence.due(n, every_frame, false)).filter(|d| *d).count() > 0).collect()
    }

    #[test]
    fn the_card_paints_every_second_frame_however_often_it_looks() {
        let mut c = Cadence::default();
        assert!(c.due(1, false, true), "a new stream's first frame shows at once");
        assert_eq!(painted(&mut c, 2..10, false), [3, 5, 7, 9]);
    }

    #[test]
    fn numbers_needn_t_be_neighbours() {
        // Frame numbers are shared by all the streams
        let mut c = Cadence::default();
        let got: Vec<u64> = [10, 13, 14, 20, 21, 30].into_iter().filter(|&n| c.due(n, false, false)).collect();
        assert_eq!(got.len(), 3, "{got:?}");
    }

    #[test]
    fn smooth_paints_every_frame_and_going_back_keeps_the_gaps_even() {
        let mut c = Cadence::default();
        assert_eq!(painted(&mut c, 1..5, true), [1, 2, 3, 4]);
        assert_eq!(painted(&mut c, 5..9, false), [6, 8]);
    }

    #[test]
    fn a_frame_that_has_to_show_doesn_t_upset_the_count() {
        let mut c = Cadence::default();
        assert_eq!(painted(&mut c, 1..4, false), [1, 3]);
        assert!(c.due(4, false, true));
        assert_eq!(painted(&mut c, 5..8, false), [6]);
    }

    #[test]
    fn a_small_stream_in_a_big_window_is_shown_at_the_windows_size() {
        assert_eq!(shown_size((640, 360), (1280, 720)), (1280, 720));
        assert_eq!(shown_size((1920, 1080), (640, 640)), (640, 360));
        assert_eq!(shown_size((640, 360), (0, 0)), (1, 1));
    }

    #[test]
    fn a_window_hidden_in_the_tray_paints_nothing_but_the_pop_out_goes_on() {
        assert!(wants_frames(false, true, true));
        assert!(!wants_frames(false, true, false), "nothing to see, so a pass's painted_at must not move");
        assert!(wants_frames(true, true, false));
        assert!(!wants_frames(false, false, true));
    }

    /// A scaler that paints the frame's first byte, after a pause, counting its runs.
    struct Flat(std::time::Duration, Arc<std::sync::atomic::AtomicUsize>);

    impl Scaler for Flat {
        fn rgb(&mut self, frame: &Frame, width: u32, height: u32) -> Vec<u8> {
            vec![frame.data[0]; (width * height * 3) as usize]
        }
        fn rgb_into(&mut self, frame: &Frame, width: u32, height: u32, out: &mut [u8]) -> bool {
            std::thread::sleep(self.0);
            self.1.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let rgb = self.rgb(frame, width, height);
            out.copy_from_slice(&rgb);
            true
        }
    }

    fn grey(level: u8, number: u64) -> Frame {
        let mut f = Frame::new(4, 2, telescope_core::PixelFormat::I420, vec![level; 12]);
        f.number = number;
        f
    }

    fn until(mut done: impl FnMut() -> bool) {
        let limit = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !done() {
            assert!(std::time::Instant::now() < limit, "the painter's thread never finished");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    fn flat(ms: u64) -> (Box<dyn Scaler>, Arc<std::sync::atomic::AtomicUsize>) {
        let runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        (Box::new(Flat(std::time::Duration::from_millis(ms), runs.clone())), runs)
    }

    #[test]
    fn the_threaded_painter_never_makes_the_caller_wait_and_hands_back_the_picture_for_the_same_frame() {
        let woke = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = woke.clone();
        let mut painter = ThreadedPainter::new(flat(50).0, Arc::new(move || {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }));
        let frame = grey(90, 7);
        let began = std::time::Instant::now();
        assert!(painter.paint(&frame, 2, 2).is_none(), "nothing yet for a frame it hasn't seen");
        assert!(began.elapsed() < std::time::Duration::from_millis(40), "the caller waited for the scaler");
        assert!(!painter.ready());
        assert!(painter.paint(&frame, 2, 2).is_none(), "still being made, and asked for once only");
        until(|| woke.load(std::sync::atomic::Ordering::SeqCst) == 1);
        assert!(painter.ready());
        let image = painter.paint(&frame, 2, 2).expect("the picture for the frame that asked");
        assert_eq!(image.to_rgb8().unwrap().as_slice()[0].r, 90);
        assert!(!painter.ready());
        // The same frame at another size is made afresh
        assert!(painter.paint(&frame, 4, 2).is_none());
        until(|| woke.load(std::sync::atomic::Ordering::SeqCst) == 2);
        assert_eq!(painter.paint(&frame, 4, 2).unwrap().to_rgb8().unwrap().width(), 4);
    }

    #[test]
    fn a_scaler_slower_than_the_frames_still_puts_pictures_up_at_a_steady_rate() {
        let (scaler, runs) = flat(40);
        let mut painter = ThreadedPainter::new(scaler, Arc::new(|| {}));
        let began = std::time::Instant::now();
        let (mut shown, mut times) = (Vec::new(), Vec::new());
        let mut number = 0;
        // 100 fps against a scaler that takes 40 ms: a picture is never for the newest frame
        while began.elapsed() < std::time::Duration::from_millis(800) {
            number += 1;
            if let Some(image) = painter.paint(&grey(number as u8, number), 2, 2) {
                shown.push(image.to_rgb8().unwrap().as_slice()[0].r as u64);
                times.push(began.elapsed().as_millis());
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(shown.len() >= 10, "{} pictures in 800 ms", shown.len());
        assert!(shown.windows(2).all(|w| w[0] < w[1]), "pictures went back: {shown:?}");
        let longest = times.windows(2).map(|t| t[1] - t[0]).max().unwrap();
        assert!(longest < 200, "a gap of {longest} ms between pictures");
        assert!(runs.load(std::sync::atomic::Ordering::SeqCst) >= shown.len());
    }

    #[test]
    fn a_frame_made_again_from_the_same_source_is_scaled_once() {
        // Colour Compare gives each retry of a stalled stream's frame a new copy of its data
        let (scaler, runs) = flat(30);
        let woke = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = woke.clone();
        let mut painter = ThreadedPainter::new(scaler, Arc::new(move || {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }));
        painter.derived_from(5);
        assert!(painter.paint(&grey(60, 3), 2, 2).is_none());
        for _ in 0..5 {
            assert!(painter.paint(&grey(60, 3), 2, 2).is_none());
        }
        until(|| woke.load(std::sync::atomic::Ordering::SeqCst) == 1);
        assert!(painter.paint(&grey(60, 3), 2, 2).is_some(), "the picture lands");
        assert!(!painter.ready());
        assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), 1);
        // The Compare line moved: the same frame is a new job
        painter.derived_from(6);
        assert!(painter.paint(&grey(60, 3), 2, 2).is_none());
        until(|| woke.load(std::sync::atomic::Ordering::SeqCst) == 2);
        assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[test]
    fn a_new_stream_doesn_t_show_the_last_ones_picture() {
        let woke = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = woke.clone();
        let mut painter = ThreadedPainter::new(flat(10).0, Arc::new(move || {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }));
        assert!(painter.paint(&grey(90, 7), 2, 2).is_none());
        until(|| woke.load(std::sync::atomic::Ordering::SeqCst) == 1);
        painter.restart();
        assert!(!painter.ready());
        assert!(painter.paint(&grey(200, 7), 2, 2).is_none());
        until(|| woke.load(std::sync::atomic::Ordering::SeqCst) == 2);
        assert_eq!(painter.paint(&grey(200, 7), 2, 2).unwrap().to_rgb8().unwrap().as_slice()[0].r, 200);
    }

    #[test]
    fn a_frame_replaced_in_place_is_another_picture() {
        assert_ne!(paint_tag(5, 10), paint_tag(5, 11));
        assert_eq!(paint_tag(5, 10), paint_tag(5, 10));
        let (scaler, runs) = flat(5);
        let woke = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = woke.clone();
        let mut painter = ThreadedPainter::new(scaler, Arc::new(move || {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }));
        // The stream stalled on frame 3; its composite later replaces the pixels under the same number
        painter.derived_from(paint_tag(0, 10));
        assert!(painter.paint(&grey(60, 3), 2, 2).is_none());
        until(|| woke.load(std::sync::atomic::Ordering::SeqCst) == 1);
        assert_eq!(painter.paint(&grey(60, 3), 2, 2).unwrap().to_rgb8().unwrap().as_slice()[0].r, 60);
        painter.derived_from(paint_tag(0, 11));
        assert!(painter.paint(&grey(200, 3), 2, 2).is_none());
        until(|| woke.load(std::sync::atomic::Ordering::SeqCst) == 2);
        assert_eq!(painter.paint(&grey(200, 3), 2, 2).unwrap().to_rgb8().unwrap().as_slice()[0].r, 200);
        assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[test]
    fn a_finished_picture_goes_up_without_asking_for_the_next() {
        assert_eq!(step(false, true, false), Step::Take);
        assert_eq!(step(false, false, false), Step::Skip);
        assert_eq!(step(true, true, false), Step::Paint);
        assert_eq!(step(false, true, true), Step::Skip, "a tinting pass paints the picture itself, so no repaints for a picture it never takes");
        let (scaler, runs) = flat(5);
        let woke = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = woke.clone();
        let mut painter = ThreadedPainter::new(scaler, Arc::new(move || {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }));
        assert!(painter.paint(&grey(60, 1), 2, 2).is_none());
        until(|| woke.load(std::sync::atomic::Ordering::SeqCst) == 1);
        assert!(painter.paint(&grey(60, 1), 2, 2).is_some());
        assert!(painter.paint(&grey(70, 2), 2, 2).is_none());
        until(|| woke.load(std::sync::atomic::Ordering::SeqCst) == 2);
        assert!(painter.ready());
        // A picture of another size isn't taken
        assert!(painter.paint_ready(4, 2).is_none());
        assert!(!painter.ready());
        assert!(painter.paint(&grey(80, 3), 2, 2).is_none());
        until(|| woke.load(std::sync::atomic::Ordering::SeqCst) == 3);
        assert!(painter.paint_ready(2, 2).is_some());
        // Newer frames came, but the cadence said no: nothing is made
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(!painter.ready());
        assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), 3);
    }

    #[test]
    fn an_overlay_ask_waits_for_the_picture_it_goes_on() {
        let ask = OverlayAsk::default();
        assert!(!ask.look(false));
        assert!(ask.look(true));
        assert!(ask.look(false), "the picture hasn't landed, so the ask is still there");
        ask.drawn();
        assert!(!ask.look(false));
    }

    #[test]
    fn dropping_the_painter_waits_for_the_scaler_in_hand() {
        let (scaler, runs) = flat(150);
        let mut painter = ThreadedPainter::new(scaler, Arc::new(|| {}));
        assert!(painter.paint(&grey(60, 1), 2, 2).is_none());
        std::thread::sleep(std::time::Duration::from_millis(30));
        drop(painter);
        assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), 1, "the scaler was still running when the painter went");
    }

    #[test]
    fn frames_shrink_to_the_stage_but_never_grow() {
        assert_eq!(fit_within((1920, 1080), (640, 640)), (640, 360));
        assert_eq!(fit_within((1920, 1080), (400, 100)), (178, 100));
        assert_eq!(fit_within((640, 360), (1280, 720)), (640, 360));
        assert_eq!(fit_within((640, 360), (0, 0)), (1, 1));
    }
}
