//! The high quality cutout (Background's switch for it): Robust Video Matting, run by ONNX Runtime on the processor. Both come from the
//! matting download (pack::Kind::Matting), not from the program: the library is loaded from its folder at run time, so a computer that never
//! switches the cutout on never loads it. The model keeps four recurrent states from frame to frame, which is what makes its edges steady,
//! so each stream has a segmenter of its own and its states start again whenever the picture could have jumped.

use crate::ops::{model_input, Rgb};
use crate::service::{Backend, Gray, LoadError, PoseModel, Segmenter, VisionError};
use ort::environment::Environment;
use ort::session::{builder::GraphOptimizationLevel, Session, SessionInputValue};
use ort::value::{DynValue, Tensor};
use std::borrow::Cow;
use std::path::Path;
use std::sync::Arc;

pub const MODEL: &str = "rvm_mobilenetv3_fp32.onnx";
/// How many pixels the model gets of a frame (640 x 360 at 16:9): 11 ms a mask on four threads and 27 ms on one, where 720p costs three times that.
pub const INPUT_PIXELS: f64 = 640.0 * 360.0;
/// RVM's own advice: the picture it works through is about 320 pixels on its longer side (its downsample ratio).
const WORK_SIDE: f32 = 320.0;
/// More than a couple of threads buys little, and this runs next to the call's own video.
const THREADS: usize = 2;
const STATES: [&str; 4] = ["r1i", "r2i", "r3i", "r4i"];
const STATES_OUT: [&str; 4] = ["r1o", "r2o", "r3o", "r4o"];

static ENVIRONMENT: std::sync::Mutex<Option<Environment>> = std::sync::Mutex::new(None);

/// The library's file name in the download.
pub fn library_name() -> &'static str {
    if cfg!(windows) { "onnxruntime.dll" } else { "libonnxruntime.so" }
}

fn failed(kind: &str, e: impl std::fmt::Display) -> VisionError {
    VisionError::new(kind, &e.to_string())
}

/// The size a frame is shrunk to for the model: INPUT_PIXELS in the frame's own shape, so a phone held upright costs what a landscape one does.
pub fn input_size(w: usize, h: usize) -> (usize, usize) {
    let scale = (INPUT_PIXELS / (w * h).max(1) as f64).sqrt();
    (((w as f64 * scale).round() as usize).max(2), ((h as f64 * scale).round() as usize).max(2))
}

/// The frame as the model gets it; None when it's no bigger than that already.
pub fn model_picture(frame: &Rgb) -> Option<Rgb> {
    model_input(frame, input_size(frame.w, frame.h))
}

/// RVM's downsample ratio for a picture of w x h: its working copy is about WORK_SIDE on the longer side, never bigger than the picture.
pub fn downsample_ratio(w: usize, h: usize) -> f32 {
    (WORK_SIDE / w.max(h).max(1) as f32).min(1.0)
}

/// ONNX Runtime loaded from the download, and the model's file. Makes one session a stream.
pub struct Matting {
    env: Environment,
    model: std::path::PathBuf,
}

impl Matting {
    /// Loads the library and checks the model is there. A folder without them is Missing; a library that won't load (or is too old) Failed.
    pub fn load(folder: &Path) -> Result<Matting, LoadError> {
        let library = folder.join("lib").join(library_name());
        let model = folder.join("models").join(MODEL);
        if !library.is_file() || !model.is_file() {
            return Err(LoadError::Missing(format!("no matting files in {}", folder.display())));
        }
        // ONNX Runtime allows one environment per process and one library, so a second load in the same process (a test, a retry) uses the first
        let mut shared = ENVIRONMENT.lock().unwrap_or_else(|e| e.into_inner());
        let env = match shared.as_ref() {
            Some(env) => env.clone(),
            None => {
                let env = ort::init_from(&library)
                    .map_err(|e| LoadError::Failed(failed("OSError", e)))?
                    .with_name("telescope")
                    .with_telemetry(false)
                    .build()
                    .map_err(|e| LoadError::Failed(failed("RuntimeError", e)))?;
                *shared = Some(env.clone());
                env
            }
        };
        Ok(Matting { env, model })
    }
}

impl Matting {
    fn session(&self) -> ort::Result<Session> {
        let session = Session::builder(&self.env)?
            .with_optimization_level(GraphOptimizationLevel::Level3)?
            .with_intra_threads(THREADS)?
            .with_inter_threads(1)?
            // idle threads sleep instead of spinning: the mask is worked out about every sixth of a second
            .with_intra_op_spinning(false)?
            .commit_from_file(&self.model)?;
        Ok(session)
    }
}

