//! libgphoto2 loaded at run time: the few functions live view and settings need, declared by hand, so the program starts on a
//! system without it and DSLRs then go through the gphoto2 command. All the unsafe code for it is here. It looks in the app's
//! own lib/gphoto2 folder first (the release bundles it), then the system's, and points the library at the bundled camera drivers
//! when it takes the bundled one.

use super::gphoto_lib::{Kind, Rig, Setting, Shot};
use libloading::Library;
use std::ffi::{c_char, c_float, c_int, c_ulong, c_void, CStr, CString};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Where the bundle's libraries are, instead of next to the program.
pub const BUNDLE_ENV: &str = "TELESCOPE_GPHOTO2_DIR";
/// Set to anything to ignore the system's libgphoto2 (to try the bundled one).
pub const NO_SYSTEM_ENV: &str = "TELESCOPE_NO_SYSTEM_GPHOTO2";
const SONAME: &str = "libgphoto2.so.6";
/// The libgphoto2 pair: the bundled copies whenever there is a bundle, since the camera drivers beside them are built for these.
const OWN: [&str; 2] = ["libgphoto2_port.so.12", SONAME];

type Ctx = *mut c_void;
type Cam = *mut c_void;
type Widget = *mut c_void;
type File = *mut c_void;

const WIDGET_TEXT: c_int = 2;
const WIDGET_RANGE: c_int = 3;
const WIDGET_TOGGLE: c_int = 4;
const WIDGET_RADIO: c_int = 5;
const WIDGET_MENU: c_int = 6;
const CAPTURE_IMAGE: c_int = 0;
const FILE_NORMAL: c_int = 1;
const EVENT_TIMEOUT: c_int = 1;
const EVENT_FILE_ADDED: c_int = 2;
const EVENT_CAPTURE_COMPLETE: c_int = 4;
/// How long a camera that makes a second file (RAW and JPEG) gets to announce it after the first, in all.
const SECOND_FILE_WAIT_MS: c_int = 3000;

/// CameraFilePath: where the camera put a file.
#[repr(C)]
struct FilePath {
    name: [c_char; 128],
    folder: [c_char; 1024],
}

/// The functions, copied out of the library, which is never unloaded.
struct Api {
    context_new: unsafe extern "C" fn() -> Ctx,
    context_unref: unsafe extern "C" fn(Ctx),
    camera_new: unsafe extern "C" fn(*mut Cam) -> c_int,
    camera_init: unsafe extern "C" fn(Cam, Ctx) -> c_int,
    camera_exit: unsafe extern "C" fn(Cam, Ctx) -> c_int,
    camera_unref: unsafe extern "C" fn(Cam) -> c_int,
    set_port_info: unsafe extern "C" fn(Cam, *mut c_void) -> c_int,
    capture_preview: unsafe extern "C" fn(Cam, File, Ctx) -> c_int,
    capture: unsafe extern "C" fn(Cam, c_int, *mut FilePath, Ctx) -> c_int,
    file_get: unsafe extern "C" fn(Cam, *const c_char, *const c_char, c_int, File, Ctx) -> c_int,
    wait_for_event: unsafe extern "C" fn(Cam, c_int, *mut c_int, *mut *mut c_void, Ctx) -> c_int,
    get_single_config: unsafe extern "C" fn(Cam, *const c_char, *mut Widget, Ctx) -> c_int,
    set_single_config: unsafe extern "C" fn(Cam, *const c_char, Widget, Ctx) -> c_int,
    file_new: unsafe extern "C" fn(*mut File) -> c_int,
    file_unref: unsafe extern "C" fn(File) -> c_int,
    file_data: unsafe extern "C" fn(File, *mut *const c_char, *mut c_ulong) -> c_int,
    widget_type: unsafe extern "C" fn(Widget, *mut c_int) -> c_int,
    widget_readonly: unsafe extern "C" fn(Widget, *mut c_int) -> c_int,
    widget_get_value: unsafe extern "C" fn(Widget, *mut c_void) -> c_int,
    widget_set_value: unsafe extern "C" fn(Widget, *const c_void) -> c_int,
    widget_count_choices: unsafe extern "C" fn(Widget) -> c_int,
    widget_choice: unsafe extern "C" fn(Widget, c_int, *mut *const c_char) -> c_int,
    widget_range: unsafe extern "C" fn(Widget, *mut c_float, *mut c_float, *mut c_float) -> c_int,
    widget_unref: unsafe extern "C" fn(Widget) -> c_int,
    result_text: unsafe extern "C" fn(c_int) -> *const c_char,
    list_new: unsafe extern "C" fn(*mut *mut c_void) -> c_int,
    list_load: unsafe extern "C" fn(*mut c_void) -> c_int,
    list_lookup_path: unsafe extern "C" fn(*mut c_void, *const c_char) -> c_int,
    list_get_info: unsafe extern "C" fn(*mut c_void, c_int, *mut *mut c_void) -> c_int,
    list_free: unsafe extern "C" fn(*mut c_void) -> c_int,
    autodetect: unsafe extern "C" fn(*mut c_void, Ctx) -> c_int,
    cams_new: unsafe extern "C" fn(*mut *mut c_void) -> c_int,
    cams_count: unsafe extern "C" fn(*mut c_void) -> c_int,
    cams_name: unsafe extern "C" fn(*mut c_void, c_int, *mut *const c_char) -> c_int,
    cams_value: unsafe extern "C" fn(*mut c_void, c_int, *mut *const c_char) -> c_int,
    cams_free: unsafe extern "C" fn(*mut c_void) -> c_int,
}

