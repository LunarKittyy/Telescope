//! MediaPipe from the vision pack (vision.MediaPipeBackend): its own C library, site/mediapipe/tasks/c/libmediapipe, which the
//! Python app's MediaPipe 1.1 calls through ctypes. Same library, same models, same options, so the masks and people come out
//! as Python's do. The models are read into memory (a path with non-ASCII characters fails on Windows).

use crate::ops::{convert_scale_abs, Rgb};
use crate::pack::{POSE_MODEL, SEGMENTER_MODEL};
use crate::service::{Backend, Gray, PoseModel, Segmenter, VisionError};
use libloading::Library;
use std::ffi::{c_char, c_int, c_uint, c_void, CStr, CString};
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::Arc;
use telescope_core::pose::Person;

/// Pose landmarks we use, by their index in MediaPipe's 33.
pub const POINTS: [(&str, usize); 23] = [
    ("nose", 0), ("left_eye", 2), ("right_eye", 5), ("left_ear", 7), ("right_ear", 8),
    ("left_shoulder", 11), ("right_shoulder", 12), ("left_elbow", 13), ("right_elbow", 14), ("left_wrist", 15), ("right_wrist", 16),
    ("left_pinky", 17), ("right_pinky", 18), ("left_index", 19), ("right_index", 20), ("left_thumb", 21), ("right_thumb", 22),
    ("left_hip", 23), ("right_hip", 24), ("left_knee", 25), ("right_knee", 26), ("left_ankle", 27), ("right_ankle", 28),
];
/// How many people the pose model looks for.
pub const MAX_PEOPLE: c_int = 3;
const VIDEO: c_int = 2;
const SRGB: c_int = 1;
const VEC32F1: c_int = 9;
const HOST_SYSTEM: c_int = if cfg!(windows) { 3 } else if cfg!(target_os = "macos") { 2 } else { 1 };

#[repr(C)]
struct BaseOptions {
    model_asset_buffer: *const c_char,
    model_asset_buffer_count: c_uint,
    model_asset_path: *const c_char,
    file_descriptor: c_int,
    delegate: c_int,
    host_environment: c_int,
    host_system: c_int,
    host_version: *const c_char,
    ca_bundle_path: *const c_char,
    app_id: *const c_char,
    app_version: *const c_char,
}

type ResultCallback = Option<unsafe extern "C" fn(i32, *const c_void, *const c_void, i64)>;

#[repr(C)]
struct SegmenterOptions {
    base_options: BaseOptions,
    running_mode: c_int,
    display_names_locale: *const c_char,
    output_confidence_masks: bool,
    output_category_mask: bool,
    result_callback: ResultCallback,
}

#[repr(C)]
struct SegmenterResult {
    confidence_masks: *mut *mut c_void,
    confidence_masks_count: u32,
    category_mask: *mut c_void,
    has_category_mask: u32,
    quality_scores: *mut f32,
    quality_scores_count: u32,
}

#[repr(C)]
struct PoseOptions {
    base_options: BaseOptions,
    running_mode: c_int,
    num_poses: c_int,
    min_pose_detection_confidence: f32,
    min_pose_presence_confidence: f32,
    min_tracking_confidence: f32,
    output_segmentation_masks: bool,
    result_callback: ResultCallback,
}

#[repr(C)]
struct Landmark {
    x: f32,
    y: f32,
    z: f32,
    has_visibility: bool,
    visibility: f32,
    has_presence: bool,
    presence: f32,
    name: *const c_char,
}

#[repr(C)]
struct Landmarks {
    landmarks: *mut Landmark,
    landmarks_count: u32,
}

#[repr(C)]
struct PoseResult {
    segmentation_masks: *mut *mut c_void,
    segmentation_masks_count: u32,
    pose_landmarks: *mut Landmarks,
    pose_landmarks_count: u32,
    pose_world_landmarks: *mut c_void,
    pose_world_landmarks_count: u32,
}

type Err = *mut *mut c_char;

