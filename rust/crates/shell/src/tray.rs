//! The tray icon (app.py's _setup_tray and what goes with it): Show, the plugins' tray actions and Quit, a click that
//! shows or hides the window, a tooltip that says what's streaming, and closing to the tray while something streams or
//! a plugin keeps Telescope there.

use crate::shell::Effect;
use crate::window::{with_window, Window};
use slint::ComponentHandle;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use telescope_core::event::Event;
use telescope_core::i18n::tr;
use telescope_core::system::TrayItem;

const SHOW: &str = "show";
const QUIT: &str = "quit";

/// A raise that came before Slint's backend existed, kept for the window to honour once the event loop is up.
static RAISE_WAITING: AtomicBool = AtomicBool::new(false);

/// A second start asked for the window (Python's _sig_raise); any thread.
pub fn raise() {
    raise_with(|| slint::invoke_from_event_loop(|| with_window(|w| w.show_window())).is_ok());
}

/// raise() with how to reach the event loop; a request it can't reach is kept for take_raise.
fn raise_with(send: impl FnOnce() -> bool) {
    log::info!("Another start asked this window to show itself");
    if !send() {
        RAISE_WAITING.store(true, Ordering::SeqCst);
    }
}

/// Whether a raise is waiting from before the event loop could take it; asking clears it.
pub fn take_raise() -> bool {
    RAISE_WAITING.swap(false, Ordering::SeqCst)
}

/// Starts this copy again, from any thread (win.restart_app()).
pub fn restart() {
    let _ = slint::invoke_from_event_loop(|| with_window(|w| {
        w.restart_app();
    }));
}

/// The tray's tooltip for what's streaming (_show_tray_state).
pub fn tooltip(streaming: bool, camera_on: bool, mic: (bool, bool)) -> String {
    let mic = match mic {
        (true, true) => Some("muted"),
        (true, false) => Some("on"),
        _ => None,
    };
    match (streaming, camera_on, mic) {
        (false, _, _) => "Telescope".into(),
        (true, true, None) => tr("Telescope: streaming the camera"),
        (true, true, Some("on")) => tr("Telescope: streaming the camera and mic"),
        (true, true, Some(_)) => tr("Telescope: streaming the camera, mic muted"),
        (true, false, Some("muted")) => tr("Telescope: streaming the mic, camera off (muted)"),
        (true, false, _) => tr("Telescope: streaming the mic, camera off"),
    }
}

impl Window {
    pub(crate) fn has_tray(&self) -> bool {
        self.services.as_ref().is_some_and(|s| s.tray.is_some())
    }

    fn with_tray(&self, f: impl FnOnce(&mut dyn telescope_core::system::Tray)) {
        if let Some(tray) = self.services.as_ref().and_then(|s| s.tray.as_ref()) {
            f(tray.lock().unwrap().as_mut());
        }
    }

    /// Show, every plugin's tray actions in registration order, then Quit.
    pub fn refresh_tray_menu(&self) {
        let item = |id: String, title: String| TrayItem { id, title, checkable: false, checked: false, visible: true, enabled: true };
        let mut items = vec![item(SHOW.into(), tr("Show"))];
        for slot in &self.state.borrow().slots {
            items.extend(slot.tray.iter().map(|a| TrayItem {
                id: format!("{}/{}", slot.info.name, a.id),
                title: a.title.clone(),
                checkable: a.checkable,
                checked: a.checked,
                visible: a.visible,
                enabled: true,
            }));
        }
        items.push(item(String::new(), String::new())); // a line above Quit
        items.push(item(QUIT.into(), tr("Quit")));
        self.with_tray(|t| t.set_items(&items));
    }

    pub fn refresh_tray_tip(&self) {
        let tip = {
            let st = self.state.borrow();
            let (streaming, camera_on) = (st.streams.streaming, !st.streams.camera_off);
            let mic = (streaming && st.mic.0).then_some(st.mic.1);
            (tooltip(streaming, camera_on, st.mic), streaming && camera_on, mic)
        };
        self.with_tray(|t| {
            t.set_tooltip(&tip.0);
            t.set_state(tip.1, tip.2);
        });
    }

    /// What was clicked in the tray since last asked: the icon toggles the window, the menu does what it says.
    pub fn poll_tray(self: &Rc<Self>) {
        let mut clicks = Vec::new();
        self.with_tray(|t| {
            while let Some(c) = t.poll_click() {
                clicks.push(c);
            }
        });
        if take_raise() {
            self.show_window();
        }
        for click in clicks {
            match click.as_str() {
                "" if self.ui.window().is_visible() => self.hide_window(),
                "" | SHOW => self.show_window(),
                QUIT => self.quit("The user quit from the tray"),
                id => {
                    let Some((plugin, action)) = id.split_once('/') else { continue };
                    let at = self.state.borrow().index(plugin);
                    if let Some(i) = at {
                        self.state.borrow_mut().call(i, |p, host| p.on_tray_action(host, action));
                    }
                    self.process();
                }
            }
        }
        self.refresh_tray_tip();
    }

