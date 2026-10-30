//! Virtual cameras behind core's Outputs trait (vcam.py): v4l2loopback slots on Linux, the Windows 11 camera's shared
//! memory and UnityCapture on Windows, the wait screen that holds them while idle and the watch that tells whether an
//! app reads them. Also the Stream output card and the Webcam source, which the shell gets as plugins.

pub mod convert;
pub mod held;
pub mod linux_setup;
pub mod mf;
pub mod stream_output;
pub mod unitycapture;
pub mod wait_screen;
pub mod wait_screen_plugin;
pub mod watch;
pub mod webcam;
pub mod webcam_capture;
pub mod webcam_controls;
pub mod webcams;
pub mod webcams_win;
pub mod wincam;
pub mod wininstall;

#[cfg(test)]
mod ui_check;

#[cfg(target_os = "linux")]
pub mod v4l2;
#[cfg(windows)]
pub mod win;

use held::Gate;
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use telescope_core::frame::{Frame, PixelFormat};
use telescope_core::output::{Outputs, Released, SetupAction, SetupResult, SetupStatus, Slot, VideoOutput};

/// Virtual cameras streams can go to at once: the usual one and up to three more.
pub const MAX_SLOTS: u32 = 4;
/// The label the Linux setup gives slot 0's v4l2loopback device.
pub const SLOT0_LABEL: &str = "Phone Camera";
/// Seconds before trying a camera that wouldn't open again.
pub const RETRY_OPEN: Duration = Duration::from_secs(3);
/// Old-driver fallback: how often to look for readers in /proc.
pub const PROC_SCAN_PERIOD: Duration = Duration::from_secs(5);
/// Windows 11 camera: how often the watch looks at it.
pub const MF_WATCH_PERIOD: Duration = Duration::from_millis(500);

/// Everything holding the cameras while idle, for a driver reload (one per process, as in vcam.py).
fn gate() -> &'static Arc<Gate> {
    static GATE: OnceLock<Arc<Gate>> = OnceLock::new();
    GATE.get_or_init(Arc::default)
}

/// What apps list slot's camera as on this system.
pub fn slot_label(slot: u32) -> String {
    #[cfg(target_os = "linux")]
    return v4l2::slot_label(slot);
    #[cfg(not(target_os = "linux"))]
    wincam::slot_label(slot)
}

/// The virtual cameras on this computer. device: use this one as slot 0 instead of /dev/video11.
#[derive(Default)]
pub struct VirtualCameras {
    pub device: Option<String>,
}

#[cfg(target_os = "linux")]
struct V4l2(v4l2::VirtualCamera);

