//! Reaching phones: pairing, finding them on the LAN or a USB cable, their session and video. Nothing here knows about the window.

pub mod browser;
pub mod adb;
pub mod adb_download;
pub mod audio;
pub mod connection;
pub mod controls;
pub mod discovery;
pub mod feeds;
pub mod https;
pub mod net;
pub mod pairing;
pub mod phone_client;
pub mod phones;
pub mod platform;
pub mod plugin;
pub mod sensors;
pub mod service;
pub mod session_client;
pub mod shared;
pub mod stills;
pub mod udp;
pub mod util;
pub mod web;
pub mod winjob;

pub use service::LinkService;