/// The C functions we call, looked up once.
struct Api {
    // never unloaded, as ctypes never unloads it: MediaPipe's own threads still run code in it when the process ends
    _lib: std::mem::ManuallyDrop<Library>,
    error_free: unsafe extern "C" fn(*mut c_char),
    image_from_u8: unsafe extern "C" fn(c_int, c_int, c_int, *const u8, c_int, *mut *mut c_void, Err) -> c_int,
    image_free: unsafe extern "C" fn(*mut c_void),
    image_width: unsafe extern "C" fn(*mut c_void) -> c_int,
    image_height: unsafe extern "C" fn(*mut c_void) -> c_int,
    image_width_step: unsafe extern "C" fn(*mut c_void) -> c_int,
    image_format: unsafe extern "C" fn(*mut c_void) -> c_int,
    image_f32: unsafe extern "C" fn(*mut c_void, *mut *const f32, Err) -> c_int,
    seg_create: unsafe extern "C" fn(*const SegmenterOptions, *mut *mut c_void, Err) -> c_int,
    seg_video: unsafe extern "C" fn(*mut c_void, *mut c_void, *const c_void, i64, *mut SegmenterResult, Err) -> c_int,
    seg_close_result: unsafe extern "C" fn(*mut SegmenterResult),
    seg_close: unsafe extern "C" fn(*mut c_void, Err) -> c_int,
    pose_create: unsafe extern "C" fn(*const PoseOptions, *mut *mut c_void, Err) -> c_int,
    pose_video: unsafe extern "C" fn(*mut c_void, *mut c_void, *const c_void, i64, *mut PoseResult, Err) -> c_int,
    pose_close_result: unsafe extern "C" fn(*mut PoseResult),
    pose_close: unsafe extern "C" fn(*mut c_void, Err) -> c_int,
}

/// Python's exception type for an MpStatus (mediapipe_c_utils.convert_to_exception), so problem() reads the same.
fn status_kind(status: c_int) -> &'static str {
    match status {
        1 | 4 => "TimeoutError",
        3 => "ValueError",
        5 => "FileNotFoundError",
        6 => "FileExistsError",
        7 | 16 => "PermissionError",
        11 => "IndexError",
        12 => "NotImplementedError",
        _ => "RuntimeError",
    }
}

impl Api {
    fn load(path: &Path) -> Result<Api, VisionError> {
        let lib = open(path).map_err(|e| VisionError::new("OSError", &e.to_string()))?;
        macro_rules! sym {
            ($name:literal) => {
                *unsafe { lib.get(concat!($name, "\0").as_bytes()) }.map_err(|e| VisionError::new("AttributeError", &e.to_string()))?
            };
        }
        Ok(Api {
            error_free: sym!("MpErrorFree"),
            image_from_u8: sym!("MpImageCreateFromUint8Data"),
            image_free: sym!("MpImageFree"),
            image_width: sym!("MpImageGetWidth"),
            image_height: sym!("MpImageGetHeight"),
            image_width_step: sym!("MpImageGetWidthStep"),
            image_format: sym!("MpImageGetFormat"),
            image_f32: sym!("MpImageDataFloat32"),
            seg_create: sym!("MpImageSegmenterCreate"),
            seg_video: sym!("MpImageSegmenterSegmentForVideo"),
            seg_close_result: sym!("MpImageSegmenterCloseResult"),
            seg_close: sym!("MpImageSegmenterClose"),
            pose_create: sym!("MpPoseLandmarkerCreate"),
            pose_video: sym!("MpPoseLandmarkerDetectForVideo"),
            pose_close_result: sym!("MpPoseLandmarkerCloseResult"),
            pose_close: sym!("MpPoseLandmarkerClose"),
            _lib: std::mem::ManuallyDrop::new(lib),
        })
    }

    /// A call that returns an MpStatus and may hand back an error message, as CStatusFunction makes them.
    fn check(&self, call: impl FnOnce(Err) -> c_int) -> Result<(), VisionError> {
        let mut message: *mut c_char = ptr::null_mut();
        let status = call(&mut message);
        let text = if message.is_null() {
            String::new()
        } else {
            let t = unsafe { CStr::from_ptr(message) }.to_string_lossy().into_owned();
            unsafe { (self.error_free)(message) };
            t
        };
        if status == 0 { Ok(()) } else { Err(VisionError::new(status_kind(status), &text)) }
    }

    fn image(&self, frame: &Rgb) -> Result<Image<'_>, VisionError> {
        let mut out = ptr::null_mut();
        self.check(|err| unsafe {
            (self.image_from_u8)(SRGB, frame.w as c_int, frame.h as c_int, frame.data.as_ptr(), frame.data.len() as c_int, &mut out, err)
        })?;
        Ok(Image { api: self, ptr: out })
    }

    /// A one-channel float mask as 0..255 (cv2.convertScaleAbs(mask, alpha=255)).
    fn mask(&self, image: *mut c_void) -> Result<Gray, VisionError> {
        let (w, h) = unsafe { ((self.image_width)(image) as usize, (self.image_height)(image) as usize) };
        if unsafe { (self.image_format)(image) } != VEC32F1 {
            return Err(VisionError::new("ValueError", "the mask isn't one float a pixel"));
        }
        let mut data: *const f32 = ptr::null();
        self.check(|err| unsafe { (self.image_f32)(image, &mut data, err) })?;
        let step = unsafe { (self.image_width_step)(image) } as usize / 4;
        let step = if step >= w { step } else { w };
        let mut values = Vec::with_capacity(w * h);
        for y in 0..h {
            values.extend_from_slice(unsafe { std::slice::from_raw_parts(data.add(y * step), w) });
        }
        Ok(Gray { w, h, data: convert_scale_abs(&values, 255.0) })
    }
}

