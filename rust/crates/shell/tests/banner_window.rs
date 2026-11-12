// test_banner.py on the real window: banners drawn from the state, real clicks on their buttons and the real dispatch behind them.
use i_slint_backend_testing::ElementHandle;
use slint::platform::{PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition};
use slint_interpreter::Value;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use telescope_core::config::{ConfigFile, CONFIG_FILENAME};
use telescope_core::plugin::{Plugin, PluginInfo};
use telescope_core::ui::{Issue, IssueAction, IssueDoes};
use telescope_core::Host;
use telescope_shell::window::{self, Window};

type Heard = Arc<Mutex<Vec<(String, String)>>>;

/// Whoever shows the issues: hears the buttons and, for "replace", shows a new issue as Python's lambda does.
struct Asker(Heard);

impl Plugin for Asker {
    fn info(&self) -> PluginInfo {
        PluginInfo { name: "asker".into(), title: "Asker".into(), ..Default::default() }
    }
    fn on_issue_action(&mut self, host: &mut dyn Host, key: &str, id: &str) {
        self.0.lock().unwrap().push((key.to_string(), id.to_string()));
        if id == "replace" {
            host.show_issue(key, issue("New", ""));
        }
    }
}

/// A plugin that did not show any banner and writes down every button it hears about.
struct Bystander(Heard);

impl Plugin for Bystander {
    fn info(&self) -> PluginInfo {
        PluginInfo { name: "bystander".into(), title: "Bystander".into(), ..Default::default() }
    }
    fn on_issue_action(&mut self, _host: &mut dyn Host, key: &str, id: &str) {
        self.0.lock().unwrap().push((key.to_string(), id.to_string()));
    }
}

fn opened(label: &str) -> (Rc<Window>, Heard) {
    let (w, heard, _) = opened_with_bystander(label);
    (w, heard)
}

fn opened_with_bystander(label: &str) -> (Rc<Window>, Heard, Heard) {
    i_slint_backend_testing::init_no_event_loop();
    let dir = telescope_core::testing::scratch_dir(label);
    let heard = Heard::default();
    let bystander = Heard::default();
    let (asker, other) = (heard.clone(), bystander.clone());
    let w = window::build(ConfigFile::at(dir.join(CONFIG_FILENAME)),
                          move |_| vec![Box::new(Asker(asker)) as Box<dyn Plugin>, Box::new(Bystander(other))], None, None).unwrap();
    w.ui.window().set_size(slint::LogicalSize::new(1300.0, 900.0));
    // A CI machine's own setting can be animations off, which snaps what these tests watch move
    telescope_shell::motion::set(&w.ui, telescope_shell::motion::Motion::Full);
    (w, heard, bystander)
}

fn issue(title: &str, text: &str) -> Issue {
    Issue { title: title.into(), text: text.into(), actions: vec![], kind: "err".into(), details: String::new(), tell_dismiss: false }
}

fn acting(title: &str, label: &str, does: IssueDoes) -> Issue {
    Issue { actions: vec![IssueAction { label: label.into(), does }], ..issue(title, "") }
}

fn plugin_action(id: &str) -> IssueDoes {
    IssueDoes::Plugin { id: id.into() }
}

fn show(w: &Rc<Window>, key: &str, shown: Issue) {
    w.state.borrow_mut().banners.show(key, shown);
    w.process();
    settled(w);
}

fn clear(w: &Rc<Window>, key: Option<&str>, only: Option<&Issue>) {
    w.state.borrow_mut().banners.clear(key, only);
    w.process();
}

/// The strips shut and the rows that were going gone (their motion is a third of a second or so).
fn settled(w: &Rc<Window>) {
    for _ in 0..40 {
        // Looked at as a screen reader would (a banner is made when it is first laid out), then a frame later
        let _ = drawn(w, "");
        i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(50));
    }
    w.process();
}

fn keys(w: &Rc<Window>) -> Vec<String> {
    w.state.borrow().banners.keys()
}

fn title(w: &Rc<Window>, key: &str) -> Option<String> {
    w.state.borrow().banners.issue(key).map(|i| i.title.clone())
}

fn drawn(w: &Rc<Window>, label: &str) -> Vec<ElementHandle> {
    ElementHandle::find_by_accessible_label(&w.ui, label).collect()
}

/// BannerArea's own height: nothing at all while it holds no banners, as a hidden widget takes up no room.
fn area_height(w: &Rc<Window>) -> f32 {
    let area: Vec<ElementHandle> = ElementHandle::find_by_element_type_name(&w.ui, "BannerArea").collect();
    assert_eq!(area.len(), 1);
    area[0].size().height
}

/// A press and release of the mouse in the middle of what's drawn, as a click goes through Qt.
fn click(w: &Rc<Window>, on: &ElementHandle) {
    let (at, size) = (on.absolute_position(), on.size());
    let position = LogicalPosition::new(at.x + size.width / 2.0, at.y + size.height / 2.0);
    let window = w.ui.window();
    window.dispatch_event(WindowEvent::PointerMoved { position });
    window.dispatch_event(WindowEvent::PointerPressed { position, button: PointerEventButton::Left });
    window.dispatch_event(WindowEvent::PointerReleased { position, button: PointerEventButton::Left });
    window.dispatch_event(WindowEvent::PointerExited);
}

fn click_label(w: &Rc<Window>, label: &str) {
    let found = drawn(w, label);
    assert!(!found.is_empty(), "{label} on screen");
    click(w, &found[0]);
}