/// Where the bundled libraries would be: BUNDLE_ENV, else lib/gphoto2 next to the program.
fn bundle_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os(BUNDLE_ENV) {
        return Some(PathBuf::from(d));
    }
    Some(std::env::current_exe().ok()?.parent()?.join("lib/gphoto2"))
}

fn sym<T: Copy>(lib: &Library, name: &str) -> Result<T, String> {
    let mut z = name.as_bytes().to_vec();
    z.push(0);
    // SAFETY: each use names the function with the signature libgphoto2 2.5 declares for it
    unsafe { lib.get::<T>(&*z).map(|s| *s).map_err(|e| e.to_string()) }
}

/// Loads the bundled copy from dir (its camera drivers in camlibs and iolibs beside it), or None when it isn't all there.
fn load_bundled(dir: &Path) -> Option<Library> {
    let (camlibs, iolibs) = (dir.join("camlibs"), dir.join("iolibs"));
    if !dir.join(SONAME).exists() || !camlibs.is_dir() || !iolibs.is_dir() {
        return None;
    }
    // What they need besides: the system's copy when it has one, else the bundled one. A few passes, since one can need another
    let mut left: Vec<String> = std::fs::read_dir(dir).ok()?.flatten().map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("lib") && n.contains(".so") && !OWN.contains(&n.as_str())).collect();
    for _ in 0..3 {
        left.retain(|name| {
            // SAFETY: loading runs the library's initialisers only, and it stays loaded for the whole run
            let loaded = unsafe { Library::new(name) }.or_else(|_| unsafe { Library::new(dir.join(name)) });
            match loaded {
                Ok(l) => {
                    std::mem::forget(l);
                    false
                }
                Err(_) => true,
            }
        });
    }
    // SAFETY: as above
    if let Ok(l) = unsafe { Library::new(dir.join(OWN[0])) } {
        std::mem::forget(l);
    }
    // SAFETY: as above
    let lib = unsafe { Library::new(dir.join(SONAME)) }.ok()?;
    // CAMLIBS and IOLIBS were set at startup (prepare_environment), before any thread could be reading the environment
    Some(lib)
}

