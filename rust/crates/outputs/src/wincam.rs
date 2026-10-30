//! Which Windows camera a slot goes to (platform/windows.py): the Windows 11 one (desktop/virtualcamera) where it's
//! registered and Windows really has it, else UnityCapture. The registry is behind a trait so this runs anywhere.

/// MFCreateVirtualCamera arrived with Windows 11.
pub const MF_MIN_BUILD: u32 = 22000;
/// kCameraVersion in virtualcamera/camera.h: raise both when an installed DLL needs replacing.
pub const MF_VERSION: i64 = 3;
pub const MF_DLL: &str = "TelescopeCamera.dll";
/// One per camera slot, as in virtualcamera/dllmain.cpp.
pub const MF_CLSIDS: [&str; 4] = [
    "{D1063481-2B46-4750-8A9D-85F69573355A}",
    "{563D2A25-6CA0-48AF-922D-B098035893B4}",
    "{8DF9E1B0-1CD9-42D3-86F1-ED2691C3D197}",
    "{128E9FE3-4FDD-4786-87E1-EEA12E34254A}",
];
/// "unitycapture" keeps the older camera on Windows 11.
pub const FORCE_ENV: &str = "TELESCOPE_VCAM";
/// What apps list the camera as. UnityCapture takes it at registration.
pub const UC_NAME: &str = "Telescope";
pub const UC_DEFAULT_NAME: &str = "Unity Video Capture";
const KSCATEGORY_VIDEO_CAMERA: &str = r"SYSTEM\CurrentControlSet\Control\DeviceClasses\{e5323777-f976-4f5b-9b55-b94699c46e44}";
/// Windows' camera access switch, for the whole computer.
const CONSENT: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\webcam";
pub const UC_DLL_64: &str = "UnityCaptureFilter64.dll";

use std::path::{Path, PathBuf};

/// What these checks read from Windows.
pub trait Registry: Send + Sync {
    /// A string value under HKEY_CLASSES_ROOT ("" names the default value).
    fn classes_string(&self, path: &str, name: &str) -> Option<String>;
    fn classes_number(&self, path: &str, name: &str) -> Option<i64>;
    /// The names of the keys under a path in HKEY_CLASSES_ROOT.
    fn classes_subkeys(&self, path: &str) -> Vec<String>;
    fn machine_subkeys(&self, path: &str) -> Vec<String>;
    fn machine_string(&self, path: &str, name: &str) -> Option<String>;
    fn file_exists(&self, path: &str) -> bool;
    fn build(&self) -> u32;
    fn env(&self, name: &str) -> Option<String>;
    /// Program Files as Windows itself has it (SHGetKnownFolderPath), unlike %ProgramFiles%, which whoever starts Telescope can
    /// point anywhere.
    fn program_files(&self) -> Option<PathBuf>;
}

/// What apps list slot's camera as on Windows. UnityCapture numbers the extra devices it registers "Telescope #2" on.
pub fn slot_label(slot: u32) -> String {
    if slot == 0 { UC_NAME.into() } else { format!("{UC_NAME} #{}", slot + 1) }
}

/// Windows 11, and the older camera not asked for instead.
pub fn mf_allowed(reg: &dyn Registry) -> bool {
    reg.build() >= MF_MIN_BUILD && reg.env(FORCE_ENV).unwrap_or_default().trim().to_lowercase() != "unitycapture"
}

/// (DLL path, version) slot's Windows 11 camera is registered with, or None.
pub fn mf_registration(reg: &dyn Registry, slot: u32) -> Option<(String, i64)> {
    let clsid = MF_CLSIDS.get(slot as usize)?;
    let path = reg.classes_string(&format!(r"CLSID\{clsid}\InprocServer32"), "")?;
    let version = reg.classes_number(&format!(r"CLSID\{clsid}"), "TelescopeVersion").unwrap_or(0);
    Some((path, version))
}

/// The CLSIDs (upper case) of the media sources behind the virtual cameras Windows has right now. Each camera's
/// device interface names its source, and goes away with the camera.
pub fn mf_cameras(reg: &dyn Registry) -> Vec<String> {
    let mut found = Vec::new();
    for interface in reg.machine_subkeys(KSCATEGORY_VIDEO_CAMERA) {
        if !interface.to_uppercase().contains("VCAMDEVAPI") {
            continue;
        }
        let base = format!(r"{KSCATEGORY_VIDEO_CAMERA}\{interface}");
        for reference in reg.machine_subkeys(&base) {
            if let Some(clsid) = reg.machine_string(&format!(r"{base}\{reference}\Device Parameters"), "CustomCaptureSourceClsid") {
                found.push(clsid.to_uppercase());
            }
        }
    }
    found
}

