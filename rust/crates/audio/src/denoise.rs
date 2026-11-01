//! RNNoise noise reduction for the microphone (denoise.py): 48 kHz mono in 480-sample frames, the same audio /v1/audio
//! sends. The library is loaded at run time from next to the program, as Python loaded it from the pyrnnoise wheel's
//! folder, so a build without it still runs and the card says why.

use telescope_core::i18n::Fields;
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use telescope_core::i18n::tr;

pub const RATE: usize = 48_000;
/// Samples RNNoise takes at a time (10 ms).
pub const FRAME: usize = 480;
/// RNNoise's own lookahead: what comes out is 20 ms behind what went in.
pub const DELAY: usize = 2 * FRAME;

/// One frame at a time, in place of rnnoise_process_frame: float samples at 16-bit scale.
pub trait FrameProcessor: Send {
    fn process_frame(&mut self, out: &mut [f32], input: &[f32]);
}

type Create = unsafe extern "C" fn(*mut c_void) -> *mut c_void;
type Destroy = unsafe extern "C" fn(*mut c_void);
type Process = unsafe extern "C" fn(*mut c_void, *mut f32, *const f32) -> f32;
type FrameSize = unsafe extern "C" fn() -> i32;

/// The loaded library and the three calls the denoiser makes.
pub struct Library {
    create: Create,
    destroy: Destroy,
    process: Process,
    _lib: libloading::Library,
}

/// Why the library can't be had: ("missing", "") or ("failed", the error).
pub type Problem = (&'static str, String);

#[cfg(windows)]
const LIBRARY: &str = "rnnoise.dll";
#[cfg(not(windows))]
const LIBRARY: &str = "librnnoise.so";

/// Where the library may be: next to the program in a pyrnnoise folder (as the frozen Python build had it), or
/// beside the program itself.
pub fn library_paths() -> Vec<PathBuf> {
    let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)) else { return Vec::new() };
    vec![dir.join("pyrnnoise").join(LIBRARY), dir.join(LIBRARY)]
}

/// The library, or why it can't be loaded.
pub fn open_library(paths: &[PathBuf]) -> Result<Library, Problem> {
    let Some(path) = paths.iter().find(|p| p.is_file()) else { return Err(("missing", String::new())) };
    // SAFETY: the library is RNNoise, and these are its C calls with their own signatures
    let loaded = unsafe {
        libloading::Library::new(path).and_then(|lib| {
            let create = *lib.get::<Create>(b"rnnoise_create\0")?;
            let destroy = *lib.get::<Destroy>(b"rnnoise_destroy\0")?;
            let process = *lib.get::<Process>(b"rnnoise_process_frame\0")?;
            let size = (*lib.get::<FrameSize>(b"rnnoise_get_frame_size\0")?)();
            Ok((Library { create, destroy, process, _lib: lib }, size))
        })
    };
    match loaded {
        Ok((_, size)) if size != FRAME as i32 => {
            let e = format!("it works in frames of {size} samples, not {FRAME}");
            log::warn!("Couldn't load RNNoise from {}: {e}", path.display());
            Err(("failed", e))
        }
        Ok((lib, _)) => {
            log::info!("RNNoise loaded from {}", path.display());
            Ok(lib)
        }
        Err(e) => {
            log::warn!("Couldn't load RNNoise from {}: {e}", path.display());
            Err(("failed", e.to_string()))
        }
    }
}

static LOADED: OnceLock<Result<Arc<Library>, Problem>> = OnceLock::new();

/// The RNNoise library, loaded once; None when it can't be, and problem() says why.
pub fn load() -> Option<Arc<Library>> {
    LOADED.get_or_init(|| open_library(&library_paths()).map(Arc::new)).as_ref().ok().cloned()
}

/// Why noise reduction isn't available, in the current language, or "" when it is.
pub fn problem() -> String {
    load();
    problem_of(LOADED.get())
}

fn problem_of<T>(loaded: Option<&Result<T, Problem>>) -> String {
    match loaded {
        Some(Err(p)) => problem_text(p),
        _ => String::new(),
    }
}

pub fn problem_text((kind, error): &Problem) -> String {
    if *kind == "missing" {
        return tr("RNNoise is missing from this copy of Telescope. Reinstall to get it back.");
    }
    tr("Couldn't load RNNoise: {error}").field("error", error)
}

/// One RNNoise state.
pub struct Rnnoise {
    lib: Arc<Library>,
    state: *mut c_void,
}

// SAFETY: an RNNoise state is plain memory, used from one thread at a time
unsafe impl Send for Rnnoise {}

