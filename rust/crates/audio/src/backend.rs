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

type RunFn = Arc<dyn Fn(&[String]) -> (bool, String) + Send + Sync>;

pub struct LinuxMic {
    modules: Arc<Mutex<Vec<u32>>>,
    run: RunFn,
    /// The pipe's path when it isn't the usual one (tests).
    fifo: Option<std::path::PathBuf>,
}

impl Default for LinuxMic {
    fn default() -> Self {
        LinuxMic::with(Arc::new(virtual_mic::run), None)
    }
}

impl LinuxMic {
    pub fn with(run: RunFn, fifo: Option<std::path::PathBuf>) -> LinuxMic {
        LinuxMic { modules: Arc::default(), run, fifo }
    }
}

/// The source exists: the modules set up earlier are still loaded, else it's set up again (a restarted sound server
/// takes them with it). "" when ready, else why not.
fn ensure_source(modules: &Mutex<Vec<u32>>, run: &RunFn, fifo: &Option<std::path::PathBuf>) -> String {
    let mut modules = modules.lock().unwrap_or_else(|e| e.into_inner());
    if !modules.is_empty() && virtual_mic::modules_loaded(&modules, &**run) {
        return String::new();
    }
    let (ids, err) = virtual_mic::linux_setup(&**run, fifo.clone());
    *modules = ids;
    err
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
        Some((tr("Needs {tools} (in pulseaudio-utils; PipeWire systems use it too).").field("tools", missing.join(", ")), None))
    }

    fn prepare(&self) -> String {
        ensure_source(&self.modules, &self.run, &self.fifo)
    }

    fn open_sink(&self) -> OpenSink {
        #[cfg(unix)]
        {
            let (modules, run, fifo) = (self.modules.clone(), self.run.clone(), self.fifo.clone());
            Arc::new(move || {
                let path = fifo.clone().map(Ok).unwrap_or_else(virtual_mic::fifo_path).map_err(|e| e.to_string())?;
                let open = || crate::sink::FifoSink::open(&path).map(|s| Box::new(s) as Box<dyn crate::worker::MicSink>);
                if let Ok(sink) = open() {
                    return Ok(sink);
                }
                // The reader is gone: if the sound server restarted, its source went with it
                let err = ensure_source(&modules, &run, &fifo);
                if !err.is_empty() {
                    return Err(err);
                }
                open().map_err(|e| e.to_string())
            })
        }
        #[cfg(not(unix))]
        {
            Arc::new(|| Err("no pipe source here".to_string()))
        }
    }

    fn teardown(&self) {
        let ids = std::mem::take(&mut *self.modules.lock().unwrap_or_else(|e| e.into_inner()));
        virtual_mic::linux_teardown(&ids, &*self.run);
    }
}

pub type Devices = Box<dyn Fn() -> Vec<(String, u32)> + Send + Sync>;
pub type Installer = Box<dyn Fn() -> Result<std::path::PathBuf, String> + Send + Sync>;
pub type Opener = Box<dyn Fn(&std::path::Path) -> String + Send + Sync>;

pub struct WindowsMic {
    /// The setup was opened once: closed without installing, it can be opened again.
    pub setup_opened: AtomicBool,
    device: Mutex<Option<usize>>,
    /// Every output device with its channels.
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
fn system_devices() -> Vec<(String, u32)> {
    crate::sink::output_devices()
}

#[cfg(not(windows))]
fn system_devices() -> Vec<(String, u32)> {
    Vec::new()
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
        let found = virtual_mic::find_vb_cable(&(self.devices)());
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStrExt;

    /// A sound server that keeps its loaded modules and makes the FIFO a pipe-source would read.
    struct Server {
        loaded: Mutex<Vec<u32>>,
        next: Mutex<u32>,
        loads: Mutex<u32>,
        fifo: std::path::PathBuf,
        reader: Mutex<Option<libc::c_int>>,
    }

    impl Server {
        fn new(name: &str) -> Arc<Server> {
            let dir = std::env::temp_dir().join(format!("telescope-backend-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Arc::new(Server { loaded: Mutex::default(), next: Mutex::new(40), loads: Mutex::default(), fifo: dir.join("mic.fifo"), reader: Mutex::default() })
        }

        fn run(&self, cmd: &[String]) -> (bool, String) {
            match cmd[1].as_str() {
                "list" => (true, self.loaded.lock().unwrap().iter()
                    .map(|id| format!("{id}\tmodule-pipe-source\tsource_name=telescope_mic file=x")).collect::<Vec<_>>().join("\n")),
                "load-module" => {
                    let id = {
                        let mut next = self.next.lock().unwrap();
                        *next += 1;
                        *next
                    };
                    self.loaded.lock().unwrap().push(id);
                    *self.loads.lock().unwrap() += 1;
                    let c = std::ffi::CString::new(self.fifo.as_os_str().as_bytes()).unwrap();
                    // SAFETY: a valid C string; the read end is closed in restart or left to the test's end
                    unsafe {
                        libc::mkfifo(c.as_ptr(), 0o600);
                        *self.reader.lock().unwrap() = Some(libc::open(c.as_ptr(), libc::O_RDONLY | libc::O_NONBLOCK));
                    }
                    (true, id.to_string())
                }
                _ => (true, String::new()),
            }
        }

        /// The sound server restarting: everything it loaded is gone, and so is the pipe's reader.
        fn restart(&self) {
            self.loaded.lock().unwrap().clear();
            if let Some(fd) = self.reader.lock().unwrap().take() {
                // SAFETY: closing the read end opened in run
                unsafe { libc::close(fd) };
            }
            let _ = std::fs::remove_file(&self.fifo);
        }

        fn mic(self: &Arc<Self>) -> LinuxMic {
            let s = self.clone();
            LinuxMic::with(Arc::new(move |c| s.run(c)), Some(self.fifo.clone()))
        }
    }

    #[test]
    fn prepare_sets_up_again_after_the_sound_server_restarted() {
        let server = Server::new("prepare");
        let mic = server.mic();
        assert_eq!(mic.prepare(), "");
        assert_eq!(mic.prepare(), "");
        assert_eq!(*server.loads.lock().unwrap(), 1); // still loaded: nothing to redo
        server.restart();
        assert_eq!(mic.prepare(), "");
        assert_eq!(*server.loads.lock().unwrap(), 2);
        assert_eq!(*server.loaded.lock().unwrap(), vec![42]);
    }

    #[test]
    fn the_output_sets_the_source_up_again_when_its_pipe_is_gone() {
        let server = Server::new("sink");
        let mic = server.mic();
        let open = mic.open_sink();
        assert_eq!(mic.prepare(), "");
        assert!(open().is_ok());
        server.restart();
        assert!(open().is_ok());
        assert_eq!(*server.loads.lock().unwrap(), 2);
    }
}
