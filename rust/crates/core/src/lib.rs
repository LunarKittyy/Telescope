//! What every crate agrees on. Crates only meet through what's here; see rust/ARCHITECTURE.md for why it's shaped this way.

pub mod actions;
pub mod audio;
pub mod bus;
pub mod colour;
pub mod config;
pub mod event;
pub mod frame;
pub mod host;
pub mod i18n;
pub mod json;
pub mod lens;
pub mod link;
pub mod media;
pub mod output;
pub mod plugin;
pub mod qr;
pub mod services;
pub mod shader;
pub mod snapshot;
pub mod source;
pub mod system;
pub mod text;
pub mod testing;
pub mod time;
pub mod ui;
pub mod view;
pub mod view_step;

pub use bus::{BusSender, EventBus};
pub use event::Event;
pub use frame::{Frame, FrameStage, FrameStep, PixelFormat};
pub use host::Host;
pub use plugin::{Plugin, PluginInfo};
pub use serde_json::{json, Value};