/// Points libgphoto2 at the bundled camera drivers when there is a bundle. It has to run at startup before the program starts any
/// thread, since changing the environment while others read it isn't safe.
pub fn prepare_environment() {
    let Some(dir) = bundle_dir() else { return };
    let (camlibs, iolibs) = (dir.join("camlibs"), dir.join("iolibs"));
    if dir.join(SONAME).exists() && camlibs.is_dir() && iolibs.is_dir() {
        std::env::set_var("CAMLIBS", &camlibs);
        std::env::set_var("IOLIBS", &iolibs);
    }
}

fn load() -> Result<Api, String> {
    let lib = match bundle_dir().and_then(|d| load_bundled(&d)) {
        Some(l) => l,
        None if std::env::var_os(NO_SYSTEM_ENV).is_some() => return Err("libgphoto2 isn't bundled".into()),
        // SAFETY: as above
        None => unsafe { Library::new(SONAME) }.map_err(|e| e.to_string())?,
    };
    let api = Api {
        context_new: sym(&lib, "gp_context_new")?,
        context_unref: sym(&lib, "gp_context_unref")?,
        camera_new: sym(&lib, "gp_camera_new")?,
        camera_init: sym(&lib, "gp_camera_init")?,
        camera_exit: sym(&lib, "gp_camera_exit")?,
        camera_unref: sym(&lib, "gp_camera_unref")?,
        set_port_info: sym(&lib, "gp_camera_set_port_info")?,
        capture_preview: sym(&lib, "gp_camera_capture_preview")?,
        capture: sym(&lib, "gp_camera_capture")?,
        file_get: sym(&lib, "gp_camera_file_get")?,
        wait_for_event: sym(&lib, "gp_camera_wait_for_event")?,
        get_single_config: sym(&lib, "gp_camera_get_single_config")?,
        set_single_config: sym(&lib, "gp_camera_set_single_config")?,
        file_new: sym(&lib, "gp_file_new")?,
        file_unref: sym(&lib, "gp_file_unref")?,
        file_data: sym(&lib, "gp_file_get_data_and_size")?,
        widget_type: sym(&lib, "gp_widget_get_type")?,
        widget_readonly: sym(&lib, "gp_widget_get_readonly")?,
        widget_get_value: sym(&lib, "gp_widget_get_value")?,
        widget_set_value: sym(&lib, "gp_widget_set_value")?,
        widget_count_choices: sym(&lib, "gp_widget_count_choices")?,
        widget_choice: sym(&lib, "gp_widget_get_choice")?,
        widget_range: sym(&lib, "gp_widget_get_range")?,
        widget_unref: sym(&lib, "gp_widget_unref")?,
        result_text: sym(&lib, "gp_result_as_string")?,
        list_new: sym(&lib, "gp_port_info_list_new")?,
        list_load: sym(&lib, "gp_port_info_list_load")?,
        list_lookup_path: sym(&lib, "gp_port_info_list_lookup_path")?,
        list_get_info: sym(&lib, "gp_port_info_list_get_info")?,
        list_free: sym(&lib, "gp_port_info_list_free")?,
        autodetect: sym(&lib, "gp_camera_autodetect")?,
        cams_new: sym(&lib, "gp_list_new")?,
        cams_count: sym(&lib, "gp_list_count")?,
        cams_name: sym(&lib, "gp_list_get_name")?,
        cams_value: sym(&lib, "gp_list_get_value")?,
        cams_free: sym(&lib, "gp_list_free")?,
    };
    std::mem::forget(lib);
    Ok(api)
}

fn api() -> Result<&'static Api, String> {
    static API: OnceLock<Result<Api, String>> = OnceLock::new();
    API.get_or_init(load).as_ref().map_err(|e| e.clone())
}

/// Whether libgphoto2 could be loaded here.
pub fn available() -> bool {
    match api() {
        Ok(_) => true,
        Err(why) => {
            log::debug!("libgphoto2 isn't available, DSLRs go through the gphoto2 command: {why}");
            false
        }
    }
}

fn say(api: &Api, code: c_int) -> String {
    // SAFETY: gp_result_as_string returns a static string, or null for a code it doesn't know
    let p = unsafe { (api.result_text)(code) };
    if p.is_null() {
        format!("libgphoto2 error {code}")
    } else {
        unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
    }
}