#[cfg(target_os = "linux")]
impl VideoOutput for V4l2 {
    fn send(&mut self, frame: &Frame) -> Result<(), String> {
        let converted;
        let frame = match frame.format {
            PixelFormat::Rgba => {
                converted = convert::rgb_to_i420(&frame.data, frame.width, frame.height, 4, false);
                &converted
            }
            _ => frame,
        };
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

impl VirtualCameras {
    /// slot's /dev/video node on Linux, if it's set up.
    #[cfg(target_os = "linux")]
    fn node(&self, slot: u32) -> Option<String> {
        if slot == 0 {
            if let Some(d) = &self.device {
                return Some(d.clone());
            }
            if std::path::Path::new(v4l2::PHONE_DEV).exists() {
                return Some(v4l2::PHONE_DEV.into());
            }
            // Set up by hand, or by the prototype, under its label
            return v4l2::find_by_label(SLOT0_LABEL);
        }
        v4l2::slot_device_in(v4l2::SYSFS, slot)
    }

    fn opener(&self) -> wait_screen::OpenCamera {
        let device = self.device.clone();
        Arc::new(move |w, h, fps, _fmt| VirtualCameras { device: device.clone() }.open(0, w, h, fps))
    }
}

/// Open slot's camera on this system (vcam.open_camera).
#[cfg(target_os = "linux")]
fn open_slot(cams: &VirtualCameras, slot: u32, width: u32, height: u32, fps: u32) -> Result<Box<dyn VideoOutput>, String> {
    let Some(device) = cams.node(slot) else {
        return Err(format!("{} isn't set up", slot_label(slot)));
    };
    v4l2::VirtualCamera::open(&device, width, height, fps)
        .map(|c| Box::new(V4l2(c)) as Box<dyn VideoOutput>)
        .map_err(|e| format!("{device}: {e}"))
}

#[cfg(windows)]
fn open_slot(_cams: &VirtualCameras, slot: u32, width: u32, height: u32, _fps: u32) -> Result<Box<dyn VideoOutput>, String> {
    let reg = wincam::System;
    let still = move || wincam::use_mf(&wincam::System, slot);
    if wincam::use_mf(&reg, slot) {
        return Ok(Box::new(mf::MfCamera::new(width, height, slot_label(slot), Box::new(move || mf::open_shared(slot)), Box::new(still))));
    }
    let names = wincam::uc_names(&reg);
    let label = slot_label(slot);
    let num = match names.iter().find(|(_, n)| *n == label) {
        Some((num, _)) => *num,
        // Registered before it was named Telescope ("Unity Video Capture"): any one will do for the main camera
        None if slot == 0 && !names.is_empty() => names[0].0,
        None => return Err(format!("{label} isn't set up")),
    };
    let target = Box::new(unitycapture::Shared::new(num));
    Ok(Box::new(unitycapture::UcCamera::new(width, height, label, target, Box::new(still))))
}

#[cfg(not(any(target_os = "linux", windows)))]
fn open_slot(_cams: &VirtualCameras, _slot: u32, _w: u32, _h: u32, _fps: u32) -> Result<Box<dyn VideoOutput>, String> {
    Err("Virtual cameras on this system aren't in this build".into())
}

impl Outputs for VirtualCameras {
    fn slots(&self) -> Vec<Slot> {
        (0..MAX_SLOTS).filter_map(|slot| {
            #[cfg(target_os = "linux")]
            let device = self.node(slot).filter(|d| std::path::Path::new(d).exists())?;
            #[cfg(windows)]
            let device = (wincam::slot_ready(&wincam::System, slot) && (slot > 0 || wincam::use_mf(&wincam::System, 0)
                || !wincam::uc_names(&wincam::System).is_empty())).then(|| format!("{}{slot}", mf::SHARED_NAME))?;
            #[cfg(not(any(target_os = "linux", windows)))]
            let device: String = None?;
            Some(Slot { slot, name: slot_label(slot), device })
        }).collect()
    }

    fn formats(&self) -> Vec<PixelFormat> {
        vec![PixelFormat::I420]
    }

    fn open(&self, slot: u32, width: u32, height: u32, fps: u32) -> Result<Box<dyn VideoOutput>, String> {
        open_slot(self, slot, width, height, fps)
    }

    fn max_slots(&self) -> u32 {
        MAX_SLOTS
    }

    fn slot_label(&self, slot: u32) -> String {
        slot_label(slot)
    }

    fn slot_ready(&self, slot: u32) -> bool {
        if slot == 0 {
            return true;
        }
        #[cfg(target_os = "linux")]
        return self.node(slot).is_some();
        #[cfg(windows)]
        return wincam::slot_ready(&wincam::System, slot);
        #[cfg(not(any(target_os = "linux", windows)))]
        false
    }

    fn locked_size(&self, slot: u32) -> Option<(u32, u32)> {
        #[cfg(target_os = "linux")]
        return v4l2::locked_size_in(v4l2::SYSFS, &self.node(slot)?);
        #[cfg(windows)]
        {
            if !wincam::use_mf(&wincam::System, slot) {
                return None;
            }
            return mf::open_shared(slot)?.reader_size();
        }
        #[cfg(not(any(target_os = "linux", windows)))]
        {
            let _ = slot;
            None
        }
    }

    fn watch(&self, on_change: Box<dyn Fn(bool) + Send + Sync>) -> Option<Box<dyn telescope_core::output::Held>> {
        #[cfg(target_os = "linux")]
        let device = self.node(0).unwrap_or_else(|| v4l2::PHONE_DEV.into());
        #[cfg(not(target_os = "linux"))]
        let device = String::new();
        let holder_device = device.clone();
        let deps = watch::WatchDeps {
            device,
            retry_open: RETRY_OPEN,
            proc_scan: PROC_SCAN_PERIOD,
            #[cfg(target_os = "linux")]
            holders: Arc::new(move || v4l2::camera_holders(&holder_device, std::process::id(), "/proc")),
            #[cfg(not(target_os = "linux"))]
            holders: Arc::new(move || { let _ = &holder_device; Vec::new() }),
            #[cfg(windows)]
            use_mf: Arc::new(|| wincam::use_mf(&wincam::System, 0)),
            #[cfg(not(windows))]
            use_mf: Arc::new(|| false),
            #[cfg(windows)]
            mf_open: Arc::new(|| mf::open_shared(0)),
            #[cfg(not(windows))]
            mf_open: Arc::new(|| None),
            mf_period: MF_WATCH_PERIOD,
            linger: mf::WATCH_LINGER,
            #[cfg(windows)]
            uc_want: Arc::new(|| {
                let num = wincam::uc_number(&wincam::System, wincam::UC_NAME);
                let event = win::open_event(&wincam::uc_object_name("Want", num), win::SYNCHRONIZE)?;
                Some(Box::new(watch::WinWant(event)) as Box<dyn watch::WantEvent>)
            }),
            #[cfg(not(windows))]
            uc_want: Arc::new(|| None),
        };
        let watcher = watch::Watcher::new(on_change, deps);
        Some(Box::new(watch::CameraWatch(held::Held::new(watcher, gate()))))
    }

    fn wait_screen(&self, slot: u32) -> Option<Box<dyn telescope_core::output::WaitScreen>> {
        Some(Box::new(wait_screen::WaitScreen::new(self.screen_deps(slot), gate())))
    }

    fn wait_picture(&self, path: Option<&str>, width: u32, height: u32) -> Option<Frame> {
        let frames = match path {
            Some(p) => wait_screen::read_image_frames(p, width, height),
            None => wait_screen::load_frames(None, width, height),
        };
        let (rgb, _) = frames.into_iter().next()?;
        let rgba = rgb.data.as_chunks::<3>().0.iter().flat_map(|p| [p[0], p[1], p[2], 255]).collect();
        Some(Frame::new(rgb.width, rgb.height, PixelFormat::Rgba, rgba))
    }

    fn release_devices(&self) -> Released {
        gate().release()
    }

    #[allow(clippy::needless_return)] // the other platforms' blocks follow it
    fn setup_status(&self) -> SetupStatus {
        #[cfg(target_os = "linux")]
        {
            let setup = linux_setup::Setup { sys: &linux_setup::Real };
            let (a, b) = setup.persist_status();
            let loaded = setup.module_loaded();
            return SetupStatus { installed: setup.module_installed(), loaded, ready: setup.devices_ready(), persisted: a || b,
                                 ..SetupStatus::default() };
        }
        #[cfg(windows)]
        {
            return wincam::setup_status(&wincam::System, &wininstall::unitycapture_dir(), &wininstall::mf_camera_dir());
        }
        #[cfg(not(any(target_os = "linux", windows)))]
        SetupStatus::default()
    }

    #[allow(clippy::needless_return)] // the other platforms' blocks follow it
    fn set_up(&self, action: SetupAction, _progress: &dyn Fn(&str)) -> SetupResult {
        #[cfg(target_os = "linux")]
        {
            let setup = linux_setup::Setup { sys: &linux_setup::Real };
            return match action {
                SetupAction::Load { persist } => setup.setup(persist),
                SetupAction::Reload => {
                    let _released = self.release_devices();
                    setup.reload()
                }
                SetupAction::Unload => {
                    let _released = self.release_devices();
                    setup.unload()
                }
                SetupAction::Persist { on: true } => setup.persist_enable(),
                SetupAction::Persist { on: false } => setup.persist_disable(),
                SetupAction::AddSlots => {
                    let labels: Vec<String> = (1..MAX_SLOTS).filter(|n| !self.slot_ready(*n)).map(slot_label).collect();
                    setup.add_devices(&labels)
                }
                SetupAction::RemoveSlots => {
                    let devices: Vec<String> = (1..MAX_SLOTS).filter_map(|n| self.node(n)).collect();
                    let _released = self.release_devices();
                    setup.remove_devices(&devices)
                }
                SetupAction::Install { .. } => SetupResult { ok: false, message: "Linux sets up v4l2loopback instead".into(), command: None },
            };
        }
        #[cfg(windows)]
        {
            return install_for(action, || self.release_devices(), |devices, reset| {
                wininstall::install_camera(&wininstall::Windows, devices, reset, _progress)
            });
        }
        #[cfg(not(any(target_os = "linux", windows)))]
        {
            let _ = action;
            SetupResult { ok: false, message: "Installing the camera from here isn't in this build yet".into(), command: None }
        }
    }
}

impl VirtualCameras {
    /// The wait screen's ties to slot's camera.
    pub fn screen_deps(&self, slot: u32) -> wait_screen::Deps {
        let me = VirtualCameras { device: self.device.clone() };
        let open: wait_screen::OpenCamera = if slot == 0 {
            self.opener()
        } else {
            let device = self.device.clone();
            Arc::new(move |w, h, fps, _fmt| open_slot(&VirtualCameras { device: device.clone() }, slot, w, h, fps))
        };
        wait_screen::Deps {
            open_camera: open,
            loader: Arc::new(|path, w, h| Ok(wait_screen::load_frames(path, w, h))),
            locked: Arc::new(move || me.locked_size(slot)),
            filter_probe: filter_probe(slot),
            i420: true,
            pace: wait_screen::Pace::default(),
        }
    }
}

/// Windows: a setup action through install(devices, reset). Removing cameras lets go of them for the whole install.
#[cfg(any(windows, test))]
fn install_for(action: SetupAction, release: impl FnOnce() -> Released, install: impl FnOnce(u32, bool) -> (bool, String)) -> SetupResult {
    let (devices, reset, _released) = match action {
        SetupAction::Install { devices, reset } => (devices, reset, None),
        SetupAction::AddSlots => (MAX_SLOTS, false, None),
        SetupAction::RemoveSlots => (1, true, Some(release())),
        _ => return SetupResult { ok: false, message: "Windows installs the camera instead".into(), command: None },
    };
    let (ok, message) = install(devices.max(1), reset);
    SetupResult { ok, message, command: None }
}

/// Windows: what tells the wait screen an app has opened slot's camera, so it sends right away, or None.
fn filter_probe(slot: u32) -> Option<wait_screen::FilterProbe> {
    #[cfg(windows)]
    {
        if wincam::use_mf(&wincam::System, slot) {
            return Some(Arc::new(move || {
                Some(Box::new(move || mf::open_shared(slot).is_some()) as Box<dyn Fn() -> bool + Send>)
            }));
        }
        if slot == 0 {
            // An app's filter makes the shared image when it opens the camera; until then every send goes nowhere
            return Some(Arc::new(|| {
                let name = wincam::uc_object_name("Data", wincam::uc_number(&wincam::System, wincam::UC_NAME));
                Some(Box::new(move || win::mapping_exists(&name)) as Box<dyn Fn() -> bool + Send>)
            }));
        }
        None
    }
    #[cfg(not(windows))]
    {
        let _ = slot;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex;
    use telescope_core::output::{Held as _, WaitScreen as _};

    struct Nothing(Arc<AtomicBool>);

    impl VideoOutput for Nothing {
        fn send(&mut self, _frame: &Frame) -> Result<(), String> {
            self.0.store(true, Ordering::SeqCst);
            Ok(())
        }
        fn size(&self) -> (u32, u32) {
            (8, 8)
        }
        fn device(&self) -> String {
            String::new()
        }
    }

    // test_vcam.py::test_a_wait_screen_on_an_extra_camera_opens_that_one
    #[test]
    fn a_wait_screen_on_an_extra_camera_opens_that_one() {
        let cams = VirtualCameras::default();
        let mut deps = cams.screen_deps(2);
        let opened = Arc::new(Mutex::new(Vec::new()));
        let asked = Arc::new(Mutex::new(Vec::new()));
        let sent = Arc::new(AtomicBool::new(false));
        // The real deps are bound to slot 2; check that by asking them, then stand in so nothing touches a device
        let (o, s) = (opened.clone(), sent.clone());
        let real_open = deps.open_camera.clone();
        deps.open_camera = Arc::new(move |w, h, fps, fmt| {
            o.lock().unwrap().push(real_open(w, h, fps, fmt).err().unwrap_or_default());
            Ok(Box::new(Nothing(s.clone())) as Box<dyn VideoOutput>)
        });
        let (a, real_locked) = (asked.clone(), deps.locked.clone());
        deps.locked = Arc::new(move || {
            a.lock().unwrap().push(real_locked());
            None
        });
        let screen = wait_screen::WaitScreen::new(deps, &Arc::new(Gate::default()));
        screen.show(64, 36, None, false);
        let until = std::time::Instant::now() + Duration::from_secs(2);
        while !sent.load(Ordering::SeqCst) {
            assert!(std::time::Instant::now() < until);
            std::thread::sleep(Duration::from_millis(5));
        }
        screen.stop();
        let why = opened.lock().unwrap()[0].clone();
        // A real Telescope running here holds the device, which the error then names instead
        assert!(why.contains(&slot_label(2)) || why.is_empty() || why.contains("busy"), "{why}");
        assert!(!asked.lock().unwrap().is_empty());
    }

    // app.py's remove_extra_cameras: with vcam.device_released() around install_camera
    #[test]
    fn removing_extra_cameras_keeps_them_let_go_until_the_install_is_done() {
        let resumed = Arc::new(AtomicBool::new(false));
        let r = resumed.clone();
        let result = install_for(SetupAction::RemoveSlots, move || Released::new(move || r.store(true, Ordering::SeqCst)), |devices, reset| {
            assert_eq!((devices, reset), (1, true));
            assert!(!resumed.load(Ordering::SeqCst), "the held cameras came back before the installer ran");
            (true, "done".into())
        });
        assert!(result.ok && resumed.load(Ordering::SeqCst));
    }

    #[test]
    fn slots_are_labelled_the_way_apps_list_them() {
        let cams = VirtualCameras::default();
        assert_eq!(cams.max_slots(), 4);
        #[cfg(target_os = "linux")]
        assert_eq!((cams.slot_label(0), cams.slot_label(1)), ("Phone Camera".to_string(), "Phone Camera 2".to_string()));
        assert!(cams.slot_ready(0));
    }

    #[test]
    fn the_wait_picture_is_the_default_screen_without_a_path() {
        let f = VirtualCameras::default().wait_picture(None, 64, 36).unwrap();
        assert_eq!((f.width, f.height, f.format), (64, 36, PixelFormat::Rgba));
        assert!(VirtualCameras::default().wait_picture(Some("/nonexistent.png"), 64, 36).is_none());
    }
}
