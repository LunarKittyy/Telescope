//! The built-in plugins that live with the window for now. Each is a Plugin like any other: its card is a .slint
//! component plus properties, which is exactly what an added plugin hands the shell too.

pub mod camera_control;
pub mod onboarding;

use std::sync::Arc;
use telescope_core::plugin::Plugin;
use telescope_core::services::Services;
use telescope_core::BusSender;

/// The built-in plugins this crate has, in the Python app's registration order. services is None for scene renders,
/// where nothing looks at this computer.
pub fn builtins(services: Option<&Services>, bus: &BusSender) -> Vec<Box<dyn Plugin>> {
    let vcam = services.map(|s| {
        let outputs = s.outputs.clone();
        Arc::new(move || !outputs.slots().is_empty()) as onboarding::VcamCheck
    });
    vec![
        Box::new(camera_control::CameraControl::default()),
        Box::new(onboarding::Onboarding::new(vcam, Some(bus.clone()))),
    ]
}
