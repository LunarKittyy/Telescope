//! Reaching phones: pairing, finding them on the LAN or a USB cable, their session and video. Nothing here knows about the window.

pub mod adb;
pub mod discovery;
pub mod https;
pub mod net;
pub mod pairing;
pub mod service;
pub mod session;
pub mod util;

pub use service::LinkService;
