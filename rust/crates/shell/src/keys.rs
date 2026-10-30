//! Keys in Telescope's window go to the plugins before the focused control (the Shortcuts plugin's key filter), and the
//! window's focus says when held keys won't come back up (on_keys_lost) and whether Telescope is in front.

use crate::window::Window;
use slint::ComponentHandle;
use slint_interpreter::Value as SValue;
use std::rc::Rc;
use telescope_core::ui::KeyInput;

/// The window's KeyEvent as core's KeyInput.
pub fn key_input(event: &SValue, released: bool, typing: bool, modal: bool) -> KeyInput {
    let SValue::Struct(e) = event else { return KeyInput::default() };
    let flag = |s: &slint_interpreter::Struct, name: &str| s.get_field(name) == Some(&SValue::Bool(true));
    let text = match e.get_field("text") { Some(SValue::String(t)) => t.to_string(), _ => String::new() };
    let (ctrl, alt, shift, meta) = match e.get_field("modifiers") {
        Some(SValue::Struct(m)) => (flag(m, "control"), flag(m, "alt"), flag(m, "shift"), flag(m, "meta")),
        _ => (false, false, false, false),
    };
    KeyInput { text, ctrl, alt, shift, meta, repeat: flag(e, "repeat"), released, typing, modal, scan: 0 }
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
    pub(crate) fn watch_focus(self: &Rc<Self>) {
        let active = |w: &slint::Window| i_slint_core::window::WindowInner::from_pub(w).active();
        let now = active(self.ui.window()) || crate::settings::window().is_some_and(|s| active(s.window()));
        if std::mem::replace(&mut self.state.borrow_mut().window_active, now) && !now {
            let count = self.state.borrow().slots.len();
            for i in 0..count {
                self.state.borrow_mut().call(i, |p, host| p.on_keys_lost(host));
            }
            self.process();
        }
    }
}