fn check(api: &Api, code: c_int) -> Result<(), String> {
    if code < 0 { Err(say(api, code)) } else { Ok(()) }
}

/// (model, port) of each camera plugged in, as `gphoto2 --auto-detect` lists them.
pub fn auto_detect() -> Result<Vec<(String, String)>, String> {
    let api = api()?;
    // SAFETY: the list is made here, read while it lives (the strings are copied) and freed
    unsafe {
        let ctx = (api.context_new)();
        let mut list: *mut c_void = std::ptr::null_mut();
        if let Err(why) = check(api, (api.cams_new)(&mut list)) {
            (api.context_unref)(ctx);
            return Err(why);
        }
        let done = (|| {
            check(api, (api.autodetect)(list, ctx))?;
            let text = |p: *const c_char| if p.is_null() { String::new() } else { CStr::from_ptr(p).to_string_lossy().into_owned() };
            let mut found = Vec::new();
            for i in 0..(api.cams_count)(list).max(0) {
                let (mut name, mut value): (*const c_char, *const c_char) = (std::ptr::null(), std::ptr::null());
                if (api.cams_name)(list, i, &mut name) >= 0 && (api.cams_value)(list, i, &mut value) >= 0 {
                    found.push((text(name), text(value)));
                }
            }
            Ok(found)
        })();
        (api.cams_free)(list);
        (api.context_unref)(ctx);
        done
    }
}

/// A camera held open: only ever used from the thread that has it.
pub struct GpCamera {
    api: &'static Api,
    ctx: Ctx,
    cam: Cam,
}

// SAFETY: the pointers are only used by the one thread that owns this value, which is moved there once and never shared
unsafe impl Send for GpCamera {}

impl GpCamera {
    /// Opens the camera on this port ("usb:001,004", or "" for whichever is found).
    pub fn open(port: &str) -> Result<GpCamera, String> {
        let api = api()?;
        // SAFETY: plain calls with out-pointers to locals; what is made here is freed on every way out
        unsafe {
            let ctx = (api.context_new)();
            let mut cam: Cam = std::ptr::null_mut();
            if let Err(why) = check(api, (api.camera_new)(&mut cam)) {
                (api.context_unref)(ctx);
                return Err(why);
            }
            if !port.is_empty() {
                if let Err(why) = set_port(api, cam, port) {
                    log::debug!("gphoto2 port {port}: {why}");
                }
            }
            let code = (api.camera_init)(cam, ctx);
            if code < 0 {
                let why = say(api, code);
                (api.camera_unref)(cam);
                (api.context_unref)(ctx);
                return Err(why);
            }
            Ok(GpCamera { api, ctx, cam })
        }
    }

    /// The file the camera made at this path, copied over.
    fn download(&mut self, path: &FilePath) -> Result<Shot, String> {
        let api = self.api;
        // SAFETY: the camera is open; the path's strings end in a zero (the library wrote them, or it is the zeroed rest); the file is
        // made, filled by the call, copied while it lives and freed
        unsafe {
            let text = |p: &[c_char]| CStr::from_ptr(p.as_ptr()).to_string_lossy().into_owned();
            let mut file: File = std::ptr::null_mut();
            check(api, (api.file_new)(&mut file))?;
            let done = (|| {
                check(api, (api.file_get)(self.cam, path.folder.as_ptr(), path.name.as_ptr(), FILE_NORMAL, file, self.ctx))?;
                let (mut data, mut size): (*const c_char, c_ulong) = (std::ptr::null(), 0);
                check(api, (api.file_data)(file, &mut data, &mut size))?;
                if data.is_null() || size == 0 {
                    return Err("the camera sent an empty file".to_string());
                }
                Ok(Shot { name: text(&path.name), data: std::slice::from_raw_parts(data.cast::<u8>(), size as usize).to_vec() })
            })();
            (api.file_unref)(file);
            done
        }
    }

