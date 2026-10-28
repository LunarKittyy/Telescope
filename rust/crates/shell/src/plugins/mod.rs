//! The built-in plugins that live with the window for now. Each is a Plugin like any other: its card is a .slint
//! component plus properties, which is exactly what an added plugin hands the shell too.

pub mod camera_control;

use telescope_core::plugin::Plugin;

/// The built-in plugins this crate has, in registration order.
pub fn builtins() -> Vec<Box<dyn Plugin>> {
    vec![Box::new(camera_control::CameraControl::default())]
}
