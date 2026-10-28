//! The window and the stream flow. Everything it uses from other crates comes in through core's traits (Services).

pub mod banners;
pub mod collapse;
pub mod compile;
pub mod doc;
pub mod flow;
pub mod layout;
pub mod offscreen;
pub mod plugins;
pub mod scenes;
pub mod profiles;
pub mod snapshot;
pub mod source;
pub mod settings;
pub mod switches;
pub mod texts;
pub mod shell;
pub mod host;
pub mod window;

pub use window::{run, WindowOptions};
