//! Talking to PipeWire for whatever needs it (reading video nodes now, producing them or capturing screens later), loading libpipewire at run time so a system without it still starts.

mod connection;
mod ffi;
pub mod pod;
mod stream;
mod video;

#[cfg(test)]
pub(crate) mod test_source;

pub use connection::{Connection, Node};
pub use ffi::{PwBuffer, StreamState};
pub use stream::{Stream, StreamConfig, StreamHandler, StreamRef, PW_DIRECTION_INPUT, PW_DIRECTION_OUTPUT, PW_STREAM_FLAG_ALLOC_BUFFERS, PW_STREAM_FLAG_AUTOCONNECT, PW_STREAM_FLAG_DRIVER, SPA_ID_INVALID};
pub use video::{VideoConsumer, VideoFormat, VideoFrame};
