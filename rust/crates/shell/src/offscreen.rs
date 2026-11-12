//! Drawing windows without putting anything on screen, for the scene renders and the UI tests: Slint's software
//! renderer behind a platform of our own.

use slint::platform::software_renderer::{MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType};
use slint::platform::{EventLoopProxy, Platform, WindowAdapter, WindowEvent};
use slint::{EventLoopError, PlatformError};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

type Job = Box<dyn FnOnce() + Send>;

/// What other threads handed slint::invoke_from_event_loop, run by pump().
static JOBS: Mutex<Vec<Job>> = Mutex::new(Vec::new());
/// Slint keeps one proxy per process, and a second one would keep the next thread from getting a platform at all.
static PROXY_TAKEN: AtomicBool = AtomicBool::new(false);
/// How far skip() has moved the clock on, in milliseconds.
static SKIPPED_MS: AtomicU64 = AtomicU64::new(0);

/// While set, the clock stops following real time and only skip() moves it on, in nanoseconds since start.
static FROZEN_AT: Mutex<Option<Duration>> = Mutex::new(None);

thread_local! {
    static WINDOWS: std::cell::RefCell<Vec<Rc<MinimalSoftwareWindow>>> = const { std::cell::RefCell::new(Vec::new()) };
}

thread_local! {
    static CLIPBOARD: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

struct Offscreen;

impl Platform for Offscreen {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        WINDOWS.with(|w| w.borrow_mut().push(window.clone()));
        Ok(window)
    }

    fn new_event_loop_proxy(&self) -> Option<Box<dyn EventLoopProxy>> {
        (!PROXY_TAKEN.swap(true, Ordering::SeqCst)).then(|| Box::new(Proxy) as Box<dyn EventLoopProxy>)
    }

    fn duration_since_start(&self) -> Duration {
        static START: Mutex<Option<Instant>> = Mutex::new(None);
        let real = START.lock().unwrap().get_or_insert_with(Instant::now).elapsed();
        let elapsed = FROZEN_AT.lock().unwrap().unwrap_or(real);
        elapsed + Duration::from_millis(SKIPPED_MS.load(Ordering::SeqCst))
    }

    fn set_clipboard_text(&self, text: &str, clipboard: slint::platform::Clipboard) {
        if clipboard == slint::platform::Clipboard::DefaultClipboard {
            CLIPBOARD.with(|c| *c.borrow_mut() = Some(text.to_string()));
        }
    }

    fn clipboard_text(&self, clipboard: slint::platform::Clipboard) -> Option<String> {
        (clipboard == slint::platform::Clipboard::DefaultClipboard).then(|| CLIPBOARD.with(|c| c.borrow().clone())).flatten()
    }
}

struct Proxy;

impl EventLoopProxy for Proxy {
    fn quit_event_loop(&self) -> Result<(), EventLoopError> {
        Ok(())
    }

    fn invoke_from_event_loop(&self, event: Job) -> Result<(), EventLoopError> {
        JOBS.lock().unwrap().push(event);
        Ok(())
    }
}

/// Runs what other threads sent to the event loop since the last pump, and the timers that are due; whether anything ran.
pub fn pump() -> bool {
    let jobs = std::mem::take(&mut *JOBS.lock().unwrap());
    let ran = !jobs.is_empty();
    for job in jobs {
        job();
    }
    slint::platform::update_timers_and_animations();
    ran
}

/// Stops real time from moving the clock until the guard is dropped, so a test can check a timer's edges exactly.
pub fn freeze_clock() -> FrozenClock {
    let now = Platform::duration_since_start(&Offscreen);
    *FROZEN_AT.lock().unwrap() = Some(now - Duration::from_millis(SKIPPED_MS.load(Ordering::SeqCst)));
    FrozenClock
}

pub struct FrozenClock;

impl Drop for FrozenClock {
    fn drop(&mut self) {
        let mut frozen = FROZEN_AT.lock().unwrap();
        *frozen = None;
    }
}