/// Whether slot's Windows 11 camera is set up: its DLL registered and still there, and Windows really has the camera.
pub fn mf_registered(reg: &dyn Registry, slot: u32) -> bool {
    let Some((path, _)) = mf_registration(reg, slot) else { return false };
    reg.file_exists(&path) && mf_cameras(reg).contains(&MF_CLSIDS[slot as usize].to_uppercase())
}

/// Whether slot streams to the Windows 11 camera rather than UnityCapture.
pub fn use_mf(reg: &dyn Registry, slot: u32) -> bool {
    mf_allowed(reg) && mf_registered(reg, slot)
}

/// The UnityCapture cameras registered, (number, name), numbered the way pyvirtualcam numbers them.
pub fn uc_names(reg: &dyn Registry) -> Vec<(u32, String)> {
    let offset = if cfg!(target_pointer_width = "64") { 0x10 } else { 0x20 };
    let mut names = Vec::new();
    for num in 0..(b'z' - b'0') as u32 {
        let last = offset + num + u32::from(num != 0);
        let key = format!(r"CLSID\{{5C2CD55C-92AD-4999-8666-912BD3E700{last:02X}}}");
        if let Some(name) = reg.classes_string(&key, "") {
            names.push((num, name.trim().to_string()));
        }
    }
    names
}

/// The number pyvirtualcam would give the camera called name, or the first registered one, as open_camera falls back to.
pub fn uc_number(reg: &dyn Registry, name: &str) -> u32 {
    let names = uc_names(reg);
    names.iter().find(|(_, n)| n == name).or(names.first()).map_or(0, |(n, _)| *n)
}

/// A UnityCapture shared object's name ("Want", "Sent", "Mutx", "Data") for camera number num.
pub fn uc_object_name(kind: &str, num: u32) -> String {
    let suffix = if num == 0 { String::new() } else { char::from_u32('0' as u32 + num).map(String::from).unwrap_or_default() };
    format!("UnityCapture_{kind}{suffix}")
}

/// Whether slot's camera is set up to stream to.
pub fn slot_ready(reg: &dyn Registry, slot: u32) -> bool {
    slot == 0 || use_mf(reg, slot) || uc_names(reg).iter().any(|(_, n)| *n == slot_label(slot))
}

/// An older Telescope's Windows 11 camera is installed, and this one has a newer one (with its DLL at dll) to put
/// in its place.
pub fn mf_outdated(reg: &dyn Registry, dll_here: bool) -> bool {
    mf_registration(reg, 0).is_some_and(|(_, v)| mf_allowed(reg) && dll_here && v < MF_VERSION)
}

/// Program Files\Telescope\folder: only an admin can write there, unlike the app's folder, so nothing can swap a DLL between the
/// hash check and every later load (protected_unitycapture_dir, protected_mf_dir).
pub fn protected_dir(reg: &dyn Registry, folder: &str) -> PathBuf {
    reg.program_files().unwrap_or_else(|| PathBuf::from(r"C:\Program Files")).join("Telescope").join(folder)
}

/// (name apps list it as, folder its DLL is in) for Telescope's UnityCapture filter, or None if it isn't registered. uc_dir is the
/// folder beside the app, where an older Telescope registered it from.
pub fn uc_registration(reg: &dyn Registry, uc_dir: &Path) -> Option<(String, PathBuf)> {
    let folders = [uc_dir.to_path_buf(), protected_dir(reg, "UnityCapture")];
    for clsid in reg.classes_subkeys("CLSID") {
        let Some(value) = reg.classes_string(&format!(r"CLSID\{clsid}\InprocServer32"), "") else { continue };
        let Some(folder) = folders.iter().find(|f| f.join(UC_DLL_64).to_string_lossy().to_lowercase() == value.to_lowercase()) else { continue };
        // The filter's own key carries its display name as the default value
        let name = reg.classes_string(&format!(r"CLSID\{clsid}"), "").unwrap_or_default();
        // The property page shares the DLL
        if !name.ends_with(" Configuration") {
            return Some((if name.is_empty() { UC_DEFAULT_NAME.into() } else { name }, folder.clone()));
        }
    }
    None
}

