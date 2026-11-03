//! What a window's input says that Slint doesn't act on by itself. Slint's winit backend ignores the pointer leaving while
//! a button is down, so a release that never arrives leaves a drag stuck to the cursor until the next click; the focus
//! rings show only after keys; and a click outside a text box has to take the focus from it. Every window goes through watch().

use i_slint_core::window::WindowInner;
use slint::winit_030::winit::event::{ElementState, WindowEvent};
use slint::winit_030::winit::keyboard::{Key, NamedKey};
use slint::winit_030::{EventResult, WinitWindowAccessor};
use slint::{ComponentHandle, LogicalPosition};
use slint_interpreter::{ComponentInstance, Value};
use std::cell::Cell;

/// What the pointer did, for telling whether a press has lost its release.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pointer {
    Pressed,
    Released,
    /// It moved, and the system says whether a button is down where it can (None on Wayland).
    Moved { button_down: Option<bool> },
    /// It left the window. Wayland sends this during a press only when the press has been taken away, elsewhere it's a drag going past the edge.
    Left { taken: bool },
    /// The window lost the keyboard while the press was on (Alt+Tab, the compositor took over).
    Unfocused,
}

/// Follows the left button. seen is true for the event that shows a press won't get its release.
#[derive(Default)]
pub struct Press {
    held: Cell<bool>,
}

impl Press {
    pub fn seen(&self, event: Pointer) -> bool {
        let lost = self.held.get() && match event {
            Pointer::Moved { button_down } => button_down == Some(false),
            Pointer::Left { taken } => taken,
            Pointer::Unfocused => true,
            Pointer::Pressed | Pointer::Released => false,
        };
        match event {
            Pointer::Pressed => self.held.set(true),
            Pointer::Released => self.held.set(false),
            _ if lost => self.held.set(false),
            _ => {}
        }
        lost
    }

    pub fn held(&self) -> bool {
        self.held.get()
    }
}

/// Ends whatever the press was holding: every TouchArea under it gets a cancel and lets the mouse go.
pub fn end_press(window: &slint::Window) {
    window.dispatch_event(slint::platform::WindowEvent::PointerExited);
}

/// A key that moves the focus: from here on the rings show.
pub fn focus_key(ui: &ComponentInstance) {
    let _ = ui.set_global_property("KeyFocus", "keyboard", Value::Bool(true));
}

/// A press at this place in the window: the rings go, and a text box the press is outside of lets go of the focus, so what
/// was typed gets applied. Before the press reaches the window, which may hand the focus on to what was clicked.
pub fn pressed_at(ui: &ComponentInstance, at: LogicalPosition) {
    let _ = ui.set_global_property("KeyFocus", "keyboard", Value::Bool(false));
    if outside_text_box(ui.window(), at) {
        let _ = ui.invoke("focus-home", &[]);
    }
}

fn outside_text_box(window: &slint::Window, at: LogicalPosition) -> bool {
    let inner = WindowInner::from_pub(window);
    if !inner.text_input_focused() || !inner.active_popups().is_empty() {
        return false;
    }
    let Some(item) = inner.focus_item.borrow().upgrade() else { return false };
    let size = item.geometry().size;
    let corner = item.map_to_window(i_slint_core::lengths::LogicalPoint::default());
    let (x, y) = (at.x - corner.x, at.y - corner.y);
    !(x >= 0.0 && y >= 0.0 && x < size.width && y < size.height)
}

/// Where the control with the keyboard focus is in the window (x, y, width, height), for tests that follow Tab round.
pub fn focused_box(ui: &ComponentInstance) -> Option<(f32, f32, f32, f32)> {
    let item = WindowInner::from_pub(ui.window()).focus_item.borrow().upgrade()?;
    let (corner, size) = (item.map_to_window(i_slint_core::lengths::LogicalPoint::default()), item.geometry().size);
    Some((corner.x, corner.y, size.width, size.height))
}

/// Whether Wayland carries this window, where leaving during a press means the press is gone.
pub(crate) fn on_wayland(window: &slint::Window) -> bool {
    use slint::winit_030::winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    window.with_winit_window(|w| w.window_handle().is_ok_and(|h| matches!(h.as_raw(), RawWindowHandle::Wayland(_)))).unwrap_or(false)
}