impl Rnnoise {
    pub fn new(lib: Arc<Library>) -> Result<Rnnoise, String> {
        // SAFETY: a null model is RNNoise's built-in one
        let state = unsafe { (lib.create)(std::ptr::null_mut()) };
        if state.is_null() {
            return Err("rnnoise_create failed".into());
        }
        Ok(Rnnoise { lib, state })
    }
}

impl FrameProcessor for Rnnoise {
    fn process_frame(&mut self, out: &mut [f32], input: &[f32]) {
        // SAFETY: both are FRAME samples long, which is what the library said it takes
        unsafe { (self.lib.process)(self.state, out.as_mut_ptr(), input.as_ptr()) };
    }
}

impl Drop for Rnnoise {
    fn drop(&mut self) {
        // SAFETY: made by this library's create, destroyed once
        unsafe { (self.lib.destroy)(self.state) };
    }
}

/// What the writer cleans its chunks with: as many samples back as went in.
pub trait Denoise: Send {
    fn process(&mut self, samples: &[f32]) -> Vec<f32>;
}

/// One stream's RNNoise state. Whole frames (the writer's 10 ms chunks) go straight through, and anything else waits
/// for the rest of its frame.
pub struct Denoiser<P: FrameProcessor> {
    state: Option<P>,
    /// Under a frame, waiting for the rest.
    pending: Vec<f32>,
    /// Cleaned, not handed back yet.
    out: Vec<f32>,
    padded: bool,
}

impl<P: FrameProcessor> Denoiser<P> {
    pub fn new(state: P) -> Denoiser<P> {
        Denoiser { state: Some(state), pending: Vec::new(), out: Vec::new(), padded: false }
    }

    /// Lets go of the state; again does nothing.
    pub fn close(&mut self) {
        self.state = None;
    }
}

impl<P: FrameProcessor> Denoise for Denoiser<P> {
    fn process(&mut self, samples: &[f32]) -> Vec<f32> {
        let mut all = std::mem::take(&mut self.pending);
        all.extend_from_slice(samples);
        let whole = all.len() - all.len() % FRAME;
        let mut out = std::mem::take(&mut self.out);
        let mut frame = vec![0f32; FRAME];
        for chunk in all[..whole].as_chunks::<FRAME>().0 {
            match self.state.as_mut() {
                Some(state) => state.process_frame(&mut frame, chunk),
                None => frame.copy_from_slice(chunk),
            }
            out.extend_from_slice(&frame);
        }
        self.pending = all[whole..].to_vec();
        if !self.pending.is_empty() && !self.padded {
            self.padded = true; // chunks that don't line up with frames: run most of a frame later from here on
            let mut shifted = vec![0f32; FRAME - 1];
            shifted.extend(out);
            out = shifted;
        }
        let n = samples.len();
        if out.len() < n {
            out.resize(n, 0.0);
        }
        self.out = out.split_off(n);
        out
    }
}

