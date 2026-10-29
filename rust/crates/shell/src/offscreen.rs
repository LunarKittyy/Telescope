//! Drawing windows without putting anything on screen, for the scene renders and the UI tests: Slint's software
//! renderer behind a platform of our own.

use slint::platform::software_renderer::{MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType};
use slint::platform::{EventLoopProxy, Platform, WindowAdapter, WindowEvent};
use slint::{EventLoopError, PlatformError};
use std::rc::Rc;
use std::sync::Mutex;

type Job = Box<dyn FnOnce() + Send>;

/// What other threads handed slint::invoke_from_event_loop, run by pump().
static JOBS: Mutex<Vec<Job>> = Mutex::new(Vec::new());

thread_local! {
    static WINDOWS: std::cell::RefCell<Vec<Rc<MinimalSoftwareWindow>>> = const { std::cell::RefCell::new(Vec::new()) };
}

struct Offscreen;

impl Platform for Offscreen {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        WINDOWS.with(|w| w.borrow_mut().push(window.clone()));
        Ok(window)
    }

    fn new_event_loop_proxy(&self) -> Option<Box<dyn EventLoopProxy>> {
        Some(Box::new(Proxy))
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

/// Makes every window this process creates from now on an offscreen one. Call before the first window; false when a
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
