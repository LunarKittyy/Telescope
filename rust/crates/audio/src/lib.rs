//! The phone's microphone into a virtual microphone (audio.py, denoise.py, platform/virtual_mic.py and the Microphone
//! plugin). The plugin (plugin.rs) is what the window uses; VirtualMic is core's Audio for anything else that has a
//! phone's audio feed.

pub mod backend;
pub mod denoise;
pub mod jitter;
pub mod meter;
pub mod plugin;
pub mod shape;
pub mod sink;
pub mod vb_cable;
pub mod virtual_mic;
pub mod worker;

use std::sync::{Arc, Mutex};
use telescope_core::audio::{Audio, AudioFeed, MicSettings, MicStream};

/// The virtual microphone on this computer.
pub struct VirtualMic {
    backend: Arc<dyn backend::MicBackend>,
}

impl Default for VirtualMic {
    fn default() -> Self {
        VirtualMic { backend: backend::default_backend() }
    }
}

impl Audio for VirtualMic {
    fn virtual_mic_ready(&self) -> Result<(), String> {
        match self.backend.problem() {
            Some((text, _)) => Err(text),
            None => Ok(()),
        }
    }

    /// Sets the virtual mic up (blocking) and plays feed into it; the feed isn't reopened once it ends.
    fn start(&self, feed: Box<dyn AudioFeed>, settings: &MicSettings) -> Result<Box<dyn MicStream>, String> {
        self.virtual_mic_ready()?;
        let err = self.backend.prepare();
        if !err.is_empty() {
            return Err(err);
        }
        let once = Arc::new(Mutex::new(Some(feed)));
        let open: worker::Opener = Arc::new(move || {
            once.lock().unwrap_or_else(|e| e.into_inner()).take().ok_or_else(|| telescope_core::i18n::tr("The phone's microphone stopped."))
        });
        let mut w = worker::AudioWorker::new("", open, self.backend.open_sink(), Arc::new(|kind, text| {
            if kind == "err" {
                log::warn!("Microphone: {text}");
            }
        }));
        w.set_settings(settings);
        w.start();
        Ok(Box::new(w))
    }
}