/// Opens the library the way ctypes.CDLL does, so its own folder is searched for what it needs on Windows.
fn open(path: &Path) -> Result<Library, libloading::Error> {
    #[cfg(windows)]
    {
        use libloading::os::windows::{Library as WinLibrary, LOAD_LIBRARY_SEARCH_DEFAULT_DIRS, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR};
        unsafe { WinLibrary::load_with_flags(path, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS) }.map(Library::from)
    }
    #[cfg(not(windows))]
    {
        unsafe { Library::new(path) }
    }
}

struct Image<'a> {
    api: &'a Api,
    ptr: *mut c_void,
}

impl Drop for Image<'_> {
    fn drop(&mut self) {
        unsafe { (self.api.image_free)(self.ptr) };
    }
}

/// MediaPipe loaded from one pack folder.
pub struct MediaPipe {
    api: Arc<Api>,
    segmenter_model: Arc<Vec<u8>>,
    pose_model: Arc<Vec<u8>>,
    strings: Arc<Strings>,
    version: String,
}

/// The strings BaseOptions points at, alive as long as the backend.
struct Strings {
    host_version: CString,
    ca_bundle: Option<CString>,
}

// Safety: the library and the models are only read; each task is used by one thread at a time (Segmenter and PoseModel are Send, not Sync).
unsafe impl Send for Api {}
unsafe impl Sync for Api {}

/// The library file inside a pack.
pub fn library_path(folder: &Path) -> PathBuf {
    let name = if cfg!(windows) { "libmediapipe.dll" } else if cfg!(target_os = "macos") { "libmediapipe.dylib" } else { "libmediapipe.so" };
    folder.join("site").join("mediapipe").join("tasks").join("c").join(name)
}

fn version_of(folder: &Path) -> String {
    let site = folder.join("site");
    let Ok(entries) = std::fs::read_dir(&site) else { return "?".into() };
    entries.flatten().filter_map(|e| {
        let name = e.file_name().to_string_lossy().into_owned();
        name.strip_prefix("mediapipe-").and_then(|v| v.strip_suffix(".dist-info")).map(str::to_string)
    }).next().unwrap_or_else(|| "?".into())
}

impl MediaPipe {
    /// Loads the library and reads both models; makes and closes a segmenter so the native parts load now, not on the first frame.
    pub fn load(folder: &Path) -> Result<MediaPipe, crate::service::LoadError> {
        use crate::service::LoadError;
        let read = |name: &str| std::fs::read(folder.join("models").join(name)).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound { LoadError::Missing(format!("no {name} in the pack")) } else { LoadError::Failed(VisionError::new("OSError", &e.to_string())) }
        });
        let segmenter_model = Arc::new(read(SEGMENTER_MODEL)?);
        let pose_model = Arc::new(read(POSE_MODEL)?);
        let api = Arc::new(Api::load(&library_path(folder)).map_err(LoadError::Failed)?);
        let cert = folder.join("site").join("certifi").join("cacert.pem");
        let strings = Arc::new(Strings {
            host_version: CString::new(format!("telescope-rs-{}", telescope_core::version::version())).unwrap_or_default(),
            ca_bundle: cert.exists().then(|| CString::new(cert.to_string_lossy().into_owned()).ok()).flatten(),
        });
        let backend = MediaPipe { api, segmenter_model, pose_model, strings, version: version_of(folder) };
        drop(backend.segmenter().map_err(LoadError::Failed)?);
        Ok(backend)
    }

    fn base(&self, model: &[u8]) -> BaseOptions {
        BaseOptions {
            model_asset_buffer: model.as_ptr() as *const c_char,
            model_asset_buffer_count: model.len() as c_uint,
            model_asset_path: ptr::null(),
            file_descriptor: -1,
            delegate: 0,
            host_environment: 0,
            host_system: HOST_SYSTEM,
            host_version: self.strings.host_version.as_ptr(),
            ca_bundle_path: self.strings.ca_bundle.as_ref().map_or(ptr::null(), |c| c.as_ptr()),
            app_id: ptr::null(),
            app_version: ptr::null(),
        }
    }
}

impl Backend for MediaPipe {
    fn version(&self) -> String {
        self.version.clone()
    }

