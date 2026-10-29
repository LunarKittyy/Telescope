//! Reaching phones: pairing, finding them on the LAN or a USB cable, their session and video. Nothing here knows about the window.

pub mod adb;
pub mod adb_download;
pub mod connection;
pub mod discovery;
pub mod feeds;
pub mod https;
pub mod net;
pub mod pairing;
pub mod phone_client;
pub mod phones;
pub mod plugin;
pub mod sensors;
pub mod service;
pub mod session_client;
pub mod shared;
pub mod udp;
pub mod util;
pub mod winjob;

pub use service::LinkService;
