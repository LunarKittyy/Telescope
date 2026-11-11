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

    /// A picture was asked for that isn't ready, so the frame that asked is painted again whatever the cadence says.
    fn catching_up(&self) -> bool {
        false
    }
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
            power_preference: wgpu::PowerPreference::HighPerformance,
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

/// Which picture a threaded painter has: the frame (its data's address and number) and the size.
type PaintKey = (usize, u64, u32, u32);

fn paint_key(frame: &Frame, width: u32, height: u32) -> PaintKey {
    (Arc::as_ptr(&frame.data) as usize, frame.number, width, height)
}

struct PaintJob {
    frame: Frame,
    width: u32,
    height: u32,
}

/// The CPU fallback off the window's thread: the scaler (which may wait on a graphics card's readback) runs on a thread of its own, and the
/// window gets the picture when it's made (`wake` asks it to paint again). The window sees nothing for a frame the first time, then the
/// picture for that same frame; a newer frame replaces one still being made.
pub struct ThreadedPainter {
    scaler: Option<Box<dyn Scaler>>,
    wake: Arc<dyn Fn() + Send + Sync>,
    worker: Option<crate::preview_passes::Worker<PaintJob>>,
    made: Arc<std::sync::Mutex<Option<(PaintKey, slint::SharedPixelBuffer<slint::Rgb8Pixel>)>>>,
    asked: Option<PaintKey>,
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
        ThreadedPainter { scaler: Some(scaler), wake, worker: None, made: Arc::default(), asked: None }
    }
}

impl Painter for ThreadedPainter {
    fn paint(&mut self, frame: &Frame, width: u32, height: u32) -> Option<slint::Image> {
        let key = paint_key(frame, width, height);
        let ready = {
            let mut made = self.made.lock().unwrap_or_else(|e| e.into_inner());
            made.take_if(|(k, _)| *k == key)
        };
        if let Some((_, buffer)) = ready {
            // Painted again later (a repaint of the same frame), it's made again
            self.asked = None;
            return Some(slint::Image::from_rgb8(buffer));
        }
        if self.asked == Some(key) {
            return None;
        }
        if self.worker.is_none() {
            let mut scaler = self.scaler.take()?;
            let (made, wake) = (self.made.clone(), self.wake.clone());
            self.worker = Some(crate::preview_passes::Worker::start("preview-paint", move |job: PaintJob| {
                let mut buffer = slint::SharedPixelBuffer::<slint::Rgb8Pixel>::new(job.width, job.height);
                let key = paint_key(&job.frame, job.width, job.height);
                if scaler.rgb_into(&job.frame, job.width, job.height, buffer.make_mut_bytes()) {
                    *made.lock().unwrap_or_else(|e| e.into_inner()) = Some((key, buffer));
                    wake();
                }
            }));
        }
        self.asked = Some(key);
        self.worker.as_ref()?.put(PaintJob { frame: frame.clone(), width, height });
        None
    }

    fn catching_up(&self) -> bool {
        self.asked.is_some()
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

    /// A scaler that paints the frame's first byte, after a pause.
    struct Flat(std::time::Duration);

    impl Scaler for Flat {
        fn rgb(&mut self, frame: &Frame, width: u32, height: u32) -> Vec<u8> {
            vec![frame.data[0]; (width * height * 3) as usize]
        }
        fn rgb_into(&mut self, frame: &Frame, width: u32, height: u32, out: &mut [u8]) -> bool {
            std::thread::sleep(self.0);
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

    #[test]
    fn the_threaded_painter_never_makes_the_caller_wait_and_hands_back_the_picture_for_the_same_frame() {
        let woke = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = woke.clone();
        let mut painter = ThreadedPainter::new(Box::new(Flat(std::time::Duration::from_millis(50))), Arc::new(move || {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }));
        let frame = grey(90, 7);
        let began = std::time::Instant::now();
        assert!(painter.paint(&frame, 2, 2).is_none(), "nothing yet for a frame it hasn't seen");
        assert!(began.elapsed() < std::time::Duration::from_millis(40), "the caller waited for the scaler");
        assert!(painter.catching_up());
        assert!(painter.paint(&frame, 2, 2).is_none(), "still being made, and asked for once only");
        until(|| woke.load(std::sync::atomic::Ordering::SeqCst) == 1);
        let image = painter.paint(&frame, 2, 2).expect("the picture for the frame that asked");
        assert_eq!(image.to_rgb8().unwrap().as_slice()[0].r, 90);
        assert!(!painter.catching_up());
        // A later frame, or the same one at another size, is made afresh
        assert!(painter.paint(&grey(200, 8), 2, 2).is_none());
        until(|| woke.load(std::sync::atomic::Ordering::SeqCst) == 2);
        assert!(painter.paint(&frame, 2, 2).is_none(), "an older frame's picture isn't taken for the newer one");
        until(|| woke.load(std::sync::atomic::Ordering::SeqCst) == 3);
        assert_eq!(painter.paint(&frame, 2, 2).unwrap().to_rgb8().unwrap().as_slice()[0].r, 90);
    }

    #[test]
    fn frames_shrink_to_the_stage_but_never_grow() {
        assert_eq!(fit_within((1920, 1080), (640, 640)), (640, 360));
        assert_eq!(fit_within((1920, 1080), (400, 100)), (178, 100));
        assert_eq!(fit_within((640, 360), (1280, 720)), (640, 360));
        assert_eq!(fit_within((640, 360), (0, 0)), (1, 1));
    }
}