    fn segmenter(&self) -> Result<Box<dyn Segmenter>, VisionError> {
        let locale = c"";
        let options = SegmenterOptions {
            base_options: self.base(&self.segmenter_model),
            running_mode: VIDEO,
            display_names_locale: locale.as_ptr(),
            output_confidence_masks: true,
            output_category_mask: false,
            result_callback: None,
        };
        let mut handle = ptr::null_mut();
        self.api.check(|err| unsafe { (self.api.seg_create)(&options, &mut handle, err) })?;
        Ok(Box::new(MpSegmenter { api: self.api.clone(), handle, _model: self.segmenter_model.clone() }))
    }

    fn pose(&self) -> Result<Box<dyn PoseModel>, VisionError> {
        let options = PoseOptions {
            base_options: self.base(&self.pose_model),
            running_mode: VIDEO,
            num_poses: MAX_PEOPLE,
            min_pose_detection_confidence: 0.5,
            min_pose_presence_confidence: 0.5,
            min_tracking_confidence: 0.5,
            output_segmentation_masks: false,
            result_callback: None,
        };
        let mut handle = ptr::null_mut();
        self.api.check(|err| unsafe { (self.api.pose_create)(&options, &mut handle, err) })?;
        Ok(Box::new(MpPose { api: self.api.clone(), handle, _model: self.pose_model.clone() }))
    }
}

struct MpSegmenter {
    api: Arc<Api>,
    handle: *mut c_void,
    _model: Arc<Vec<u8>>,
}

// Safety: a task is used from one thread at a time, as Python's SerialDispatcher does.
unsafe impl Send for MpSegmenter {}

impl Segmenter for MpSegmenter {
    fn segment(&mut self, frame: &Rgb, ts_ms: i64) -> Result<Gray, VisionError> {
        let api = &*self.api;
        let image = api.image(frame)?;
        let mut result = SegmenterResult { confidence_masks: ptr::null_mut(), confidence_masks_count: 0, category_mask: ptr::null_mut(),
                                           has_category_mask: 0, quality_scores: ptr::null_mut(), quality_scores_count: 0 };
        api.check(|err| unsafe { (api.seg_video)(self.handle, image.ptr, ptr::null(), ts_ms, &mut result, err) })?;
        let mask = if result.confidence_masks_count == 0 {
            Err(VisionError::new("IndexError", "list index out of range"))
        } else {
            api.mask(unsafe { *result.confidence_masks })
        };
        unsafe { (api.seg_close_result)(&mut result) };
        mask
    }
}

impl Drop for MpSegmenter {
    fn drop(&mut self) {
        let api = &*self.api;
        if let Err(e) = api.check(|err| unsafe { (api.seg_close)(self.handle, err) }) {
            log::warn!("Closing the person segmenter failed: {e:?}");
        }
    }
}

struct MpPose {
    api: Arc<Api>,
    handle: *mut c_void,
    _model: Arc<Vec<u8>>,
}

// Safety: as MpSegmenter.
unsafe impl Send for MpPose {}

impl PoseModel for MpPose {
    fn detect(&mut self, frame: &Rgb, ts_ms: i64) -> Result<Vec<Person>, VisionError> {
        let api = &*self.api;
        let image = api.image(frame)?;
        let mut result = PoseResult { segmentation_masks: ptr::null_mut(), segmentation_masks_count: 0, pose_landmarks: ptr::null_mut(),
                                      pose_landmarks_count: 0, pose_world_landmarks: ptr::null_mut(), pose_world_landmarks_count: 0 };
        api.check(|err| unsafe { (api.pose_video)(self.handle, image.ptr, ptr::null(), ts_ms, &mut result, err) })?;
        let mut people = Vec::new();
        for i in 0..result.pose_landmarks_count as usize {
            let lms = unsafe { &*result.pose_landmarks.add(i) };
            let all = unsafe { std::slice::from_raw_parts(lms.landmarks, lms.landmarks_count as usize) };
            if all.len() < 33 {
                continue;
            }
            people.push(Person::new(POINTS.iter().map(|&(name, at)| {
                let lm = &all[at];
                (name, lm.x as f64, lm.y as f64, if lm.has_visibility { lm.visibility as f64 } else { 0.0 })
            })));
        }
        unsafe { (api.pose_close_result)(&mut result) };
        Ok(people)
    }
}

impl Drop for MpPose {
    fn drop(&mut self) {
        let api = &*self.api;
        if let Err(e) = api.check(|err| unsafe { (api.pose_close)(self.handle, err) }) {
            log::warn!("Closing the pose model failed: {e:?}");
        }
    }
}
