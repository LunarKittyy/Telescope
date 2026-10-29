//! The preview card's picture: the focused stream's newest frame, drawn into a texture on the window's own graphics
//! card when Slint renders with wgpu (the painter comes from the app's wiring, so core never names wgpu), or scaled on
//! the CPU otherwise.

pub use slint::wgpu_30::wgpu;
use std::sync::{Arc, OnceLock};
use telescope_core::media::Scaler;
use telescope_core::Frame;

/// Draws frames for the preview at the size asked for.
pub trait Painter {
    fn paint(&mut self, frame: &Frame, width: u32, height: u32) -> Option<slint::Image>;
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
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle().with_env());
        let adapter = block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            ..Default::default()
        })).map_err(|e| e.to_string())?;
        let (device, queue) = block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("telescope window"),
            required_limits: adapter.limits(),
            ..Default::default()
        })).map_err(|e| e.to_string())?;
        let config = slint::wgpu_30::WGPUConfiguration::Manual { instance, adapter, device: device.clone(), queue: queue.clone() };
        slint::BackendSelector::new().require_wgpu_30(config).select().map_err(|e| e.to_string())?;
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

/// The CPU fallback: media's scaler to RGB, then Slint uploads it.
pub struct CpuPainter(pub Box<dyn Scaler>);

impl Painter for CpuPainter {
    fn paint(&mut self, frame: &Frame, width: u32, height: u32) -> Option<slint::Image> {
        let rgb = self.0.rgb(frame, width, height);
        if rgb.len() != (width * height * 3) as usize {
            return None;
        }
        let buffer = slint::SharedPixelBuffer::<slint::Rgb8Pixel>::clone_from_slice(&rgb, width, height);
        Some(slint::Image::from_rgb8(buffer))
    }
}

/// _fit_within: the frame's size scaled down to fit inside the stage, keeping its shape, never up.
pub fn fit_within(frame: (u32, u32), stage: (u32, u32)) -> (u32, u32) {
    let (w, h) = (frame.0.max(1) as f64, frame.1.max(1) as f64);
    let scale = (stage.0 as f64 / w).min(stage.1 as f64 / h).min(1.0);
    (((w * scale).round() as u32).max(1), ((h * scale).round() as u32).max(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_shrink_to_the_stage_but_never_grow() {
        assert_eq!(fit_within((1920, 1080), (640, 640)), (640, 360));
        assert_eq!(fit_within((1920, 1080), (400, 100)), (178, 100));
        assert_eq!(fit_within((640, 360), (1280, 720)), (640, 360));
        assert_eq!(fit_within((640, 360), (0, 0)), (1, 1));
    }
}
