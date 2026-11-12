//! The built-in plugins that live with the window for now. Each is a Plugin like any other: its card is a .slint
//! component plus properties, which is exactly what an added plugin hands the shell too.

pub mod camera_control;
pub mod onboarding;
pub mod presets;
pub mod preview;

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
    let installers = services.map(|s| {
        let (outputs, link, usb, apk) = (s.outputs.clone(), s.link.clone(), s.link.clone(), s.link.clone());
        onboarding::Installers {
            camera: Arc::new(move || {
                let done = outputs.set_up(telescope_core::output::SetupAction::Install { devices: 1, reset: false }, &|_| {});
                (done.ok, done.message)
            }),
            usb_ready: Arc::new(move || usb.bundled_apk().is_some() && usb.adb_available()),
            phone_app: Arc::new(move || {
                let phones = link.usb_devices();
                let Some(phone) = phones.iter().find(|d| d.state == "device") else {
                    return Err(telescope_core::i18n::tr("No phone found over USB. Plug it in and allow USB debugging when the phone asks."));
                };
                link.install_app(&phone.serial, &apk.bundled_apk().ok_or("The phone app isn't here")?)
            }),
        }
    });
    vec![
        Box::new(camera_control::CameraControl::default()),
        Box::new(onboarding::Onboarding::new(vcam, Some(bus.clone())).with_installers(installers)),
        Box::new(preview::Preview::default()),
        Box::new(presets::PresetsPlugin::new()),
    ]
}

/// Every built-in plugin's name in the Python app's registration order (plugins/__init__.py), which is the order
/// their cards and menu entries come in. Built-ins from other crates (Connection from link) arrive through
/// Services.plugins and are put in their place by this.
pub const ORDER: &[&str] = &[
    "setup", "connection", "camera_control", "browser_camera", "webcam", "cinema", "face_exposure", "background",
    "chroma_key", "fun_pack", "privacy_blur", "photos", "colour_grading", "filters", "stream_output", "transforms", "framing",
    "desk", "name_tag", "notes", "composite", "microphone", "presets", "qr_reader", "preview", "onboarding",
    "monitoring", "stats", "updates", "wait_screen", "on_air", "shortcuts", "startup", "mcp",
];

/// Built-ins in their registration order, then added plugins as they came.
pub fn in_order(mut plugins: Vec<Box<dyn Plugin>>) -> Vec<Box<dyn Plugin>> {
    plugins.sort_by_key(|p| ORDER.iter().position(|n| *n == p.info().name).unwrap_or(ORDER.len()));
    plugins
}