/// A fresh denoiser, or None without the library.
pub fn make() -> Result<Option<Box<dyn Denoise>>, String> {
    match load() {
        Some(lib) => Ok(Some(Box::new(Denoiser::new(Rnnoise::new(lib)?)))),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// Stands in for RNNoise: hands each frame back unchanged and keeps a copy of what it was given.
    #[derive(Clone, Default)]
    struct Echo {
        frames: Arc<Mutex<Vec<Vec<f32>>>>,
        dropped: Arc<Mutex<u32>>,
    }

    struct EchoState(Echo);

    impl FrameProcessor for EchoState {
        fn process_frame(&mut self, out: &mut [f32], input: &[f32]) {
            self.0.frames.lock().unwrap().push(input.to_vec());
            out.copy_from_slice(input);
        }
    }

    impl Drop for EchoState {
        fn drop(&mut self) {
            *self.0.dropped.lock().unwrap() += 1;
        }
    }

    fn ramp(n: usize) -> Vec<f32> {
        (1..=n).map(|v| v as f32).collect()
    }

    #[test]
    fn the_stream_is_already_what_rnnoise_takes_so_nothing_is_resampled_or_held_back() {
        // Phones and the Browser camera both send 48 kHz mono, and the writer's 10 ms chunk is exactly one frame
        assert_eq!((crate::jitter::RATE, RATE), (48_000, 48_000));
        assert_eq!((crate::jitter::CHUNK, FRAME), (480, 480));
    }

    #[test]
    fn whole_frames_go_straight_through_with_no_delay() {
        let lib = Echo::default();
        let mut d = Denoiser::new(EchoState(lib.clone()));
        let x = ramp(FRAME * 6);
        let out: Vec<f32> = x.chunks(FRAME * 2).flat_map(|c| d.process(c)).collect();
        assert_eq!(out, x);
        let frames = lib.frames.lock().unwrap();
        assert!(frames.len() == 6 && frames.iter().all(|f| f.len() == FRAME));
        assert_eq!(frames.concat(), x); // in order, none skipped or repeated
    }

    #[test]
    fn uneven_chunks_come_out_in_order_just_under_a_frame_late() {
        let lib = Echo::default();
        let mut d = Denoiser::new(EchoState(lib.clone()));
        let x = ramp(FRAME * 20);
        let sizes = [300, 333, 7, 480, 1000, 1, 479, 960];
        let (mut out, mut at, mut k) = (Vec::new(), 0, 0);
        while at < x.len() {
            let n = sizes[k % sizes.len()].min(x.len() - at);
            let piece = d.process(&x[at..at + n]);
            assert_eq!(piece.len(), n); // always as much out as went in
            out.extend(piece);
            at += n;
            k += 1;
        }
        let mut expected = vec![0f32; FRAME - 1];
        expected.extend(&x);
        expected.truncate(x.len());
        assert_eq!(out, expected);
        let frames = lib.frames.lock().unwrap();
        assert_eq!(frames.concat(), x[..frames.len() * FRAME].to_vec());
    }

    #[test]
    fn once_chunks_stop_lining_up_it_shifts_once_and_never_again() {
        let mut d = Denoiser::new(EchoState(Echo::default()));
        let x = ramp(FRAME * 10);
        let first = d.process(&x[..FRAME * 2]);
        assert_eq!(first, x[..FRAME * 2].to_vec());
        let mut out = first;
        for i in (FRAME * 2..x.len()).step_by(250) {
            out.extend(d.process(&x[i..(i + 250).min(x.len())]));
        }
        let mut expected = x[..FRAME * 2].to_vec();
        expected.extend(vec![0f32; FRAME - 1]);
        expected.extend(&x[FRAME * 2..]);
        expected.truncate(x.len());
        assert_eq!(out, expected);
    }

    #[test]
    fn close_frees_the_state_once() {
        let lib = Echo::default();
        let mut d = Denoiser::new(EchoState(lib.clone()));
        d.close();
        d.close();
        drop(d);
        assert_eq!(*lib.dropped.lock().unwrap(), 1);
    }

    static DESTROYED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

    #[test]
    fn close_frees_the_librarys_state_once() {
        use std::sync::atomic::Ordering;
        unsafe extern "C" fn create(_model: *mut c_void) -> *mut c_void {
            std::ptr::dangling_mut::<u8>().cast()
        }
        unsafe extern "C" fn destroy(_state: *mut c_void) {
            DESTROYED.fetch_add(1, Ordering::SeqCst);
        }
        unsafe extern "C" fn process(_state: *mut c_void, _out: *mut f32, _input: *const f32) -> f32 {
            0.0
        }
        // SAFETY: the program's own symbols, opened only so a Library has something to hold on to
        #[cfg(unix)]
        let own: libloading::Library = libloading::os::unix::Library::this().into();
        #[cfg(windows)]
        let own: libloading::Library = libloading::os::windows::Library::this().unwrap().into();
        let lib = Arc::new(Library { create, destroy, process, _lib: own });
        let mut d = Denoiser::new(Rnnoise::new(lib).unwrap());
        d.close();
        d.close();
        drop(d);
        assert_eq!(DESTROYED.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_missing_library_says_the_package_isnt_installed() {
        let dir = std::env::temp_dir().join(format!("telescope-rnnoise-missing-{}", std::process::id()));
        assert_eq!(open_library(&[dir.join(LIBRARY)]).err(), Some(("missing", String::new())));
    }

    #[test]
    fn a_library_that_wont_load_says_why() {
        let dir = std::env::temp_dir().join(format!("telescope-rnnoise-broken-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let broken = dir.join(LIBRARY);
        std::fs::write(&broken, b"not a library").unwrap();
        let (kind, error) = open_library(&[broken]).err().unwrap();
        assert!(kind == "failed" && !error.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn problem_explains_each_case() {
        assert!(problem_text(&("missing", String::new())).contains("Reinstall"));
        assert!(problem_text(&("failed", "bad ELF".into())).contains("bad ELF"));
        assert!(problem_of::<()>(Some(&Err(("missing", String::new())))).contains("Reinstall"));
        assert!(problem_of::<()>(Some(&Err(("failed", "bad ELF".into())))).contains("bad ELF"));
        assert_eq!(problem_of(Some(&Ok(()))), "");
        // whatever this machine has, what it says is what load found
        assert_eq!(problem().is_empty(), load().is_some());
    }
}
