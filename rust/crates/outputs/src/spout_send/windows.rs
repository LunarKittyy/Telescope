//! Spout send on Windows: a Spout sender for each shared stream, which programs like OBS list and read as a shared texture.

use super::{Backend, Sender};
use crate::spout_shared::win::sender::TextureSender;
use std::sync::Arc;

pub fn backend() -> Option<Arc<dyn Backend>> {
    Some(Arc::new(Spout))
}

struct Spout;

impl Backend for Spout {
    fn open(&self, name: &str, width: u32, height: u32) -> Result<Box<dyn Sender>, String> {
        Ok(Box::new(SpoutSender(TextureSender::new(name, width, height)?)))
    }
}

struct SpoutSender(TextureSender);

impl Sender for SpoutSender {
    fn send(&mut self, bgra: &[u8]) -> Result<(), String> {
        self.0.send(bgra)
    }

    fn resize(&mut self, width: u32, height: u32) -> Result<bool, String> {
        self.0.resize(width, height).map(|()| true)
    }
}

#[cfg(test)]
mod tests;
