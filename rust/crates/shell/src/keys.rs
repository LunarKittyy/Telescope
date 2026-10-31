//! Keys in Telescope's window go to the plugins before the focused control (the Shortcuts plugin's key filter), and the
//! window's focus says when held keys won't come back up (on_keys_lost) and whether Telescope is in front.

use crate::window::Window;
use slint::ComponentHandle;
use slint_interpreter::Value as SValue;
use std::rc::Rc;
use telescope_core::ui::KeyInput;

thread_local! {
    /// The scan code of the key winit last reported, which Slint's KeyEvent leaves out.
    static LAST_SCAN: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// The scan code the system gives a physical key, 0 when it gives none.
fn scan_code(key: slint::winit_030::winit::keyboard::PhysicalKey) -> u32 {
    use slint::winit_030::winit::platform::scancode::PhysicalKeyExtScancode;
    key.to_scancode().unwrap_or(0)
}

/// Makes the scan code of each key event available to key_input: winit's event comes first, then Slint's own KeyEvent for it.
pub fn watch_scan_codes(window: &slint::Window) {
    use slint::winit_030::{winit::event::WindowEvent, EventResult, WinitWindowAccessor};
    window.on_winit_window_event(|_, event| {
        if let WindowEvent::KeyboardInput { event, .. } = event {
            LAST_SCAN.with(|s| s.set(scan_code(event.physical_key)));
        }
        EventResult::Propagate
    });
}

/// The key Slint's KeyEvent names for a winit key that has no text of its own.
fn named_key(key: slint::winit_030::winit::keyboard::NamedKey) -> Option<slint::platform::Key> {
    use slint::platform::Key as K;
    use slint::winit_030::winit::keyboard::NamedKey as N;
    Some(match key {
        N::Backspace => K::Backspace, N::Tab => K::Tab, N::Enter => K::Return, N::Escape => K::Escape, N::Delete => K::Delete,
        N::Shift => K::Shift, N::Control => K::Control, N::Alt => K::Alt, N::AltGraph => K::AltGr, N::CapsLock => K::CapsLock,
        N::Meta | N::Super => K::Meta, N::ArrowUp => K::UpArrow, N::ArrowDown => K::DownArrow, N::ArrowLeft => K::LeftArrow,
        N::ArrowRight => K::RightArrow, N::Home => K::Home, N::End => K::End, N::PageUp => K::PageUp, N::PageDown => K::PageDown,
        N::Insert => K::Insert, N::F1 => K::F1, N::F2 => K::F2, N::F3 => K::F3, N::F4 => K::F4, N::F5 => K::F5, N::F6 => K::F6,
        N::F7 => K::F7, N::F8 => K::F8, N::F9 => K::F9, N::F10 => K::F10, N::F11 => K::F11, N::F12 => K::F12,
        _ => return None,
    })
}

/// What Slint's KeyEvent would say for a winit key, so a plugin hears the same from any window.
fn key_text(key: &slint::winit_030::winit::keyboard::Key, typed: Option<&str>) -> String {
    use slint::winit_030::winit::keyboard::{Key, NamedKey};
    match key {
        Key::Named(NamedKey::Space) => " ".to_string(),
        Key::Named(named) => named_key(*named).map_or_else(|| typed.unwrap_or("").to_string(), |k| slint::SharedString::from(k).to_string()),
        Key::Character(text) => text.to_string(),
        _ => typed.unwrap_or("").to_string(),
    }
}

/// A plugin's own window: its component is the plugin's, so nothing in it can catch keys first as the main window's
/// FocusScope does, and they're taken here, before Slint gives them to the control with focus.
pub fn watch_plugin_keys(window: &slint::Window, owner: std::rc::Weak<Window>) {
    use slint::winit_030::winit::event::{ElementState, WindowEvent};
    use slint::winit_030::winit::keyboard::ModifiersState;
    use slint::winit_030::{EventResult, WinitWindowAccessor};
    let held = std::cell::Cell::new(ModifiersState::empty());
    window.on_winit_window_event(move |window, event| {
        match event {
            WindowEvent::ModifiersChanged(m) => held.set(m.state()),
            WindowEvent::KeyboardInput { event, .. } => {
                let inner = i_slint_core::window::WindowInner::from_pub(window);
                let mods = held.get();
                let input = KeyInput {
                    text: key_text(&event.logical_key, event.text.as_deref()), ctrl: mods.control_key(), alt: mods.alt_key(), shift: mods.shift_key(),
                    meta: mods.super_key(), repeat: event.repeat, released: event.state == ElementState::Released,
                    typing: inner.text_input_focused(), modal: !inner.active_popups().is_empty(), scan: scan_code(event.physical_key),
                };
                if owner.upgrade().is_some_and(|w| w.key(&input)) {
                    return EventResult::PreventDefault;
                }
            }
            _ => {}
        }
        EventResult::Propagate
    });
}

/// The window's KeyEvent as core's KeyInput.
pub fn key_input(event: &SValue, released: bool, typing: bool, modal: bool) -> KeyInput {
    let SValue::Struct(e) = event else { return KeyInput::default() };
    let flag = |s: &slint_interpreter::Struct, name: &str| s.get_field(name) == Some(&SValue::Bool(true));
    let text = match e.get_field("text") { Some(SValue::String(t)) => t.to_string(), _ => String::new() };
    let (ctrl, alt, shift, meta) = match e.get_field("modifiers") {
        Some(SValue::Struct(m)) => (flag(m, "control"), flag(m, "alt"), flag(m, "shift"), flag(m, "meta")),
        _ => (false, false, false, false),
    };
    KeyInput { text, ctrl, alt, shift, meta, repeat: flag(e, "repeat"), released, typing, modal, scan: LAST_SCAN.with(|s| s.get()) }
}

impl Window {
    /// A key in the window: each plugin's on_key in turn until one takes it.
    pub(crate) fn key(self: &Rc<Self>, input: &KeyInput) -> bool {
        let count = self.state.borrow().slots.len();
        let mut taken = false;
        for i in 0..count {
            if self.state.borrow_mut().call(i, |p, host| p.on_key(host, input)) == Some(true) {
                taken = true;
                break;
            }
        }
        self.process();
        taken
    }

    /// A popup or menu is open over the window.
    pub(crate) fn modal(&self) -> bool {
        !i_slint_core::window::WindowInner::from_pub(self.ui.window()).active_popups().is_empty()
    }

    /// Whether one of Telescope's windows is in front, checked on the preview's timer: when that changes the plugins'
    /// host.window_active() follows, and a window losing focus tells them held keys won't come back up.
    pub fn watch_focus(self: &Rc<Self>) {
        if self.report_shown() {
            self.process(); // minimized or back: the window manager says so to nobody else
        }
        let active = |w: &slint::Window| i_slint_core::window::WindowInner::from_pub(w).active();
        let now = active(self.ui.window()) || crate::settings::window().is_some_and(|s| active(s.window()))
            || self.plugin_windows.borrow().iter().any(|(_, _, ui)| active(ui.window()));
        if std::mem::replace(&mut self.state.borrow_mut().window_active, now) && !now {
            let count = self.state.borrow().slots.len();
            for i in 0..count {
                self.state.borrow_mut().call(i, |p, host| p.on_keys_lost(host));
            }
            self.process();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use slint::winit_030::winit::keyboard::{Key, NamedKey, SmolStr};

    #[test]
    fn a_plugin_window_names_keys_as_the_main_window_does() {
        assert_eq!(key_text(&Key::Character(SmolStr::new("a")), Some("a")), "a");
        assert_eq!(key_text(&Key::Named(NamedKey::Space), Some(" ")), " ");
        assert_eq!(key_text(&Key::Named(NamedKey::Escape), None), slint::SharedString::from(slint::platform::Key::Escape).as_str());
        assert_eq!(key_text(&Key::Named(NamedKey::ArrowLeft), None), slint::SharedString::from(slint::platform::Key::LeftArrow).as_str());
        assert_eq!(key_text(&Key::Named(NamedKey::F5), None), slint::SharedString::from(slint::platform::Key::F5).as_str());
        assert_eq!(key_text(&Key::Named(NamedKey::Enter), Some("\r")), "\n", "Return is a newline to Slint");
    }
}
