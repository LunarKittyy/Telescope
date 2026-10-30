//! The window and the stream flow. Everything it uses from other crates comes in through core's traits (Services).

pub mod admin;
pub mod banners;
pub mod collapse;
pub mod compile;
pub mod doc;
mod extra_cameras;
pub mod file_dialog;
pub mod flow;
pub mod layout;
pub mod offscreen;
pub mod overlays;
pub mod preview_passes;
pub mod plugins;
pub mod preview;
pub mod plugins_tab;
pub mod scenes;
pub mod search;
pub mod profiles;
pub mod snapshot;
pub mod source;
pub mod sources;
pub mod stream_flow;
pub mod streams;
pub mod session;
pub mod settings;
pub mod switches;
pub mod texts;
pub mod tray;
pub mod shell;
pub mod host;
pub mod keys;
pub mod view;
pub mod view_control;
pub mod window;
pub mod worker;

pub use window::{run, ShownHook, WindowOptions};