/// The name Telescope's UnityCapture filter is registered under, or None if it isn't registered.
pub fn uc_registered_name(reg: &dyn Registry, uc_dir: &Path) -> Option<String> {
    uc_registration(reg, uc_dir).map(|(name, _)| name)
}

/// Registered by an older Telescope straight from the app's folder, which this user's programs can write to.
pub fn uc_in_app_folder(reg: &dyn Registry, uc_dir: &Path) -> bool {
    uc_registration(reg, uc_dir).is_some_and(|(_, folder)| folder != protected_dir(reg, "UnityCapture"))
}

pub fn uc_is_registered(reg: &dyn Registry, uc_dir: &Path) -> bool {
    uc_registered_name(reg, uc_dir).is_some()
}

/// Whether apps can see a Telescope camera, either kind.
pub fn camera_installed(reg: &dyn Registry, uc_dir: &Path) -> bool {
    use_mf(reg, 0) || uc_is_registered(reg, uc_dir)
}

/// Whether installing the camera here sets up the Windows 11 one: allowed, and its DLL in mf_dir.
pub fn mf_installable(reg: &dyn Registry, mf_dir: &Path) -> bool {
    mf_allowed(reg) && mf_dir.join(MF_DLL).is_file()
}

/// Whether Windows' camera access switch is off for the whole computer, which also stops cameras being removed.
pub fn camera_access_off(reg: &dyn Registry) -> bool {
    reg.machine_string(CONSENT, "Value").as_deref() == Some("Deny")
}

/// The registry as it is.
#[cfg(windows)]
pub struct System;