/// Moves the clock timers go by on, and runs the ones that came due.
pub fn skip(time: Duration) {
    SKIPPED_MS.fetch_add(time.as_millis() as u64, Ordering::SeqCst);
    pump();
}

/// Forgets what other threads sent that no pump ran, so a test doesn't get the last one's.
pub fn drop_jobs() {
    JOBS.lock().unwrap().clear();
}

/// Makes every window this thread creates from now on an offscreen one. Call before the first window; false when a
/// platform was already set.
pub fn install() -> bool {
    slint::platform::set_platform(Box::new(Offscreen)).is_ok()
}

/// RGBA pixels of a drawn window.
pub struct Picture {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl Picture {
    pub fn save_png(&self, path: &std::path::Path) -> Result<(), String> {
        let file = std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut enc = png::Encoder::new(std::io::BufWriter::new(file), self.width, self.height);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut w = enc.write_header().map_err(|e| e.to_string())?;
        w.write_image_data(&self.rgba).map_err(|e| e.to_string())
    }
}

/// One frame of the most recently created window at the size it has, as the window would paint right after an event, with no
/// timers or changed handlers run first (draw lets them settle).
pub fn frame(width: f32, height: f32, scale: f32) -> Option<Picture> {
    let adapter = WINDOWS.with(|w| w.borrow().last().cloned())?;
    let (pw, ph) = ((width * scale).round() as u32, (height * scale).round() as u32);
    let mut buffer = vec![PremultipliedRgbaColor::default(); (pw * ph) as usize];
    adapter.request_redraw();
    adapter.draw_if_needed(|r| {
        r.render(&mut buffer, pw as usize);
    });
    let mut rgba = Vec::with_capacity(buffer.len() * 4);
    for px in buffer {
        rgba.extend_from_slice(&[px.red, px.green, px.blue, 255]);
    }
    Some(Picture { width: pw, height: ph, rgba })
}

/// Draws `window` if it asked to be drawn again since it was last drawn (a property it paints changed, an animation ran), and
/// says whether it had. A window that has never been drawn asks once.
pub fn redraw(window: &slint::Window) -> bool {
    let Some(adapter) = WINDOWS.with(|w| w.borrow().iter().find(|a| std::ptr::eq(a.window(), window)).cloned()) else { return false };
    let size = adapter.size();
    let mut buffer = vec![PremultipliedRgbaColor::default(); (size.width * size.height) as usize];
    adapter.draw_if_needed(|r| {
        r.render(&mut buffer, size.width as usize);
    })
}

/// Draws the most recently created window at width x height logical pixels and scale.
pub fn draw(window: &slint::Window, width: f32, height: f32, scale: f32) -> Option<Picture> {
    let adapter = WINDOWS.with(|w| w.borrow().last().cloned())?;
    window.dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor: scale });
    let (pw, ph) = ((width * scale).round() as u32, (height * scale).round() as u32);
    window.set_size(slint::PhysicalSize::new(pw, ph));
    // Let timers and changed handlers run before drawing (the column scrollbars settle through one)
    for _ in 0..3 {
        slint::platform::update_timers_and_animations();
        adapter.request_redraw();
        let mut scratch = vec![PremultipliedRgbaColor::default(); (pw * ph) as usize];
        adapter.draw_if_needed(|r| {
            r.render(&mut scratch, pw as usize);
        });
    }
    let mut buffer = vec![PremultipliedRgbaColor::default(); (pw * ph) as usize];
    adapter.request_redraw();
    adapter.draw_if_needed(|r| {
        r.render(&mut buffer, pw as usize);
    });
    let mut rgba = Vec::with_capacity(buffer.len() * 4);
    for px in buffer {
        rgba.extend_from_slice(&[px.red, px.green, px.blue, 255]);
    }
    Some(Picture { width: pw, height: ph, rgba })
}
