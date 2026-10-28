//! Virtual cameras behind core's Outputs trait. v4l2loopback on Linux; the Windows camera and UnityCapture aren't written yet.

#[cfg(target_os = "linux")]
pub mod v4l2;

#[cfg(target_os = "linux")]
use telescope_core::frame::Frame;
use telescope_core::frame::PixelFormat;
use telescope_core::output::{Outputs, Slot, VideoOutput};

/// The label the Linux setup gives slot 0's v4l2loopback device.
pub const SLOT0_LABEL: &str = "Phone Camera";

/// The virtual cameras on this computer. device: use this one as slot 0 instead of looking it up by label.
#[derive(Default)]
pub struct VirtualCameras {
    pub device: Option<String>,
}

#[cfg(target_os = "linux")]
struct V4l2(v4l2::VirtualCamera);

#[cfg(target_os = "linux")]
impl VideoOutput for V4l2 {
    fn send(&mut self, frame: &Frame) -> Result<(), String> {
        if frame.format != PixelFormat::I420 || (frame.width, frame.height) != (self.0.width, self.0.height) {
            return Err(format!("{} takes I420 at {}x{}", self.0.device, self.0.width, self.0.height));
        }
        self.0.send(&frame.data).map_err(|e| format!("{}: {e}", self.0.device))
    }

    fn size(&self) -> (u32, u32) {
        (self.0.width, self.0.height)
    }

    fn device(&self) -> String {
        self.0.device.clone()
    }
}

impl Outputs for VirtualCameras {
    fn slots(&self) -> Vec<Slot> {
        #[cfg(target_os = "linux")]
        let device = self.device.clone().or_else(|| v4l2::find_by_label(SLOT0_LABEL));
        #[cfg(not(target_os = "linux"))]
        let device: Option<String> = None;
        device.map(|device| Slot { slot: 0, name: "Telescope".into(), device }).into_iter().collect()
    }

    fn formats(&self) -> Vec<PixelFormat> {
        vec![PixelFormat::I420]
    }

    fn open(&self, slot: u32, width: u32, height: u32, fps: u32) -> Result<Box<dyn VideoOutput>, String> {
        let Some(s) = self.slots().into_iter().find(|s| s.slot == slot) else {
            return Err("No v4l2loopback camera is set up".into());
        };
        #[cfg(target_os = "linux")]
        return v4l2::VirtualCamera::open(&s.device, width, height, fps)
            .map(|c| Box::new(V4l2(c)) as Box<dyn VideoOutput>)
            .map_err(|e| format!("{}: {e}", s.device));
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (s, width, height, fps);
            Err("Virtual cameras on this system aren't in this build yet".into())
        }
    }
}