    fn widget(&mut self, name: &str) -> Result<Widget, String> {
        let name = CString::new(name).map_err(|e| e.to_string())?;
        let mut w: Widget = std::ptr::null_mut();
        // SAFETY: the camera is open; the widget out-pointer is a local
        check(self.api, unsafe { (self.api.get_single_config)(self.cam, name.as_ptr(), &mut w, self.ctx) })?;
        Ok(w)
    }

    fn describe(&self, w: Widget) -> Result<Setting, String> {
        let api = self.api;
        // SAFETY: w is a live widget from get_single_config; strings it hands back are copied before it is released
        unsafe {
            let (mut t, mut ro) = (0, 0);
            check(api, (api.widget_type)(w, &mut t))?;
            let _ = (api.widget_readonly)(w, &mut ro);
            let text = |p: *const c_char| if p.is_null() { String::new() } else { CStr::from_ptr(p).to_string_lossy().into_owned() };
            let mut s = Setting { kind: Kind::Other, readonly: ro != 0, value: String::new(), choices: Vec::new() };
            match t {
                WIDGET_TOGGLE => {
                    let mut v: c_int = 0;
                    check(api, (api.widget_get_value)(w, (&mut v as *mut c_int).cast()))?;
                    s.kind = Kind::Toggle;
                    s.value = (v != 0).to_string();
                }
                WIDGET_RANGE => {
                    let (mut v, mut lo, mut hi, mut step): (c_float, c_float, c_float, c_float) = (0.0, 0.0, 0.0, 0.0);
                    check(api, (api.widget_get_value)(w, (&mut v as *mut c_float).cast()))?;
                    check(api, (api.widget_range)(w, &mut lo, &mut hi, &mut step))?;
                    s.kind = Kind::Range { min: lo as f64, max: hi as f64, step: step as f64 };
                    s.value = v.to_string();
                }
                WIDGET_RADIO | WIDGET_MENU | WIDGET_TEXT => {
                    let mut p: *const c_char = std::ptr::null();
                    check(api, (api.widget_get_value)(w, (&mut p as *mut *const c_char).cast()))?;
                    s.value = text(p);
                    if t == WIDGET_TEXT {
                        s.kind = Kind::Text;
                    } else {
                        s.kind = Kind::Choice;
                        for i in 0..(api.widget_count_choices)(w).max(0) {
                            let mut c: *const c_char = std::ptr::null();
                            if (api.widget_choice)(w, i, &mut c) >= 0 {
                                s.choices.push(text(c));
                            }
                        }
                    }
                }
                _ => {}
            }
            Ok(s)
        }
    }
}

/// Points the camera at the USB port, when the library knows it.
///
/// # Safety
/// cam must be a camera made by camera_new.
unsafe fn set_port(api: &Api, cam: Cam, port: &str) -> Result<(), String> {
    let path = CString::new(port).map_err(|e| e.to_string())?;
    let mut list: *mut c_void = std::ptr::null_mut();
    check(api, (api.list_new)(&mut list))?;
    let done = (|| {
        check(api, (api.list_load)(list))?;
        let index = (api.list_lookup_path)(list, path.as_ptr());
        check(api, index)?;
        let mut info: *mut c_void = std::ptr::null_mut();
        check(api, (api.list_get_info)(list, index, &mut info))?;
        check(api, (api.set_port_info)(cam, info))
    })();
    (api.list_free)(list);
    done
}

impl Rig for GpCamera {
    fn preview(&mut self) -> Result<Vec<u8>, String> {
        let api = self.api;
        // SAFETY: the file is made, filled by the call, read while it lives (its data is copied), and freed
        unsafe {
            let mut file: File = std::ptr::null_mut();
            check(api, (api.file_new)(&mut file))?;
            let done = (|| {
                check(api, (api.capture_preview)(self.cam, file, self.ctx))?;
                let (mut data, mut size): (*const c_char, c_ulong) = (std::ptr::null(), 0);
                check(api, (api.file_data)(file, &mut data, &mut size))?;
                if data.is_null() || size == 0 {
                    return Err("the camera sent an empty frame".to_string());
                }
                Ok(std::slice::from_raw_parts(data.cast::<u8>(), size as usize).to_vec())
            })();
            (api.file_unref)(file);
            done
        }
    }

