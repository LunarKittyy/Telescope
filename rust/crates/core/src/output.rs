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
    /// Its slot now goes to another kind of camera (one installed or updated while it was open), so whoever holds it
    /// should open it again (vcam.camera_changed).
    fn outdated(&self) -> bool {
        false
    }
}

/// A job holding a virtual camera from a thread of its own (vcam._Held): start and stop from any thread. It lets go
/// while a Released is alive and takes the camera back after, if it's still wanted.
pub trait Held: Send + Sync {
    fn start(&self);
    fn stop(&self);
    fn running(&self) -> bool;
}

/// What the virtual camera shows while nothing streams to it (vcam.WaitScreen).
pub trait WaitScreen: Held {
    /// Hold the camera at width x height showing the image or animation at path (None: the default screen), flipped
    /// left to right if mirror; restarts only if one of them changed.
    fn show(&self, width: u32, height: u32, path: Option<&str>, mirror: bool);
    /// An app reads the camera: send often enough to look alive.
    fn set_watched(&self, watched: bool);
}

/// While alive, everything holding the virtual cameras while idle has let go (vcam.device_released), so a driver can
/// be reloaded. Dropping it gives them back.
pub struct Released(Option<Box<dyn FnOnce() + Send>>);

impl Released {
    pub fn new(resume: impl FnOnce() + Send + 'static) -> Released {
        Released(Some(Box::new(resume)))
    }
    /// Nothing to give back.
    pub fn none() -> Released {
        Released(None)
    }
}

impl Drop for Released {
    fn drop(&mut self) {
        if let Some(resume) = self.0.take() {
            resume();
        }
    }
}

/// What a root operation for the cameras came to (platform.linux.PrivResult, or Windows' install).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupResult {
    pub ok: bool,
    /// Shown as it is (already translated), "" when there's nothing to say.
    pub message: String,
    /// Set when no password prompt could be shown: the line to run in a terminal instead.
    pub command: Option<String>,
}

/// Root operations on the virtual cameras: one password prompt (or Windows' admin prompt) each. Blocking: run them
/// off the UI thread.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "do")]
pub enum SetupAction {
    /// Linux: load v4l2loopback with Telescope's settings, and with persist at every boot too.
    Load { persist: bool },
    /// Linux: unload and load it again.
    Reload,
    /// Linux: unload it.
    Unload,
    /// Linux: write or remove the files that load it at boot.
    Persist { on: bool },
    /// Set up the extra cameras that aren't ready yet (Linux adds them to the running module, Windows installs the
    /// camera with every slot).
    AddSlots,
    /// Take the extra cameras away again (Linux; Windows reinstalls with one camera).
    RemoveSlots,
    /// Windows: install the camera (the Windows 11 one where it can, else UnityCapture) with devices cameras; reset
    /// goes back to fewer.
    Install { devices: u32, reset: bool },
}

/// SetupResult::message when the person said no at the password prompt. Compared, never shown, so it stays as it is.
pub const SETUP_CANCELLED: &str = "Cancelled.";
/// Slot 0's node, as the Linux setup makes it (platform/linux.py V4L2_PHONE_DEV).
pub const V4L2_PHONE_DEV: &str = "/dev/video11";

/// How far the virtual camera is set up on this computer, for the setup checks.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupStatus {
    /// Linux: the v4l2loopback package is there (loaded or not).
    pub installed: bool,
    /// Linux: the module is loaded; Windows: a camera is registered.
    pub loaded: bool,
    /// Slot 0's camera exists, so a stream can go to it.
    pub ready: bool,
    /// Linux: Telescope's boot-time files exist.
    pub persisted: bool,
    /// Windows: a newer Windows 11 camera is ready to replace the installed one.
    pub outdated: bool,
    /// Windows: the camera apps see is the Windows 11 one.
    pub windows_camera: bool,
}

pub trait Outputs: Send + Sync {
    /// The virtual cameras that are set up now.
    fn slots(&self) -> Vec<Slot>;
    /// The formats send() takes, preferred first.
    fn formats(&self) -> Vec<PixelFormat>;
    fn open(&self, slot: u32, width: u32, height: u32, fps: u32) -> Result<Box<dyn VideoOutput>, String>;
    /// How many cameras streams can go to at once (vcam.MAX_SLOTS).
    fn max_slots(&self) -> u32 {
        4
    }
    /// What apps list slot's camera as, set up or not (vcam.slot_label).
    fn slot_label(&self, slot: u32) -> String {
        if slot == 0 { "Telescope".into() } else { format!("Telescope {}", slot + 1) }
    }
    /// Whether slot's camera is set up to stream to; the first one always is (vcam.slot_ready).
    fn slot_ready(&self, slot: u32) -> bool {
        slot == 0 || self.slots().iter().any(|s| s.slot == slot)
    }
    /// The size slot's camera is stuck at while an app still reads it, which whoever opens it next has to use
    /// (vcam.locked_size).
    fn locked_size(&self, _slot: u32) -> Option<(u32, u32)> {
        None
    }
    /// Reports whether an app reads the main camera, on a thread of its own, only when that changes
    /// (vcam.CameraWatch). Start it to begin.
    fn watch(&self, _on_change: Box<dyn Fn(bool) + Send + Sync>) -> Option<Box<dyn Held>> {
        None
    }
    /// The wait screen for slot's camera (vcam.WaitScreen). Not running until shown.
    fn wait_screen(&self, _slot: u32) -> Option<Box<dyn WaitScreen>> {
        None
    }
    /// The image or animation at path as the wait screen shows it, first frame, RGBA at width x height; None if it
    /// won't load. path None: the default screen.
    fn wait_picture(&self, _path: Option<&str>, _width: u32, _height: u32) -> Option<Frame> {
        None
    }
    /// Everything holding the cameras while idle lets go until the Released is dropped (vcam.device_released).
    fn release_devices(&self) -> Released {
        Released::none()
    }
    /// How far the camera is set up. Can be slow (it may run lsmod or read the registry): not on the UI thread.
    fn setup_status(&self) -> SetupStatus {
        SetupStatus { ready: !self.slots().is_empty(), ..SetupStatus::default() }
    }
    /// Runs a root operation behind one password or admin prompt. Blocking.
    fn set_up(&self, _action: SetupAction, _progress: &dyn Fn(&str)) -> SetupResult {
        SetupResult { ok: false, message: "Not in this build yet".into(), command: None }
    }
}
