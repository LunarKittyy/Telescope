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
}

/// Open slot's camera on this system (vcam.open_camera).
#[cfg(target_os = "linux")]
fn open_slot(cams: &VirtualCameras, slot: u32, width: u32, height: u32, fps: u32) -> Result<Box<dyn VideoOutput>, String> {
    open_node(cams.node(slot), slot, width, height, fps)
}

/// Open device for slot, which isn't set up without one.
#[cfg(target_os = "linux")]
fn open_node(device: Option<String>, slot: u32, width: u32, height: u32, fps: u32) -> Result<Box<dyn VideoOutput>, String> {
    let Some(device) = device else {
        return Err(format!("{} isn't set up", slot_label(slot)));
    };
    v4l2::VirtualCamera::open(&device, width, height, fps)
        .map(|c| Box::new(V4l2(c)) as Box<dyn VideoOutput>)
        .map_err(|e| format!("{device}: {e}"))
}

#[cfg(windows)]
fn open_slot(_cams: &VirtualCameras, slot: u32, width: u32, height: u32, _fps: u32) -> Result<Box<dyn VideoOutput>, String> {
    let shared = || Box::new(move || mf::open_shared(slot)) as mf::Opener;
    open_windows(Arc::new(wincam::System), slot, width, height, shared, |num| Box::new(unitycapture::Shared::new(num)))
}

/// Windows: slot's camera, the Windows 11 one where it's set up, else UnityCapture. Either kind counts as outdated once
/// the slot goes to the other (vcam.camera_changed). shared reaches the former's memory, filter the latter's number.
#[cfg(any(windows, test))]
fn open_windows(
    reg: Arc<dyn wincam::Registry>, slot: u32, width: u32, height: u32, shared: impl FnOnce() -> mf::Opener,
    filter: impl FnOnce(u32) -> Box<dyn unitycapture::Target>,
) -> Result<Box<dyn VideoOutput>, String> {
    let label = wincam::slot_label(slot);
    let to_mf = {
        let reg = reg.clone();
        move || wincam::use_mf(reg.as_ref(), slot)
    };
    if to_mf() {
        return Ok(Box::new(mf::MfCamera::new(width, height, label, shared(), Box::new(to_mf))));
    }
    let names = wincam::uc_names(reg.as_ref());
    let num = match names.iter().find(|(_, n)| *n == label) {
        Some((num, _)) => *num,
        // Registered before it was named Telescope ("Unity Video Capture"): any one will do for the main camera
        None if slot == 0 && !names.is_empty() => names[0].0,
        None => return Err(format!("{label} isn't set up")),
    };
    Ok(Box::new(unitycapture::UcCamera::new(width, height, label, filter(num), Box::new(to_mf))))
}

/// Windows: the size slot's camera is stuck at, which only the Windows 11 camera has (vcam._mf_locked_size).
#[cfg(any(windows, test))]
fn locked_size_on(reg: &dyn wincam::Registry, slot: u32, open: impl FnOnce() -> Option<mf::SharedView>) -> Option<(u32, u32)> {
    if !wincam::use_mf(reg, slot) {
        return None;
    }
    open()?.reader_size()
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
        return v4l2::slot_ready_in(v4l2::SYSFS, slot);
        #[cfg(windows)]
        return wincam::slot_ready(&wincam::System, slot);
        #[cfg(not(any(target_os = "linux", windows)))]
        false
    }

    fn locked_size(&self, slot: u32) -> Option<(u32, u32)> {
        #[cfg(target_os = "linux")]
        return v4l2::locked_size_in(v4l2::SYSFS, &self.node(slot)?);
        #[cfg(windows)]
        return locked_size_on(&wincam::System, slot, || mf::open_shared(slot));
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
        let (one, other) = (VirtualCameras { device: self.device.clone() }, VirtualCameras { device: self.device.clone() });
        let open = Arc::new(move |slot, w, h, fps| open_slot(&one, slot, w, h, fps));
        screen_deps_on(slot, open, Arc::new(move |slot| other.locked_size(slot)))
    }
}

