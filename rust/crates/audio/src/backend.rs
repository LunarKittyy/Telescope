//! What the Microphone plugin needs from this computer (microphone.py's LinuxMic and WindowsMic): whether the
//! virtual mic can exist here, setting it up and removing it, and the output the worker writes to.

use telescope_core::i18n::Fields;
use crate::virtual_mic;
use crate::worker::OpenSink;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use telescope_core::i18n::tr;

/// A problem's action that runs the backend's install() instead of opening a link.
pub const INSTALL: &str = "install";

/// (text, (button label, url or INSTALL)) when this computer can't have the mic.
pub type Problem = (String, Option<(String, String)>);

pub trait MicBackend: Send + Sync {
    /// What apps list the mic as.
    fn pick_name(&self) -> String;
    fn problem(&self) -> Option<Problem>;
    /// Sets the virtual mic up; "" when ready, else why not. Blocking.
    fn prepare(&self) -> String;
    fn open_sink(&self) -> OpenSink;
    /// Blocking.
    fn teardown(&self);
    /// Fetches what's missing and opens its setup; "" once it's open. Blocking.
    fn install(&self) -> String {
        String::new()
    }
}

pub fn default_backend() -> Arc<dyn MicBackend> {
    if cfg!(windows) { Arc::new(WindowsMic::default()) } else { Arc::new(LinuxMic::default()) }
}

#[derive(Default)]
pub struct LinuxMic {
    modules: Mutex<Vec<u32>>,
}

impl MicBackend for LinuxMic {
    fn pick_name(&self) -> String {
        virtual_mic::source_name().into()
    }

    fn problem(&self) -> Option<Problem> {
        let missing = virtual_mic::linux_tools_missing(&virtual_mic::which);
        if missing.is_empty() {
            return None;
        }
        // LINUX_TOOLS is one tool, so the list never needs an "and"
        Some((tr("Needs {tools} (in pulseaudio-utils; PipeWire systems use it too).").field("tools", &missing.join(", ")), None))
    }

    fn prepare(&self) -> String {
        let mut modules = self.modules.lock().unwrap_or_else(|e| e.into_inner());
        if !modules.is_empty() {
            return String::new();
        }
        let (ids, err) = virtual_mic::linux_setup(&virtual_mic::run, None);
        *modules = ids;
        err
    }

    fn open_sink(&self) -> OpenSink {
        #[cfg(unix)]
        {
            Arc::new(|| {
                let path = virtual_mic::fifo_path().map_err(|e| e.to_string())?;
                crate::sink::FifoSink::open(&path).map(|s| Box::new(s) as Box<dyn crate::worker::MicSink>).map_err(|e| e.to_string())
            })
        }
        #[cfg(not(unix))]
        {
            Arc::new(|| Err("no pipe source here".to_string()))
        }
    }

    fn teardown(&self) {
        let ids = std::mem::take(&mut *self.modules.lock().unwrap_or_else(|e| e.into_inner()));
        virtual_mic::linux_teardown(&ids, &virtual_mic::run);
    }
}

pub type Devices = Box<dyn Fn() -> Option<Vec<(String, u32)>> + Send + Sync>;
pub type Installer = Box<dyn Fn() -> Result<std::path::PathBuf, String> + Send + Sync>;
pub type Opener = Box<dyn Fn(&std::path::Path) -> String + Send + Sync>;

pub struct WindowsMic {
    /// The setup was opened once: closed without installing, it can be opened again.
    pub setup_opened: AtomicBool,
    device: Mutex<Option<usize>>,
    /// Every output device with its channels; None when the sound system can't be asked.
    devices: Devices,
    download: Installer,
    run_setup: Opener,
}

impl Default for WindowsMic {
    fn default() -> Self {
        WindowsMic::new(
            Box::new(system_devices),
            Box::new(|| crate::vb_cable::download_vb_cable(None, &std::env::temp_dir(), &crate::vb_cable::web, &Default::default())),
            Box::new(|setup| crate::vb_cable::run_vb_cable_setup(setup, &crate::vb_cable::start_as_admin, &Default::default())),
        )
    }
}

#[cfg(windows)]
fn system_devices() -> Option<Vec<(String, u32)>> {
    Some(crate::sink::output_devices())
}

#[cfg(not(windows))]
fn system_devices() -> Option<Vec<(String, u32)>> {
    None
}

impl WindowsMic {
    pub fn new(devices: Devices, download: Installer, run_setup: Opener) -> WindowsMic {
        WindowsMic { setup_opened: AtomicBool::new(false), device: Mutex::new(None), devices, download, run_setup }
    }
}

impl MicBackend for WindowsMic {
    fn pick_name(&self) -> String {
        virtual_mic::VB_CABLE_RECORD.into()
    }

    fn problem(&self) -> Option<Problem> {
        let Some(devices) = (self.devices)() else {
            return Some((tr("The sounddevice package isn't installed."), None));
        };
        let found = virtual_mic::find_vb_cable(&devices);
        *self.device.lock().unwrap_or_else(|e| e.into_inner()) = found;
        if found.is_some() {
            return None;
        }
        if self.setup_opened.load(Ordering::SeqCst) {
            return Some((tr("Once VB-Cable's setup says it's done, restart the computer, then switch this off and on."),
                         Some((tr("Open setup again"), INSTALL.into()))));
        }
        Some((tr("Needs VB-Audio Virtual Cable, a free virtual audio cable by VB-Audio."), Some((tr("Install VB-Cable"), INSTALL.into()))))
    }

    fn install(&self) -> String {
        let err = match (self.download)() {
            Ok(setup) => (self.run_setup)(&setup),
            Err(e) => e,
        };
        self.setup_opened.store(err.is_empty(), Ordering::SeqCst);
        err
    }

    fn prepare(&self) -> String {
        String::new()
    }

    fn open_sink(&self) -> OpenSink {
        let device = *self.device.lock().unwrap_or_else(|e| e.into_inner());
        #[cfg(windows)]
        {
            Arc::new(move || {
                let index = device.ok_or_else(|| "VB-Cable isn't there".to_string())?;
                crate::sink::SoundDeviceSink::open(index).map(|s| Box::new(s) as Box<dyn crate::worker::MicSink>)
            })
        }
        #[cfg(not(windows))]
        {
            let _ = device;
            Arc::new(|| Err("VB-Cable is only on Windows".to_string()))
        }
    }

    fn teardown(&self) {}
}
