//! Virtual cameras: v4l2loopback slots on Linux, the Windows 11 camera's shared memory and UnityCapture. The outputs crate implements these.

use crate::frame::{Frame, PixelFormat};
use serde::{Deserialize, Serialize};

/// One virtual camera that's set up. Slot 0 is the one apps list as Telescope.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Slot {
    pub slot: u32,
    /// As apps list it ("Telescope", "Telescope 2").
    pub name: String,
    /// Where it lives (/dev/video0, Global\TelescopeCamera0).
    pub device: String,
}

/// An open virtual camera taking frames of one size.
pub trait VideoOutput: Send {
    /// The frame has to be the size it was opened at, in a format from formats().
    fn send(&mut self, frame: &Frame) -> Result<(), String>;
    fn size(&self) -> (u32, u32);
    fn device(&self) -> String;
    /// Whether an app is reading it right now, when that can be told (camera_watched).
    fn watched(&self) -> Option<bool> {
        None
    }
}

pub trait Outputs: Send + Sync {
    /// The virtual cameras that are set up now.
    fn slots(&self) -> Vec<Slot>;
    /// The formats send() takes, preferred first.
    fn formats(&self) -> Vec<PixelFormat>;
    fn open(&self, slot: u32, width: u32, height: u32, fps: u32) -> Result<Box<dyn VideoOutput>, String>;
}
