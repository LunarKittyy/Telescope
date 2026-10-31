//! Setting up the extra virtual cameras when a second stream needs one (app.py _set_up_slots, _ask_slots, _on_slots_ready):
//! it takes a password, so it asks first, and what wanted a camera in the meantime starts once they're there.

use telescope_core::i18n::Fields;
use crate::window::{with_window, Window};
use slint::ComponentHandle;
use slint_interpreter::{ComponentInstance, Value as SValue};
use std::cell::RefCell;
use std::rc::Rc;
use telescope_core::i18n::tr;
use telescope_core::output::{SetupAction, SetupResult, SETUP_CANCELLED};
use telescope_core::ui::{Issue, IssueAction, IssueDoes};

const QUESTION: &str = r#"
import { Ask } from "shell/settings.slint";

export component SlotsQuestion inherits Window {
    in property <string> window-title;
    in property <string> text;
    in property <string> yes;
    callback answered(bool);
    title: window-title;
    width: 460px;
    height: 220px;
    Ask {
        width: root.width;
        height: root.height;
        title: root.window-title;
        text: root.text;
        yes: root.yes;
        answered(yes) => { root.answered(yes); }
    }
}
"#;

#[derive(Default)]
pub(crate) struct SlotSetup {
    /// While the question is open or the cameras are being set up: the ids that start after.
    waiting: RefCell<Option<Vec<String>>>,
    question: RefCell<Option<ComponentInstance>>,
}

impl SlotSetup {
    /// source_id wants an extra camera; true when that's the first and it's time to ask.
    fn want(&self, source_id: &str) -> bool {
        let mut waiting = self.waiting.borrow_mut();
        match waiting.as_mut() {
            Some(ids) => {
                ids.push(source_id.to_string());
                false
            }
            None => {
                *waiting = Some(vec![source_id.to_string()]);
                true
            }
        }
    }

    fn cancel(&self) {
        *self.waiting.borrow_mut() = None;
    }

    fn finish(&self) -> Vec<String> {
        self.waiting.borrow_mut().take().unwrap_or_default()
    }
}

impl Window {
    /// Sets up the extra cameras (asking first), and starts source_id after. Already under way: it goes after the others.
    pub(crate) fn set_up_slots(self: &Rc<Self>, source_id: &str) {
        if self.streams.slots.want(source_id) {
            self.ask_slots();
        }
    }

    fn ask_slots(self: &Rc<Self>) {
        let outputs = self.services.as_ref().map(|s| s.outputs.clone());
        let label = |slot| outputs.as_ref().map_or_else(String::new, |o| o.slot_label(slot));
        let last = outputs.as_ref().map_or(1, |o| o.max_slots().saturating_sub(1));
        let names = tr("{first} to {last}").field("first", &label(1)).field("last", &label(last));
        let asks = if cfg!(target_os = "linux") { tr("This asks for your password.") } else { tr("Windows asks for permission.") };
        let text = format!("{} {asks}", tr("To stream several cameras at once, Telescope adds more virtual cameras ({names}).").field("names", &names));
        let built = crate::compile::compile("slots-question.slint", QUESTION, Vec::new(), "SlotsQuestion").and_then(|d| d.create().map_err(|e| e.to_string()));
        let ui = match built {
            Ok(ui) => ui,
            Err(e) => {
                log::error!("The question about more virtual cameras didn't build: {e}");
                return self.streams.slots.cancel();
            }
        };
        let string = |t: &str| SValue::String(t.into());
        let _ = ui.set_property("window-title", string(&tr("Set up more virtual cameras")));
        let _ = ui.set_property("text", string(&text));
        let _ = ui.set_property("yes", string(&tr("OK")));
        let weak = Rc::downgrade(self);
        let _ = ui.set_callback("answered", move |args| {
            let yes = args.first() == Some(&SValue::Bool(true));
            if let Some(w) = weak.upgrade() {
                // After the callback returns: the window it came from goes
                slint::Timer::single_shot(std::time::Duration::ZERO, move || w.slots_answered(yes));
            }
            SValue::Void
        });
        let weak = Rc::downgrade(self);
        ui.window().on_close_requested(move || {
            if let Some(w) = weak.upgrade() {
                slint::Timer::single_shot(std::time::Duration::ZERO, move || w.slots_answered(false));
            }
            slint::CloseRequestResponse::HideWindow
        });
        let _ = ui.show();
        *self.streams.slots.question.borrow_mut() = Some(ui);
    }

    fn slots_answered(self: &Rc<Self>, yes: bool) {
        let Some(question) = self.streams.slots.question.borrow_mut().take() else { return };
        let _ = question.hide();
        let Some(outputs) = self.services.as_ref().map(|s| s.outputs.clone()).filter(|_| yes) else {
            return self.streams.slots.cancel();
        };
        self.set_status(&tr("Setting up more virtual cameras…"), "dim");
        std::thread::spawn(move || {
            let run = || outputs.set_up(SetupAction::AddSlots, &|_| {});
            // Still reports back, so a later Add camera can try again
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(run)).unwrap_or_else(|_| {
                log::error!("Setting up more virtual cameras failed");
                SetupResult { ok: false, message: tr("Couldn't add more virtual cameras"), command: None }
            });
            let _ = slint::invoke_from_event_loop(move || with_window(|w| w.on_slots_ready(result)));
        });
    }

    fn on_slots_ready(self: &Rc<Self>, result: SetupResult) {
        let waiting = self.streams.slots.finish();
        let focus = self.streams.flow.borrow().sessions.focus.clone();
        if let Some(focus) = focus {
            let (msg, kind) = focus.state.borrow().status.clone();
            self.set_status(&msg, &kind);
        }
        if result.ok {
            self.state.borrow_mut().banners.clear(Some("vcam"), None);
            self.with_flow(move |flow, host| flow.slots_ready(host, waiting));
            return;
        }
        if result.message != SETUP_CANCELLED {
            let command = result.command.unwrap_or_default();
            let actions = (!command.is_empty()).then(|| IssueAction { label: tr("Copy command"), does: IssueDoes::OpenUrl { url: crate::host::copy_url(&command) } });
            self.state.borrow_mut().banners.show("vcam", Issue {
                title: tr("Couldn't add more virtual cameras"), text: result.message, actions: actions.into_iter().collect(),
                kind: "warn".into(), details: command, tell_dismiss: false,
            });
        }
        self.process();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whoever_asks_while_the_cameras_are_set_up_waits_behind_the_first() {
        let setup = SlotSetup::default();
        assert!(setup.want("b"), "the first one asks");
        assert!(!setup.want("c"));
        assert_eq!(setup.finish(), ["b", "c"]);
        assert!(setup.want("d"), "and after that it can ask again");
        setup.cancel();
        assert!(setup.want("e"));
    }
}
