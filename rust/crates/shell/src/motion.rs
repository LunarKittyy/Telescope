//! Whether the windows may move things: the desktop's own reduced-motion setting, which TELESCOPE_MOTION (full, reduced or off) overrides.
//! The kit's motions (ui-kit/ui/widgets/motion.slint) read it from the `Motion` global: reduced snaps every move and keeps colour and
//! opacity changes, off keeps nothing. Appearance can set the same global later.

use slint_interpreter::{ComponentInstance, Value};
use std::sync::OnceLock;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Motion {
    Full,
    Reduced,
    Off,
}

fn word(text: &str) -> Option<Motion> {
    match text.trim().to_lowercase().as_str() {
        "full" => Some(Motion::Full),
        "reduced" => Some(Motion::Reduced),
        "off" => Some(Motion::Off),
        _ => None,
    }
}

/// What `gsettings get org.gnome.desktop.interface enable-animations` printed: false is a person who turned animations off.
pub fn gnome_says(output: &str) -> Option<Motion> {
    match output.trim() {
        "false" => Some(Motion::Reduced),
        "true" => Some(Motion::Full),
        _ => None,
    }
}

/// What ~/.config/kdeglobals says: its animation speed at 0 is every animation off.
pub fn kde_says(kdeglobals: &str) -> Option<Motion> {
    let mut in_kde = false;
    for line in kdeglobals.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_kde = line == "[KDE]";
        } else if in_kde {
            if let Some(factor) = line.strip_prefix("AnimationDurationFactor=") {
                return factor.trim().parse::<f64>().ok().map(|f| if f <= 0.0 { Motion::Reduced } else { Motion::Full });
            }
        }
    }
    None
}

#[cfg(target_os = "linux")]
fn system() -> Motion {
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default().to_lowercase();
    if desktop.contains("kde") {
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
        let config = std::env::var_os("XDG_CONFIG_HOME").map(std::path::PathBuf::from).or_else(|| home.map(|h| h.join(".config")));
        return config.and_then(|c| std::fs::read_to_string(c.join("kdeglobals")).ok()).and_then(|t| kde_says(&t)).unwrap_or(Motion::Full);
    }
    if desktop.contains("gnome") || desktop.contains("unity") || desktop.contains("cinnamon") || desktop.contains("budgie") || desktop.contains("pantheon") {
        let output = std::process::Command::new("gsettings").args(["get", "org.gnome.desktop.interface", "enable-animations"]).output();
        return output.ok().and_then(|o| gnome_says(&String::from_utf8_lossy(&o.stdout))).unwrap_or(Motion::Full);
    }
    Motion::Full
}

/// What Windows' "Show animations in Windows" (SPI_GETCLIENTAREAANIMATION) says: off is reduced motion.
pub fn windows_says(animations_on: bool) -> Motion {
    if animations_on { Motion::Full } else { Motion::Reduced }
}

#[cfg(windows)]
fn system() -> Motion {
    use windows::Win32::UI::WindowsAndMessaging::{SystemParametersInfoW, SPI_GETCLIENTAREAANIMATION, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS};
    // A BOOL: 0 when animations are off
    let mut on: i32 = 1;
    // SAFETY: the pointer is to a live i32, which is the size of the BOOL this action fills in, and uiparam is unused by it
    let read = unsafe { SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, Some(&mut on as *mut i32 as *mut core::ffi::c_void), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0)) };
    if read.is_ok() { windows_says(on != 0) } else { Motion::Full }
}

#[cfg(not(any(target_os = "linux", windows)))]
fn system() -> Motion {
    Motion::Full
}

/// What the person's setting says, looked up once.
pub fn current() -> Motion {
    static MODE: OnceLock<Motion> = OnceLock::new();
    *MODE.get_or_init(|| std::env::var("TELESCOPE_MOTION").ok().and_then(|w| word(&w)).unwrap_or_else(system))
}

/// Tells a window's kit what is allowed to move.
pub fn apply(ui: &ComponentInstance) {
    set(ui, current());
}

/// The same for a given mode, which a test or the Appearance option can ask for.
pub fn set(ui: &ComponentInstance, mode: Motion) {
    let _ = ui.set_global_property("Motion", "reduced", Value::Bool(mode != Motion::Full));
    let _ = ui.set_global_property("Motion", "off", Value::Bool(mode == Motion::Off));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_words_for_a_mode_are_read_as_they_are_written() {
        assert_eq!(word("Reduced\n"), Some(Motion::Reduced));
        assert_eq!(word("off"), Some(Motion::Off));
        assert_eq!(word("FULL"), Some(Motion::Full));
        assert_eq!(word("slow"), None);
    }

    #[test]
    fn gnome_animations_off_is_reduced_motion() {
        assert_eq!(gnome_says("false\n"), Some(Motion::Reduced));
        assert_eq!(gnome_says("true\n"), Some(Motion::Full));
        assert_eq!(gnome_says(""), None);
    }

    #[test]
    fn windows_animations_off_is_reduced_motion() {
        assert_eq!(windows_says(false), Motion::Reduced);
        assert_eq!(windows_says(true), Motion::Full);
    }

    #[test]
    fn kdes_animation_speed_at_zero_is_reduced_motion() {
        assert_eq!(kde_says("[General]\nAnimationDurationFactor=0\n[KDE]\nAnimationDurationFactor=0\n"), Some(Motion::Reduced));
        assert_eq!(kde_says("[KDE]\nAnimationDurationFactor=0.5\n"), Some(Motion::Full));
        assert_eq!(kde_says("[KDE]\nwidgetStyle=Breeze\n"), None);
        // Only the [KDE] group counts
        assert_eq!(kde_says("[Other]\nAnimationDurationFactor=0\n"), None);
    }
}