/// Whether the system has a mouse button down, where it can be asked.
#[cfg(windows)]
fn button_down(_window: &slint::Window) -> Option<bool> {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON, VK_RBUTTON};
    // Both, since these are the physical buttons and the system can have them swapped
    let down = |key: i32| unsafe { GetAsyncKeyState(key) } < 0;
    Some(down(VK_LBUTTON.0 as i32) || down(VK_RBUTTON.0 as i32))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn button_down(window: &slint::Window) -> Option<bool> {
    use slint::winit_030::winit::raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
    use x11_dl::xlib;
    thread_local! {
        static XLIB: Option<xlib::Xlib> = xlib::Xlib::open().ok();
    }
    window.with_winit_window(|w| {
        let RawWindowHandle::Xlib(handle) = w.window_handle().ok()?.as_raw() else { return None };
        let RawDisplayHandle::Xlib(display) = w.display_handle().ok()?.as_raw() else { return None };
        let display = display.display?.as_ptr() as *mut xlib::Display;
        XLIB.with(|x| {
            let x = x.as_ref()?;
            let (mut root, mut child, mut root_x, mut root_y, mut win_x, mut win_y, mut mask) = (0, 0, 0, 0, 0, 0, 0);
            // The display and window are the ones winit is using on this thread
            let same_screen = unsafe { (x.XQueryPointer)(display, handle.window, &mut root, &mut child, &mut root_x, &mut root_y, &mut win_x, &mut win_y, &mut mask) };
            (same_screen != 0).then_some(mask & (xlib::Button1Mask | xlib::Button2Mask | xlib::Button3Mask) != 0)
        })
    }).flatten()
}

/// Watches a window's input, then gives each event to `then` (a window has one winit filter, so the others hang off this).
pub fn watch(ui: &ComponentInstance, mut then: impl FnMut(&slint::Window, &WindowEvent) -> EventResult + 'static) {
    set_room(ui, ui.window().size().to_logical(ui.window().scale_factor()));
    let weak = ui.as_weak();
    let press = Press::default();
    let cursor = Cell::new(LogicalPosition::default());
    ui.window().on_winit_window_event(move |window, event| {
        let pointer = match event {
            WindowEvent::MouseInput { state: ElementState::Pressed, .. } => {
                if let Some(ui) = weak.upgrade() {
                    pressed_at(&ui, cursor.get());
                }
                Some(Pointer::Pressed)
            }
            WindowEvent::MouseInput { state: ElementState::Released, .. } => Some(Pointer::Released),
            WindowEvent::CursorMoved { position, .. } => {
                let at = position.to_logical::<f32>(window.scale_factor() as f64);
                cursor.set(LogicalPosition::new(at.x, at.y));
                Some(Pointer::Moved { button_down: if press.held() { button_down(window) } else { None } })
            }
            WindowEvent::CursorLeft { .. } => {
                // Past a lost press Slint has its pressed flag up and skips this, so it's said here
                if !press.held() {
                    window.dispatch_event(slint::platform::WindowEvent::PointerExited);
                }
                Some(Pointer::Left { taken: on_wayland(window) })
            }
            WindowEvent::Focused(false) => Some(Pointer::Unfocused),
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                if matches!(&event.logical_key, Key::Named(NamedKey::Tab | NamedKey::ArrowUp | NamedKey::ArrowDown | NamedKey::ArrowLeft | NamedKey::ArrowRight)) {
                    if let Some(ui) = weak.upgrade() {
                        focus_key(&ui);
                    }
                }
                None
            }
            WindowEvent::Resized(size) => {
                if let Some(ui) = weak.upgrade() {
                    set_room(&ui, slint::PhysicalSize::new(size.width, size.height).to_logical(window.scale_factor()));
                }
                None
            }
            _ => None,
        };
        if pointer.is_some_and(|p| press.seen(p)) {
            end_press(window);
        }
        then(window, event)
    });
}

/// The window's size as the kit's dialogs and lists see it; a window that says so itself in its document keeps saying the same.
fn set_room(ui: &ComponentInstance, size: slint::LogicalSize) {
    let _ = ui.set_global_property("WindowBox", "width", Value::Number(size.width as f64));
    let _ = ui.set_global_property("WindowBox", "height", Value::Number(size.height as f64));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_press_ends_on_its_release() {
        let p = Press::default();
        assert!(!p.seen(Pointer::Pressed) && p.held());
        assert!(!p.seen(Pointer::Moved { button_down: None }));
        assert!(!p.seen(Pointer::Moved { button_down: Some(true) }));
        assert!(!p.seen(Pointer::Released) && !p.held());
    }

    #[test]
    fn a_move_with_no_button_down_ends_a_press() {
        let p = Press::default();
        p.seen(Pointer::Pressed);
        assert!(p.seen(Pointer::Moved { button_down: Some(false) }));
        assert!(!p.held());
        assert!(!p.seen(Pointer::Moved { button_down: Some(false) }), "once is enough");
    }

    #[test]
    fn leaving_ends_a_press_only_where_it_means_the_press_was_taken() {
        let p = Press::default();
        p.seen(Pointer::Pressed);
        assert!(!p.seen(Pointer::Left { taken: false }), "a drag past the window's edge carries on");
        assert!(p.held());
        assert!(p.seen(Pointer::Left { taken: true }));
    }

    #[test]
    fn losing_the_window_ends_a_press() {
        let p = Press::default();
        assert!(!p.seen(Pointer::Unfocused), "nothing to end");
        p.seen(Pointer::Pressed);
        assert!(p.seen(Pointer::Unfocused));
    }
}