// test_banner.py::test_keys_replace_and_clear
#[test]
fn keys_replace_and_clear() {
    let (w, _) = opened("banner-keys");
    assert_eq!(area_height(&w), 0.0, "nothing to show");
    show(&w, "start", issue("First", ""));
    show(&w, "vcam", issue("Other", ""));
    show(&w, "start", issue("Second", ""));
    assert_eq!(keys(&w), ["vcam", "start"]);
    assert_eq!(title(&w, "start").as_deref(), Some("Second"));
    assert!(drawn(&w, "First").is_empty() && !drawn(&w, "Second").is_empty());
    clear(&w, Some("vcam"), None);
    assert_eq!(keys(&w), ["start"]);
    settled(&w);
    assert!(area_height(&w) > 0.0, "one left");
    clear(&w, None, None);
    assert!(keys(&w).is_empty());
    assert!(area_height(&w) > 0.0, "it shuts over a few frames, it does not vanish");
    settled(&w);
    assert_eq!(area_height(&w), 0.0);
    assert_eq!(w.ui.get_property("banners").map(|v| matches!(v, Value::Model(m) if slint::Model::row_count(&m) == 0)), Ok(true), "and its row goes after");
}

// test_banner.py::test_an_action_runs_and_takes_the_banner_with_it
#[test]
fn an_action_runs_and_takes_the_banner_with_it() {
    let (w, heard) = opened("banner-action");
    show(&w, "start", Issue { text: "Open the app.".into(), ..acting("Can't connect", "Try again", plugin_action("again")) });
    click_label(&w, "Try again");
    assert_eq!(*heard.lock().unwrap(), [("start".to_string(), "again".to_string())], "ran once");
    assert!(keys(&w).is_empty());
    assert!(drawn(&w, "Can't connect").is_empty());
}

#[test]
fn a_button_of_an_added_plugins_banner_reaches_only_that_plugin() {
    let (w, asker, bystander) = opened_with_bystander("banner-owner");
    let key = telescope_core::ui::added_issue_key("asker", "k");
    show(&w, &key, acting("Mine", "Do mine", plugin_action("mine")));
    click_label(&w, "Do mine");
    assert_eq!(*asker.lock().unwrap(), [(key.clone(), "mine".to_string())]);
    assert!(bystander.lock().unwrap().is_empty(), "another plugin never hears a banner that isn't its own");
    // The app's own banners still go to every plugin
    show(&w, "start", acting("Theirs", "Do theirs", plugin_action("theirs")));
    click_label(&w, "Do theirs");
    assert_eq!(*bystander.lock().unwrap(), [("start".to_string(), "theirs".to_string())]);
}

// test_banner.py::test_an_action_that_replaces_the_issue_keeps_the_new_one
#[test]
fn an_action_that_replaces_the_issue_keeps_the_new_one() {
    let (w, _) = opened("banner-replace");
    show(&w, "start", acting("Old", "Try again", plugin_action("replace")));
    click_label(&w, "Try again");
    assert_eq!(title(&w, "start").as_deref(), Some("New"));
    assert_eq!(keys(&w), ["start"]);
    assert!(!drawn(&w, "New").is_empty() && drawn(&w, "Old").is_empty());
}

// test_banner.py::test_copy_keeps_the_banner_and_fills_the_clipboard
#[test]
fn copy_keeps_the_banner_and_fills_the_clipboard() {
    let (w, _) = opened("banner-copy");
    let command = "sudo modprobe -r v4l2loopback";
    let copy = IssueDoes::OpenUrl { url: format!("{}{command}", telescope_shell::banners::COPY_SCHEME) };
    show(&w, "start", Issue { details: command.into(), ..acting("Run this", "Copy command", copy) });
    let details: Vec<ElementHandle> = ElementHandle::find_by_element_type_name(&w.ui, "TextInput").filter(|e| e.accessible_value().as_deref() == Some(command)).collect();
    assert_eq!(details.len(), 1, "the command shows");
    click_label(&w, "Copy command");
    let pasted = w.ui.invoke("paste-text", &[]).unwrap();
    assert_eq!(pasted, Value::String(command.into()));
    assert_eq!(keys(&w), ["start"]);
}

// test_banner.py::test_dismiss
#[test]
fn dismiss() {
    let (w, heard) = opened("banner-dismiss");
    show(&w, "start", issue("Gone soon", ""));
    click_label(&w, "Dismiss");
    assert!(keys(&w).is_empty());
    assert!(drawn(&w, "Gone soon").is_empty());
    assert!(heard.lock().unwrap().is_empty(), "nobody asked to hear");
}

// test_banner.py::test_closing_tells_whoever_showed_it
#[test]
fn closing_tells_whoever_showed_it() {
    let (w, heard) = opened("banner-closing");
    let waiting = Issue { tell_dismiss: true, ..issue("Waiting", "") };
    show(&w, "start", waiting.clone());
    clear(&w, Some("start"), None);
    assert!(heard.lock().unwrap().is_empty(), "cleared by the app: not a close");
    show(&w, "start", waiting);
    click_label(&w, "Dismiss");
    assert_eq!(*heard.lock().unwrap(), [("start".to_string(), "dismiss".to_string())]);
    assert!(keys(&w).is_empty());
}

// test_banner.py::test_clearing_one_issue_leaves_a_newer_one_under_the_same_key
#[test]
fn clearing_one_issue_leaves_a_newer_one_under_the_same_key() {
    let (w, _) = opened("banner-newer");
    let (old, new) = (issue("Old", "x"), issue("New", "y"));
    show(&w, "start", old.clone());
    show(&w, "start", new.clone());
    clear(&w, Some("start"), Some(&old));
    assert_eq!(title(&w, "start").as_deref(), Some("New"));
    clear(&w, Some("start"), Some(&new));
    assert_eq!(title(&w, "start"), None);
}