impl Backend for Matting {
    fn version(&self) -> String {
        "Robust Video Matting on ONNX Runtime".into()
    }

    fn segmenter(&self) -> Result<Box<dyn Segmenter>, VisionError> {
        let session = self.session().map_err(|e| failed("RuntimeError", e))?;
        let has = |names: &[&str], have: Vec<&str>| names.iter().all(|n| have.contains(n));
        let (inputs, outputs): (Vec<&str>, Vec<&str>) = (session.inputs().iter().map(|i| i.name()).collect(), session.outputs().iter().map(|o| o.name()).collect());
        if !has(&["src", "downsample_ratio"], inputs.clone()) || !has(&STATES, inputs) || !has(&["pha"], outputs.clone()) || !has(&STATES_OUT, outputs) {
            return Err(VisionError::new("ValueError", "the model isn't Robust Video Matting"));
        }
        Ok(Box::new(Rvm { session, states: None, size: (0, 0) }))
    }

    fn pose(&self) -> Result<Box<dyn PoseModel>, VisionError> {
        Err(VisionError::new("NotImplementedError", "the high quality cutout has no pose model"))
    }
}

/// One stream's model: its session and the states the last frame left.
pub struct Rvm {
    session: Session,
    /// None at the start and after a reset: the model gets zeros then.
    states: Option<[DynValue; 4]>,
    size: (usize, usize),
}

impl Rvm {
    fn zeros() -> Result<[DynValue; 4], VisionError> {
        let one = || Tensor::from_array(([1usize, 1, 1, 1], vec![0f32])).map(|t| t.into_dyn()).map_err(|e| failed("RuntimeError", e));
        Ok([one()?, one()?, one()?, one()?])
    }
}

impl Segmenter for Rvm {
    fn segment(&mut self, frame: &Rgb, _ts_ms: i64) -> Result<Gray, VisionError> {
        let (w, h) = (frame.w, frame.h);
        // States of another size mean nothing to this one
        if self.size != (w, h) {
            self.reset();
            self.size = (w, h);
        }
        let n = w * h;
        let mut planes = vec![0f32; n * 3];
        let (r, rest) = planes.split_at_mut(n);
        let (g, b) = rest.split_at_mut(n);
        for (i, p) in frame.data.as_chunks::<3>().0.iter().enumerate() {
            r[i] = p[0] as f32 * (1.0 / 255.0);
            g[i] = p[1] as f32 * (1.0 / 255.0);
            b[i] = p[2] as f32 * (1.0 / 255.0);
        }
        let states = match self.states.take() {
            Some(states) => states,
            None => Rvm::zeros()?,
        };
        let src = Tensor::from_array(([1usize, 3, h, w], planes)).map_err(|e| failed("RuntimeError", e))?;
        let ratio = Tensor::from_array(([1usize], vec![downsample_ratio(w, h)])).map_err(|e| failed("RuntimeError", e))?;
        let mut inputs: Vec<(Cow<str>, SessionInputValue)> = vec![("src".into(), src.into()), ("downsample_ratio".into(), ratio.into())];
        for (name, state) in STATES.iter().zip(states) {
            inputs.push(((*name).into(), state.into()));
        }
        // A failed run leaves no states behind, so the next frame starts again
        let mut out = self.session.run(inputs).map_err(|e| failed("RuntimeError", e))?;
        let mask = {
            let (shape, pha) = out["pha"].try_extract_tensor::<f32>().map_err(|e| failed("RuntimeError", e))?;
            if shape.len() != 4 || shape[2] as usize != h || shape[3] as usize != w || pha.len() != n {
                return Err(VisionError::new("ValueError", "the model's mask isn't the size of its picture"));
            }
            Gray { w, h, data: pha.iter().map(|v| (v * 255.0).round().clamp(0.0, 255.0) as u8).collect() }
        };
        let mut next = Vec::with_capacity(4);
        for name in STATES_OUT {
            match out.remove(name) {
                Some(state) => next.push(state),
                None => return Err(VisionError::new("ValueError", "the model gave no state")),
            }
        }
        self.states = next.try_into().ok();
        Ok(mask)
    }

    fn input(&self, frame: &Rgb) -> Option<Rgb> {
        model_picture(frame)
    }

    fn blends_last(&self) -> bool {
        false
    }

    fn reset(&mut self) {
        self.states = None;
    }
}

/// The model and the library as an Arc, as Vision keeps its backends.
pub fn load(folder: &Path) -> Result<Arc<dyn Backend>, LoadError> {
    Matting::load(folder).map(|m| Arc::new(m) as Arc<dyn Backend>)
}

#[cfg(test)]
#[path = "matting_tests.rs"]
mod tests;
