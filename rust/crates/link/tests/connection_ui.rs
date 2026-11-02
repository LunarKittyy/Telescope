//! The Connection plugin's .slint compiles against ui-kit, the way the shell builds plugin UI.

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
fn the_connection_ui_compiles_with_every_component_it_hands_out() {
    // As the shell does: each piece imported into a window of its own making
    let doc = r#"import { ConnectionCard, PhonePicker, ConnectionSettings } from "plugin/connection.slint";
export component Wrapper inherits Window {
    VerticalLayout { PhonePicker {} ConnectionCard {} ConnectionSettings {} }
}"#;
    let mut compiler = slint_interpreter::Compiler::default();
    compiler.set_library_paths([(telescope_ui_kit::LIBRARY.to_string(), telescope_ui_kit::library_path())].into_iter().collect());
    compiler.set_file_loader(|path: &Path| {
        let found = path.ends_with("plugin/connection.slint").then(|| Ok(include_str!("../ui/connection.slint").to_string()));
        Box::pin(async move { found })
    });
    let result = spin_on(compiler.build_from_source(doc.to_string(), Path::new("/root/wrapper.slint").to_path_buf()));
    let errors: Vec<String> = result.diagnostics().filter(|d| d.level() == slint_interpreter::DiagnosticLevel::Error).map(|d| d.to_string()).collect();
    assert!(errors.is_empty(), "{}", errors.join("\n"));
    assert!(result.component("Wrapper").is_some());
}