    fn read(&mut self, name: &str) -> Option<Setting> {
        let w = self.widget(name).ok()?;
        let s = self.describe(w);
        // SAFETY: w came from get_single_config and isn't used again
        unsafe { (self.api.widget_unref)(w) };
        s.ok()
    }

    fn write(&mut self, name: &str, value: &str) -> Result<(), String> {
        let w = self.widget(name)?;
        let api = self.api;
        let done = (|| {
            let setting = self.describe(w)?;
            let cname = CString::new(name).map_err(|e| e.to_string())?;
            // SAFETY: w is live; each value is a pointer to a local of the type that kind of widget takes
            unsafe {
                match setting.kind {
                    Kind::Toggle => {
                        let v: c_int = (value == "1" || value == "true") as c_int;
                        check(api, (api.widget_set_value)(w, (&v as *const c_int).cast()))?;
                    }
                    Kind::Range { .. } => {
                        let v: c_float = value.parse().map_err(|_| format!("{value} isn't a number"))?;
                        check(api, (api.widget_set_value)(w, (&v as *const c_float).cast()))?;
                    }
                    _ => {
                        let v = CString::new(value).map_err(|e| e.to_string())?;
                        check(api, (api.widget_set_value)(w, v.as_ptr().cast()))?;
                    }
                }
                check(api, (api.set_single_config)(self.cam, cname.as_ptr(), w, self.ctx))
            }
        })();
        // SAFETY: w came from get_single_config and isn't used again
        unsafe { (api.widget_unref)(w) };
        done
    }

    fn capture(&mut self, expect: usize) -> Result<Vec<Shot>, String> {
        let api = self.api;
        // SAFETY: the path is a local the call fills in; each file is made, filled, copied and freed in download(); an event's data is
        // malloc'd by the library, read as the file path it is for a file event and freed here once
        unsafe {
            let mut path = FilePath { name: [0; 128], folder: [0; 1024] };
            check(api, (api.capture)(self.cam, CAPTURE_IMAGE, &mut path, self.ctx))?;
            let mut shots = vec![self.download(&path)?];
            // A camera that makes RAW and JPEG says about the second one as an event, a moment after the first
            let mut waited = 0;
            while shots.len() < expect && waited < SECOND_FILE_WAIT_MS {
                let (mut kind, mut data): (c_int, *mut c_void) = (0, std::ptr::null_mut());
                if (api.wait_for_event)(self.cam, 250, &mut kind, &mut data, self.ctx) < 0 {
                    break;
                }
                match kind {
                    EVENT_TIMEOUT => waited += 250,
                    EVENT_FILE_ADDED if !data.is_null() => match self.download(&*(data as *const FilePath)) {
                        Ok(shot) => shots.push(shot),
                        Err(why) => log::debug!("the camera's second file wouldn't come: {why}"),
                    },
                    _ => {}
                }
                if !data.is_null() {
                    libc::free(data);
                }
                if kind == EVENT_CAPTURE_COMPLETE {
                    break;
                }
            }
            Ok(shots)
        }
    }

    fn restart_live_view(&mut self) {
        // The camera ended it (it does every few minutes): off and on again
        let _ = self.write("viewfinder", "0");
        let _ = self.write("viewfinder", "1");
    }

    fn close(&mut self) {
        if self.cam.is_null() {
            return;
        }
        // Live view off first, so the mirror goes back down
        let _ = self.write("viewfinder", "0");
        // SAFETY: the camera is open and isn't used again
        unsafe {
            (self.api.camera_exit)(self.cam, self.ctx);
            (self.api.camera_unref)(self.cam);
            (self.api.context_unref)(self.ctx);
        }
        self.cam = std::ptr::null_mut();
    }
}

impl Drop for GpCamera {
    fn drop(&mut self) {
        self.close();
    }
}
