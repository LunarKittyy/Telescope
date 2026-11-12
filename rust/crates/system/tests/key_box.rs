//! The shortcut editor's key box records the keys pressed in it, except the ones that get the keyboard out of it again.

use slint::platform::{Key, WindowEvent};
use slint::ComponentHandle;
use slint_interpreter::{ComponentInstance, Value};
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

const DOC: &str = r#"import { KeyBox } from "plugin/shortcuts.slint";
export component Wrapper inherits Window {
    width: 300px;
    height: 80px;
    out property <int> presses;
    KeyBox { width: 200px; pressed(t, c, a, s, m) => { presses += 1; } }
}"#;

fn rig() -> ComponentInstance {
    thread_local!(static INIT: std::sync::Once = const { std::sync::Once::new() });
    INIT.with(|once| once.call_once(i_slint_backend_testing::init_no_event_loop));
    let mut compiler = slint_interpreter::Compiler::default();
    compiler.set_library_paths([(telescope_ui_kit::LIBRARY.to_string(), telescope_ui_kit::library_path())].into_iter().collect());
    compiler.set_file_loader(|path: &Path| {
        let found = path.ends_with("plugin/shortcuts.slint").then(|| Ok(include_str!("../ui/shortcuts.slint").to_string()));
        Box::pin(async move { found })
    });
    let result = spin_on(compiler.build_from_source(DOC.to_string(), Path::new("/root/wrapper.slint").to_path_buf()));
    let errors: Vec<String> = result.diagnostics().map(|d| d.to_string()).collect();
    let ui = result.component("Wrapper").unwrap_or_else(|| panic!("{}", errors.join("\n"))).create().unwrap();
    ui.window().set_size(slint::LogicalSize::new(300.0, 80.0));
    ui.show().unwrap();
    // The first Tab lands on the box, the only thing here that takes the focus
    press(&ui, Key::Tab);
    ui
}

fn press(ui: &ComponentInstance, key: impl Into<slint::SharedString>) {
    let text: slint::SharedString = key.into();
    ui.window().dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
    ui.window().dispatch_event(WindowEvent::KeyReleased { text });
}

fn presses(ui: &ComponentInstance) -> i32 {
    match ui.get_property("presses") {
        Ok(Value::Number(n)) => n as i32,
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_key_box_records_keys_but_lets_tab_and_escape_through() {
    let ui = rig();
    press(&ui, "a");
    assert_eq!(presses(&ui), 1, "a letter is recorded");
    for key in [Key::Tab, Key::Backtab, Key::Escape] {
        let ui = rig();
        press(&ui, key);
        assert_eq!(presses(&ui), 0, "{key:?} is left to focus and the dialog, not recorded");
    }
}