    /// _tray_show: up, not minimized, in front.
    pub fn show_window(&self) {
        use slint::winit_030::WinitWindowAccessor;
        let _ = self.ui.show();
        crate::frame::dress(&self.ui);
        self.ui.window().set_minimized(false);
        self.ui.window().with_winit_window(|w| w.focus_window());
        self.keep_on_screen();
        self.report_shown();
        self.reattach_children();
    }

    /// The windows open under this one go back under it: on Wayland they left it when its toplevel went with the window's hiding.
    fn reattach_children(&self) {
        if let Some(settings) = crate::settings::window() {
            crate::parent::attach(&settings, &self.ui);
        }
        for (_, _, open) in self.plugin_windows.borrow().iter() {
            crate::parent::attach(open, &self.ui);
        }
        if let Some(question) = self.slots_question() {
            crate::parent::attach(&question, &self.ui);
        }
    }

    /// As it first opens: a window bigger than the screen's work area is cut down to it and one sticking out is moved in.
    fn keep_on_screen(&self) {
        if self.fitted.replace(true) {
            return;
        }
        let scale = self.ui.window().scale_factor();
        crate::screen::keep_inside(&self.ui, ((560.0 * scale) as u32, (520.0 * scale) as u32));
    }

    fn hide_window(&self) {
        let _ = self.ui.hide();
        self.report_shown();
    }

    /// toggle_window: away if it's up and in front (into the tray, or minimized without one), else up.
    pub(crate) fn toggle_window(&self) {
        let w = self.ui.window();
        if w.is_visible() && !w.is_minimized() && self.state.borrow().window_active {
            if self.has_tray() {
                self.hide_window();
            } else {
                w.set_minimized(true);
                self.report_shown();
            }
        } else {
            self.show_window();
        }
    }

    /// start_hidden, for --minimized: in the tray, or minimized where there's none.
    pub fn start_hidden(&self) {
        if !self.has_tray() {
            let _ = self.ui.show();
            crate::frame::dress(&self.ui);
            self.ui.window().set_minimized(true);
        }
        self.report_shown();
    }

    /// _report_shown: the plugins hear whether the window is on screen (the tray and minimized both count as not).
    /// Whether that changed.
    pub fn report_shown(&self) -> bool {
        let w = self.ui.window();
        let shown = w.is_visible() && !w.is_minimized();
        let changed = {
            let mut st = self.state.borrow_mut();
            let changed = st.shown != Some(shown);
            st.shown = Some(shown);
            changed
        };
        if changed {
            self.state.borrow_mut().bus.emit(Event::WindowShown { shown });
        }
        changed
    }

    /// closeEvent: into the tray while something streams or a plugin keeps Telescope there (saying so the first time),
    /// else quit.
    pub fn close_requested(self: &Rc<Self>) -> slint::CloseRequestResponse {
        let keep = {
            let st = self.state.borrow();
            st.streams.streaming || st.keep_in_tray
        };
        if !(self.has_tray() && keep) {
            self.quit("The user closed the window");
            return slint::CloseRequestResponse::HideWindow;
        }
        let first = !std::mem::replace(&mut self.state.borrow_mut().tray_close_notified, true);
        if first {
            let streaming = self.state.borrow().streams.streaming;
            let body = if streaming { tr("Streaming continues in the background.") } else { tr("Streaming starts automatically when needed.") }
                + " " + &tr("Right-click the tray icon to quit.");
            self.state.borrow_mut().effects.push(Effect::Notify { title: tr("Telescope is still running"), body, urgent: false });
        }
        // Hidden after this returns; the plugins hear it then
        let weak = Rc::downgrade(self);
        slint::Timer::single_shot(std::time::Duration::ZERO, move || {
            if let Some(w) = weak.upgrade() {
                w.report_shown();
                w.process();
            }
        });
        self.process();
        slint::CloseRequestResponse::HideWindow
    }

    /// _tray_quit: the event loop ends and run() shuts everything down.
    pub(crate) fn quit(&self, reason: &str) {
        log::info!("Quitting: {reason}");
        telescope_core::diagnostics::events().add("QUIT", reason);
        self.state.borrow_mut().tray_close_notified = true;
        let _ = slint::quit_event_loop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tray_tip_says_what_streams() {
        assert_eq!(tooltip(false, true, (true, false)), "Telescope");
        assert_eq!(tooltip(true, true, (false, false)), "Telescope: streaming the camera");
        assert_eq!(tooltip(true, true, (true, false)), "Telescope: streaming the camera and mic");
        assert_eq!(tooltip(true, true, (true, true)), "Telescope: streaming the camera, mic muted");
        assert_eq!(tooltip(true, false, (true, true)), "Telescope: streaming the mic, camera off (muted)");
        assert_eq!(tooltip(true, false, (true, false)), "Telescope: streaming the mic, camera off");
    }

    #[test]
    fn a_raise_before_the_event_loop_is_kept() {
        assert!(!take_raise());
        raise_with(|| false);
        assert!(take_raise());
        assert!(!take_raise(), "taken once");
        raise_with(|| true);
        assert!(!take_raise(), "one that went through isn't kept");
    }
}
