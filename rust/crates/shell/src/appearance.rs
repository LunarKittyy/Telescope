//! Appearance: the options that scale or swap a design token (corners, text size, density, contrast, motion, tooltip timing). One value
//! is kept for the whole app, and every window of the app, as it is built or when the person changes it, gets it written to its own
//! Theme, Tips and Motion globals. The look and the card colours have their own calls (Effect::Look, Effect::PluginHues).

use crate::motion::{self, Motion};
use slint_interpreter::{ComponentInstance, Value};
use std::sync::Mutex;
use telescope_core::host::{Appearance, MotionChoice};

static CURRENT: Mutex<Option<Appearance>> = Mutex::new(None);

/// What the person picked (the defaults until they have).
pub fn current() -> Appearance {
    CURRENT.lock().ok().and_then(|a| *a).unwrap_or_default()
}

/// Remembers a new pick, for the windows made from now on.
pub fn store(appearance: Appearance) {
    if let Ok(mut current) = CURRENT.lock() {
        *current = Some(appearance);
    }
}

/// The motion that follows from a pick: the system's (or TELESCOPE_MOTION's) unless the person chose one.
pub fn motion_of(appearance: &Appearance) -> Motion {
    match appearance.motion {
        MotionChoice::System => motion::current(),
        MotionChoice::Full => Motion::Full,
        MotionChoice::Reduced => Motion::Reduced,
        MotionChoice::Off => Motion::Off,
    }
}

/// Writes what is picked into a window's globals.
pub fn apply_to(ui: &ComponentInstance, appearance: &Appearance) {
    let number = |value: f64| Value::Number(value);
    let _ = ui.set_global_property("Theme", "radius", number(appearance.radius()));
    let _ = ui.set_global_property("Theme", "body", number(appearance.body()));
    let _ = ui.set_global_property("Theme", "unit", number(appearance.unit()));
    let _ = ui.set_global_property("Theme", "contrast", Value::Bool(appearance.contrast));
    let _ = ui.set_global_property("Tips", "delay", number(f64::from(appearance.tip_delay_ms)));
    let _ = ui.set_global_property("Tips", "warm", Value::Bool(appearance.tip_warm));
    motion::set(ui, motion_of(appearance));
}

/// A window just built starts as the app is.
pub fn apply(ui: &ComponentInstance) {
    apply_to(ui, &current());
}
