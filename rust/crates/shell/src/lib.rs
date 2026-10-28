//! The window and the stream flow. Everything it uses from other crates comes in through core's traits (Services).

pub mod flow;
pub mod snapshot;
pub mod window;

pub use window::{run, WindowOptions};
