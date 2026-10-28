//! Everything but the window: pairing, finding and talking to the phone, decoding its video and the virtual camera.
//! Like the Python app's Qt-free modules, nothing here knows about the GUI.

pub mod adb;
pub mod config;
pub mod decode;
pub mod discovery;
pub mod https;
pub mod net;
pub mod pairing;
pub mod session;
pub mod stream;
pub mod util;
pub mod v4l2;
