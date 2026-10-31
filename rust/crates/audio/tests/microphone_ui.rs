//! The Microphone plugin's .slint compiles against ui-kit, the way the shell builds plugin UI.

use std::path::Path;

fn spin_on<F: std::future::Future>(f: F) -> F::Output {
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    let mut f = std::pin::pin!(f);
    loop {
        if let std::task::Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
    }
}

#[test]
fn the_microphone_ui_compiles_with_every_component_it_hands_out() {
    // As the shell does: each piece imported into a window of its own making
    let doc = r#"import { MicrophoneCard } from "plugin/microphone.slint";
export component Wrapper inherits Window {
    VerticalLayout { MicrophoneCard { here: true; fix-shown: true; status-text: "x"; } }
}"#;
    let mut compiler = slint_interpreter::Compiler::default();
    compiler.set_library_paths([(telescope_ui_kit::LIBRARY.to_string(), telescope_ui_kit::library_path())].into_iter().collect());
    compiler.set_file_loader(|path: &Path| {
        let found = path.ends_with("plugin/microphone.slint").then(|| Ok(include_str!("../ui/microphone.slint").to_string()));
        Box::pin(async move { found })
    });
    let result = spin_on(compiler.build_from_source(doc.to_string(), Path::new("/root/wrapper.slint").to_path_buf()));
    let errors: Vec<String> = result.diagnostics().filter(|d| d.level() == slint_interpreter::DiagnosticLevel::Error).map(|d| d.to_string()).collect();
    assert!(errors.is_empty(), "{}", errors.join("\n"));
    assert!(result.component("Wrapper").is_some());
}

// test_vb_cable.py::test_the_question_names_vb_audio_and_says_what_they_ask, on the drawn card
#[test]
fn the_question_names_vb_audio_and_closing_it_is_a_no() {
    use i_slint_backend_testing::ElementHandle;
    use slint_interpreter::ComponentHandle;
    use slint_interpreter::Value;
    use std::cell::RefCell;
    use std::rc::Rc;
    i_slint_backend_testing::init_no_event_loop();
    let doc = r#"import { MicrophoneCard } from "plugin/microphone.slint";
export component Wrapper inherits Window {
    width: 400px;
    height: 600px;
    in property <int> asked;
    callback answered(bool);
    VerticalLayout {
        MicrophoneCard {
            here: true;
            origin: "Made by VB-Audio";
            donationware: "Donationware";
            donate-text: "Donate";
            confirm-asked: root.asked;
            confirm-answered(yes) => { root.answered(yes); }
        }
    }
}"#;
    let mut compiler = slint_interpreter::Compiler::default();
    compiler.set_library_paths([(telescope_ui_kit::LIBRARY.to_string(), telescope_ui_kit::library_path())].into_iter().collect());
    compiler.set_file_loader(|path: &Path| {
        let found = path.ends_with("plugin/microphone.slint").then(|| Ok(include_str!("../ui/microphone.slint").to_string()));
        Box::pin(async move { found })
    });
    let result = spin_on(compiler.build_from_source(doc.to_string(), Path::new("/root/wrapper.slint").to_path_buf()));
    let ui = result.component("Wrapper").unwrap().create().unwrap();
    ui.show().unwrap();
    let answers = Rc::new(RefCell::new(Vec::new()));
    let seen = answers.clone();
    ui.set_callback("answered", move |args| {
        seen.borrow_mut().push(args[0].clone());
        Value::Void
    }).unwrap();
    let ask = |n: f64| {
        ui.set_property("asked", Value::Number(n)).unwrap();
        slint::platform::update_timers_and_animations();
    };
    let shown = |label: &str| ElementHandle::find_by_accessible_label(&ui, label).next().is_some();
    assert!(!shown("Download and install"), "nothing is asked yet");
    ask(1.0);
    assert!(shown("Download and install") && shown("Made by VB-Audio\nDonationware"));
    // A click outside closes it, and that answers nothing
    use slint::platform::{PointerEventButton, WindowEvent};
    let outside = slint::LogicalPosition::new(390.0, 590.0);
    ui.window().dispatch_event(WindowEvent::PointerPressed { position: outside, button: PointerEventButton::Left });
    ui.window().dispatch_event(WindowEvent::PointerReleased { position: outside, button: PointerEventButton::Left });
    slint::platform::update_timers_and_animations();
    assert!(!shown("Download and install"));
    assert!(answers.borrow().is_empty());
    // And the next try asks again
    ask(2.0);
    assert!(shown("Download and install"), "the card went dead after a click outside");
}
