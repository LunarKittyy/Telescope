//! The window and the stream flow. Everything it uses from other crates comes in through core's traits (Services).

pub mod banners;
pub mod collapse;
pub mod compile;
pub mod doc;
pub mod flow;
pub mod layout;
pub mod snapshot;
pub mod source;
pub mod window;

pub use window::{run, WindowOptions};