/// Opens a slot's camera at width x height, fps.
type OpenSlot = Arc<dyn Fn(u32, u32, u32, u32) -> Result<Box<dyn VideoOutput>, String> + Send + Sync>;
/// The size an app still reading a slot's camera holds it at.
type LockedSlot = Arc<dyn Fn(u32) -> Option<(u32, u32)> + Send + Sync>;

fn screen_deps_on(slot: u32, open: OpenSlot, locked: LockedSlot) -> wait_screen::Deps {
    wait_screen::Deps {
        open_camera: Arc::new(move |w, h, fps, _fmt| open(slot, w, h, fps)),
        loader: Arc::new(|path, w, h, convert| Ok(wait_screen::load_frames_as(path, w, h, convert))),
        locked: Arc::new(move || locked(slot)),
        filter_probe: filter_probe(slot),
        i420: true,
        pace: wait_screen::Pace::default(),
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
        Some(match wincam::filter_probe_for(&wincam::System, slot)? {
            wincam::FilterProbe::Shared => Arc::new(move || {
                Some(Box::new(move || mf::open_shared(slot).is_some()) as Box<dyn Fn() -> bool + Send>)
            }),
            // An app's filter makes the shared image when it opens the camera; until then every send goes nowhere
            wincam::FilterProbe::Image => Arc::new(|| {
                let name = wincam::uc_object_name("Data", wincam::uc_number(&wincam::System, wincam::UC_NAME));
                Some(Box::new(move || win::mapping_exists(&name)) as Box<dyn Fn() -> bool + Send>)
            }),
        })
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
    use crate::mf::tests::Opener as MfMemory;
    use crate::wincam::tests::FakeRegistry;
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
        let (opened, asked) = (Arc::new(Mutex::new(Vec::new())), Arc::new(Mutex::new(Vec::new())));
        let sent = Arc::new(AtomicBool::new(false));
        let (o, a, s) = (opened.clone(), asked.clone(), sent.clone());
        let open: OpenSlot = Arc::new(move |slot, _, _, _| {
            o.lock().unwrap().push(slot);
            Ok(Box::new(Nothing(s.clone())) as Box<dyn VideoOutput>)
        });
        let deps = screen_deps_on(2, open, Arc::new(move |slot| {
            a.lock().unwrap().push(slot);
            None
        }));
        let screen = wait_screen::WaitScreen::new(deps, &Arc::new(Gate::default()));
        screen.show(64, 36, None, false);
        wait_until(|| sent.load(Ordering::SeqCst));
        screen.stop();
        assert_eq!((opened.lock().unwrap()[0], asked.lock().unwrap()[0]), (2, 2));
    }

    fn wait_until(what: impl Fn() -> bool) {
        let until = std::time::Instant::now() + Duration::from_secs(2);
        while !what() {
            assert!(std::time::Instant::now() < until, "timed out");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Nowhere for UnityCapture's frames to go, but it says when it's let go of.
    struct NoFilter(Arc<AtomicBool>);

    impl unitycapture::Target for NoFilter {
        fn send(&mut self, _rgba: &[u8], _width: u32, _height: u32) -> Result<(), String> {
            Ok(())
        }
    }

    impl Drop for NoFilter {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    fn windows_camera(reg: &Arc<FakeRegistry>, slot: u32, shared: &MfMemory, closed: &Arc<AtomicBool>) -> Box<dyn VideoOutput> {
        let closed = closed.clone();
        let reg: Arc<dyn wincam::Registry> = reg.clone();
        open_windows(reg, slot, 8, 4, || shared.opener(), |_num| Box::new(NoFilter(closed))).ok().unwrap()
    }

    // test_vcam.py::test_windows_opens_the_windows_11_camera_where_it_is_set_up
    #[test]
    fn windows_opens_the_windows_11_camera_where_it_is_set_up() {
        let reg = Arc::new(FakeRegistry::default().with_mf(&[0, 2]));
        let shared = MfMemory::new(Some(crate::mf::tests::reading(8, 4)));
        let mut cam = windows_camera(&reg, 2, &shared, &Arc::default());
        assert_eq!(cam.device(), "Telescope #3");
        cam.send(&convert::grey(8, 4, 0)).unwrap();
        assert_eq!(shared.header().counter, 2, "frames reach the Windows 11 camera's memory");
        assert!(wincam::slot_ready(reg.as_ref(), 2) && !wincam::slot_ready(reg.as_ref(), 1));
        assert_eq!(wincam::filter_probe_for(reg.as_ref(), 1), None);
        assert_eq!(wincam::filter_probe_for(reg.as_ref(), 2), Some(wincam::FilterProbe::Shared));
        assert_eq!(wincam::filter_probe_for(&FakeRegistry::uc(&[(0, "Telescope")]), 0), Some(wincam::FilterProbe::Image));
    }

    // test_vcam.py::test_camera_changed_compares_the_kind_with_the_slots_camera
    #[test]
    fn a_camera_is_outdated_once_its_slot_goes_to_the_other_kind() {
        let reg = Arc::new(FakeRegistry::uc(&[(0, "Telescope"), (1, "Telescope #2")]).with_mf(&[0]));
        let shared = MfMemory::new(None);
        let (mf_cam, uc_cam) = (windows_camera(&reg, 0, &shared, &Arc::default()), windows_camera(&reg, 1, &shared, &Arc::default()));
        assert!(!mf_cam.outdated() && !uc_cam.outdated());
        let moved = FakeRegistry::default().with_mf(&[1]);
        reg.take_cameras_from(&moved);
        assert!(mf_cam.outdated(), "slot 0 left the Windows 11 camera");
        assert!(uc_cam.outdated(), "slot 1 moved to it");
        // Linux has one kind of camera, so its cameras keep the trait's answer
        assert!(!Nothing(Arc::default()).outdated());
    }

    // test_vcam.py::test_the_wait_screen_moves_to_a_camera_installed_while_it_runs
    #[test]
    fn the_wait_screen_moves_to_a_camera_installed_while_it_runs() {
        let reg = Arc::new(FakeRegistry { build: wincam::MF_MIN_BUILD, ..FakeRegistry::uc(&[(0, "Telescope")]) });
        let attempts = Arc::new(Mutex::new(0));
        let closed: Arc<AtomicBool> = Arc::default();
        let (r, a, c) = (reg.clone(), attempts.clone(), closed.clone());
        let shared = MfMemory::new(None);
        let open: OpenSlot = Arc::new(move |slot, _, _, _| {
            *a.lock().unwrap() += 1;
            Ok(windows_camera(&r, slot, &shared, &c))
        });
        let mut deps = screen_deps_on(0, open, Arc::new(|_| None));
        deps.pace = wait_screen::Pace { idle: Duration::from_millis(20), ..wait_screen::Pace::default() };
        let screen = wait_screen::WaitScreen::new(deps, &Arc::new(Gate::default()));
        screen.show(8, 8, None, false);
        wait_until(|| *attempts.lock().unwrap() > 0);
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(*attempts.lock().unwrap(), 1, "the same kind: kept");
        reg.take_cameras_from(&FakeRegistry::default().with_mf(&[0]));
        wait_until(|| *attempts.lock().unwrap() >= 2);
        screen.stop();
        assert!(closed.load(Ordering::SeqCst));
    }

    // test_vcam.py::test_locked_size_is_linux_only (the Windows 11 camera has the size its reader picked, UnityCapture none)
    #[test]
    fn only_the_windows_11_camera_has_a_locked_size_on_windows() {
        let reader = || MfMemory::new(Some(crate::mf::tests::reading(8, 4))).opener()();
        assert_eq!(locked_size_on(&FakeRegistry::uc(&[(0, "Telescope")]), 0, reader), None);
        let reg = FakeRegistry::default().with_mf(&[0]);
        assert_eq!(locked_size_on(&reg, 0, reader), Some((8, 4)));
        assert_eq!(locked_size_on(&reg, 0, || None), None);
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
