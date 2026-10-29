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

/// What these checks read from Windows.
pub trait Registry: Send + Sync {
    /// A string value under HKEY_CLASSES_ROOT ("" names the default value).
    fn classes_string(&self, path: &str, name: &str) -> Option<String>;
    fn classes_number(&self, path: &str, name: &str) -> Option<i64>;
    fn machine_subkeys(&self, path: &str) -> Vec<String>;
    fn machine_string(&self, path: &str, name: &str) -> Option<String>;
    fn file_exists(&self, path: &str) -> bool;
    fn build(&self) -> u32;
    fn env(&self, name: &str) -> Option<String>;
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

/// The name Telescope's UnityCapture filter is registered under, or None: the first registration whose DLL is one of
/// dlls (lower case paths to UnityCaptureFilter64.dll).
pub fn uc_registered_name(reg: &dyn Registry, clsids: &[String], dlls: &[String]) -> Option<String> {
    for clsid in clsids {
        let Some(path) = reg.classes_string(&format!(r"CLSID\{clsid}\InprocServer32"), "") else { continue };
        if dlls.contains(&path.to_lowercase()) {
            let name = reg.classes_string(&format!(r"CLSID\{clsid}"), "").unwrap_or_default();
            if !name.ends_with(" Configuration") {
                return Some(if name.is_empty() { UC_DEFAULT_NAME.into() } else { name });
            }
        }
    }
    None
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
}