#[cfg(windows)]
impl Registry for System {
    fn classes_string(&self, path: &str, name: &str) -> Option<String> {
        crate::win::classes_string(path, name)
    }
    fn classes_number(&self, path: &str, name: &str) -> Option<i64> {
        crate::win::classes_number(path, name)
    }
    fn classes_subkeys(&self, path: &str) -> Vec<String> {
        crate::win::classes_subkeys(path)
    }
    fn machine_subkeys(&self, path: &str) -> Vec<String> {
        crate::win::machine_subkeys(path)
    }
    fn machine_string(&self, path: &str, name: &str) -> Option<String> {
        crate::win::machine_string(path, name)
    }
    fn file_exists(&self, path: &str) -> bool {
        std::path::Path::new(path).is_file()
    }
    fn build(&self) -> u32 {
        crate::win::windows_build()
    }
    fn env(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }
    fn program_files(&self) -> Option<PathBuf> {
        crate::win::program_files()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    #[derive(Default)]
    pub struct FakeRegistry {
        pub classes: Mutex<HashMap<(String, String), String>>,
        pub machine: Mutex<HashMap<String, Vec<String>>>,
        pub machine_values: Mutex<HashMap<(String, String), String>>,
        pub files: Mutex<Vec<String>>,
        pub build: u32,
        pub env: Option<String>,
        pub program_files: Option<PathBuf>,
        pub class_keys: Mutex<Vec<String>>,
    }

    impl FakeRegistry {
        pub fn uc(names: &[(u32, &str)]) -> FakeRegistry {
            let reg = FakeRegistry::default();
            for (num, name) in names {
                let last = 0x10 + num + u32::from(*num != 0);
                reg.classes.lock().unwrap().insert((format!(r"CLSID\{{5C2CD55C-92AD-4999-8666-912BD3E700{last:02X}}}"), String::new()), name.to_string());
            }
            reg
        }

        pub fn with_mf(mut self, slots: &[u32]) -> FakeRegistry {
            self.build = 22631;
            let refs: Vec<String> = slots.iter().map(|s| format!("#ref{s}")).collect();
            self.machine.lock().unwrap().insert(KSCATEGORY_VIDEO_CAMERA.into(), vec![r"##?#SWD#VCAMDEVAPI#x".into()]);
            self.machine.lock().unwrap().insert(format!(r"{KSCATEGORY_VIDEO_CAMERA}\##?#SWD#VCAMDEVAPI#x"), refs);
            for s in slots {
                let clsid = MF_CLSIDS[*s as usize];
                let dll = format!(r"C:\Program Files\Telescope\VirtualCamera\{MF_DLL}");
                self.classes.lock().unwrap().insert((format!(r"CLSID\{clsid}\InprocServer32"), String::new()), dll.clone());
                self.files.lock().unwrap().push(dll);
                self.machine_values.lock().unwrap().insert(
                    (format!(r"{KSCATEGORY_VIDEO_CAMERA}\##?#SWD#VCAMDEVAPI#x\#ref{s}\Device Parameters"), "CustomCaptureSourceClsid".into()),
                    clsid.to_lowercase());
            }
            self
        }
    }

    impl Registry for FakeRegistry {
        fn classes_string(&self, path: &str, name: &str) -> Option<String> {
            self.classes.lock().unwrap().get(&(path.to_string(), name.to_string())).cloned()
        }
        fn classes_number(&self, path: &str, name: &str) -> Option<i64> {
            self.classes_string(path, name)?.parse().ok()
        }
        fn classes_subkeys(&self, path: &str) -> Vec<String> {
            assert_eq!(path, "CLSID");
            self.class_keys.lock().unwrap().clone()
        }
        fn machine_subkeys(&self, path: &str) -> Vec<String> {
            self.machine.lock().unwrap().get(path).cloned().unwrap_or_default()
        }
        fn machine_string(&self, path: &str, name: &str) -> Option<String> {
            self.machine_values.lock().unwrap().get(&(path.to_string(), name.to_string())).cloned()
        }
        fn file_exists(&self, path: &str) -> bool {
            self.files.lock().unwrap().iter().any(|f| f == path)
        }
        fn build(&self) -> u32 {
            self.build
        }
        fn env(&self, _name: &str) -> Option<String> {
            self.env.clone()
        }
        fn program_files(&self) -> Option<PathBuf> {
            self.program_files.clone()
        }
    }

    // test_vcam.py::test_windows_slots_need_their_numbered_registration
    #[test]
    fn windows_slots_need_their_numbered_registration() {
        let reg = FakeRegistry::uc(&[(0, "Telescope"), (2, "Telescope #2")]);
        assert_eq!(slot_label(1), "Telescope #2");
        assert!(slot_ready(&reg, 1) && !slot_ready(&reg, 2));
    }

    // test_vcam.py::test_unitycapture_event_follows_the_camera_number
    #[test]
    fn unitycapture_objects_follow_the_camera_number() {
        let reg = FakeRegistry::uc(&[(0, "Unity Video Capture"), (1, "Telescope")]);
        assert_eq!(uc_object_name("Want", uc_number(&reg, "Telescope")), "UnityCapture_Want1");
        assert_eq!(uc_object_name("Data", uc_number(&reg, "Something else")), "UnityCapture_Data");
        let without_first = FakeRegistry::uc(&[(1, "Telescope")]);
        assert_eq!(uc_object_name("Want", uc_number(&without_first, "Something else")), "UnityCapture_Want1");
    }

    #[test]
    fn the_windows_11_camera_needs_windows_11_its_dll_and_windows_having_it() {
        let reg = FakeRegistry::default().with_mf(&[0]);
        assert!(use_mf(&reg, 0) && !use_mf(&reg, 1));
        let forced = FakeRegistry { env: Some("UnityCapture ".into()), ..FakeRegistry::default().with_mf(&[0]) };
        assert!(!use_mf(&forced, 0));
        let old = FakeRegistry { build: 19045, ..FakeRegistry::default().with_mf(&[0]) };
        assert!(!use_mf(&old, 0));
        let gone = FakeRegistry::default().with_mf(&[0]);
        gone.files.lock().unwrap().clear();
        assert!(!mf_registered(&gone, 0));
        assert!(slot_ready(&FakeRegistry::default().with_mf(&[0, 1]), 1));
    }

    #[test]
    fn an_older_windows_11_camera_is_outdated_when_a_newer_dll_is_here() {
        let reg = FakeRegistry::default().with_mf(&[0]);
        assert!(mf_outdated(&reg, true) && !mf_outdated(&reg, false));
        reg.classes.lock().unwrap().insert((format!(r"CLSID\{}", MF_CLSIDS[0]), "TelescopeVersion".into()), MF_VERSION.to_string());
        assert!(!mf_outdated(&reg, true));
    }

    fn put_class(reg: &FakeRegistry, path: &str, value: &str) {
        reg.classes.lock().unwrap().insert((path.to_string(), String::new()), value.to_string());
    }

    // test_platform_windows.py::test_the_windows_11_camera_needs_windows_11
    #[test]
    fn the_windows_11_camera_needs_windows_11() {
        for (build, forced, allowed) in [(22000, "", true), (26200, "", true), (19045, "", false), (0, "", false), (22621, "UnityCapture", false)] {
            let reg = FakeRegistry { build, env: Some(forced.into()), ..FakeRegistry::default() };
            assert_eq!(mf_allowed(&reg), allowed, "build {build}, forced {forced:?}");
        }
    }

    // test_platform_windows.py::test_it_installs_only_with_the_dll_bundled
    #[test]
    fn it_installs_only_with_the_dll_bundled() {
        let dir = telescope_core::testing::scratch_dir("mf-dll");
        let reg = FakeRegistry { build: 22631, ..FakeRegistry::default() };
        assert!(!mf_installable(&reg, &dir));
        std::fs::write(dir.join(MF_DLL), "dll").unwrap();
        assert!(mf_installable(&reg, &dir));
        assert!(!mf_installable(&FakeRegistry { build: 19045, ..FakeRegistry::default() }, &dir), "Windows 10 keeps UnityCapture");
    }

    // test_platform_windows.py::test_a_slot_streams_to_the_windows_11_camera_once_its_registered
    #[test]
    fn a_slot_streams_to_the_windows_11_camera_once_its_registered() {
        let mut reg = FakeRegistry::default().with_mf(&[1]);
        let dll = reg.files.lock().unwrap().remove(0);
        // Registered, but its DLL is gone
        assert!(!use_mf(&reg, 1));
        reg.files.lock().unwrap().push(dll);
        assert!(use_mf(&reg, 1) && !use_mf(&reg, 0));
        // Windows refused the camera, or it was removed some other way: the registration alone isn't enough
        let cameras = reg.machine.lock().unwrap().clone();
        reg.machine.lock().unwrap().clear();
        assert!(!use_mf(&reg, 1));
        *reg.machine.lock().unwrap() = cameras;
        assert!(use_mf(&reg, 1));
        // Windows 10 keeps UnityCapture whatever the registry says
        reg.build = 19045;
        assert!(!use_mf(&reg, 1));
    }

    // test_platform_windows.py::test_the_cameras_windows_has_are_read_from_their_device_interfaces
    #[test]
    fn the_cameras_windows_has_are_read_from_their_device_interfaces() {
        let ours = r"##?#SWD#VCAMDEVAPI#2B26#{e5323777-f976-4f5b-9b55-b94699c46e44}";
        let webcam = r"##?#USB#VID_046D#{e5323777-f976-4f5b-9b55-b94699c46e44}";
        let reference = "#{FCEBBA03-9D13-4C13-9940-CC84FCD132D1}";
        let reg = FakeRegistry::default();
        {
            let mut keys = reg.machine.lock().unwrap();
            keys.insert(KSCATEGORY_VIDEO_CAMERA.into(), vec![webcam.into(), ours.into()]);
            keys.insert(format!(r"{KSCATEGORY_VIDEO_CAMERA}\{ours}"), vec![reference.into()]);
            keys.insert(format!(r"{KSCATEGORY_VIDEO_CAMERA}\{webcam}"), vec![]);
        }
        reg.machine_values.lock().unwrap().insert(
            (format!(r"{KSCATEGORY_VIDEO_CAMERA}\{ours}\{reference}\Device Parameters"), "CustomCaptureSourceClsid".into()),
            MF_CLSIDS[0].to_lowercase());
        assert_eq!(mf_cameras(&reg), [MF_CLSIDS[0]]);
        reg.machine.lock().unwrap().insert(KSCATEGORY_VIDEO_CAMERA.into(), vec![webcam.into()]);
        assert!(mf_cameras(&reg).is_empty());
    }

    // test_platform_windows.py::test_protected_folder_comes_from_the_known_folder_not_the_environment
    #[test]
    fn protected_folder_comes_from_the_known_folder_not_the_environment() {
        let reg = FakeRegistry { env: Some("/somewhere/the/environment/says".into()), program_files: Some(PathBuf::from(r"D:\Programs")), ..FakeRegistry::default() };
        assert_eq!(protected_dir(&reg, "UnityCapture"), PathBuf::from(r"D:\Programs").join("Telescope").join("UnityCapture"));
        // Without the system's answer, the usual place
        assert_eq!(protected_dir(&FakeRegistry::default(), "VirtualCamera"), PathBuf::from(r"C:\Program Files").join("Telescope").join("VirtualCamera"));
    }

    // test_platform_windows.py::test_uc_is_registered_scans_registry_and_handles_absence
    #[test]
    fn uc_is_registered_scans_registry_and_handles_absence() {
        let uc = Path::new("/app/unitycapture");
        let reg = FakeRegistry::default();
        *reg.class_keys.lock().unwrap() = vec!["first".into(), "second".into()];
        put_class(&reg, r"CLSID\first\InprocServer32", "other.dll");
        put_class(&reg, r"CLSID\second\InprocServer32", &uc.join(UC_DLL_64).to_string_lossy());
        assert!(uc_is_registered(&reg, uc));
        assert!(!uc_is_registered(&FakeRegistry::default(), uc), "no registry");
    }

    fn filter_registry(dll: &Path, name: Option<&str>) -> FakeRegistry {
        let reg = FakeRegistry::default();
        *reg.class_keys.lock().unwrap() = vec!["{other}".into(), "{props}".into(), "{filter}".into()];
        let dll = dll.to_string_lossy().into_owned();
        put_class(&reg, r"CLSID\{props}\InprocServer32", &dll);
        put_class(&reg, r"CLSID\{props}", "Unity Video Capture Configuration");
        put_class(&reg, r"CLSID\{filter}\InprocServer32", &dll);
        if let Some(name) = name {
            put_class(&reg, r"CLSID\{filter}", name);
        }
        reg
    }

    // test_platform_windows.py::test_registered_name_reads_the_filter_key_and_skips_the_property_page
    #[test]
    fn registered_name_reads_the_filter_key_and_skips_the_property_page() {
        let uc = Path::new("/app/unitycapture");
        let dll = uc.join(UC_DLL_64);
        // Paths are compared whatever their case
        let upper = PathBuf::from(dll.to_string_lossy().to_uppercase());
        assert_eq!(uc_registered_name(&filter_registry(&upper, Some("Telescope")), uc).as_deref(), Some("Telescope"));
        assert_eq!(uc_registered_name(&filter_registry(&dll, Some("Unity Video Capture")), uc).as_deref(), Some("Unity Video Capture"));
        // No name stored: UnityCapture's default
        assert_eq!(uc_registered_name(&filter_registry(&dll, None), uc).as_deref(), Some(UC_DEFAULT_NAME));
    }

    // test_platform_windows.py::test_a_registration_from_the_app_folder_is_flagged
    #[test]
    fn a_registration_from_the_app_folder_is_flagged() {
        let (app, protected) = (PathBuf::from("/app/unitycapture"), PathBuf::from("/pf/Telescope/UnityCapture"));
        for protected_install in [true, false] {
            let dll = if protected_install { protected.join(UC_DLL_64) } else { app.join(UC_DLL_64) };
            let reg = FakeRegistry { program_files: Some(PathBuf::from("/pf")), ..filter_registry(&dll, Some("Telescope")) };
            // Still works either way, so the camera keeps working
            assert_eq!(uc_registered_name(&reg, &app).as_deref(), Some("Telescope"));
            assert_eq!(uc_in_app_folder(&reg, &app), !protected_install);
        }
    }

    #[test]
    fn a_camera_is_installed_when_apps_can_see_either_kind() {
        let uc = Path::new("/app/unitycapture");
        assert!(camera_installed(&FakeRegistry::default().with_mf(&[0]), uc));
        assert!(camera_installed(&filter_registry(&uc.join(UC_DLL_64), Some("Telescope")), uc));
        assert!(!camera_installed(&FakeRegistry::default(), uc));
    }

    #[test]
    fn camera_access_is_off_when_windows_says_deny() {
        let reg = FakeRegistry::default();
        assert!(!camera_access_off(&reg));
        reg.machine_values.lock().unwrap().insert((CONSENT.into(), "Value".into()), "Deny".into());
        assert!(camera_access_off(&reg));
        reg.machine_values.lock().unwrap().insert((CONSENT.into(), "Value".into()), "Allow".into());
        assert!(!camera_access_off(&reg));
    }
}
