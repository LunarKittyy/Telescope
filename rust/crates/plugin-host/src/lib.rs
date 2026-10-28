//! Plugins people add: Lua scripts with GPU shaders and .slint cards, behind core's Plugin trait. See rust/docs/writing-plugins.md.
//!
//! The boundary is runtime-neutral (guest.rs): a runtime implements Guest, sees plain JSON both ways and reaches the host only through
//! PluginContext::call. Lua (lua.rs) is the one runtime today; rust/docs/plugin-wasm.md shows a WASM one fitting the same boundary.

pub mod guest;
pub mod i18n;
pub mod manifest;
pub mod package;
pub mod switches;
pub mod text;

pub use guest::{call_host, Guest, GuestPlugin, HostCalls, PluginContext, HOST_API};
pub use manifest::{OLDEST_PLUGIN_API, PLUGIN_API};
pub use package::{Installed, PluginFolder};
