//! Spout send on Linux: a PipeWire video source for each shared stream.

use super::{Backend, Sender};
use crate::pipewire::{Connection, VideoSender};
use std::sync::{Arc, Mutex};

pub fn backend() -> Option<Arc<dyn Backend>> {
    Some(Arc::new(PipeWireBackend { connection: Mutex::new(None) }))
}

struct PipeWireBackend {
    connection: Mutex<Option<Arc<Connection>>>,
}

impl PipeWireBackend {
    /// The connection, made again if PipeWire was restarted since.
    fn connection(&self) -> Result<Arc<Connection>, String> {
        let mut held = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(connection) = held.clone().filter(|c| !c.is_dead()) {
            return Ok(connection);
        }
        let connection = Connection::shared()?;
        *held = Some(connection.clone());
        Ok(connection)
    }
}

impl Backend for PipeWireBackend {
    fn open(&self, name: &str, width: u32, height: u32) -> Result<Box<dyn Sender>, String> {
        let connection = self.connection()?;
        Ok(Box::new(VideoSender::open(&connection, name, width, height)?))
    }
}

impl Sender for VideoSender {
    fn send(&mut self, bgra: &[u8]) -> Result<(), String> {
        VideoSender::send(self, bgra)
    }
}
