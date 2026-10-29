//! The system crate's .slint files compile against ui-kit, the way the shell builds plugin UI.

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

fn errors(doc: &str, file: &'static str, text: &'static str) -> Vec<String> {
    let mut compiler = slint_interpreter::Compiler::default();
    compiler.set_library_paths([(telescope_ui_kit::LIBRARY.to_string(), telescope_ui_kit::library_path())].into_iter().collect());
    compiler.set_file_loader(move |path: &Path| {
        let found = path.ends_with(file).then(|| Ok(text.to_string()));
        Box::pin(async move { found })
    });
    let result = spin_on(compiler.build_from_source(doc.to_string(), Path::new("/root/wrapper.slint").to_path_buf()));
    let mut errors: Vec<String> = result.diagnostics().filter(|d| d.level() == slint_interpreter::DiagnosticLevel::Error).map(|d| d.to_string()).collect();
    if errors.is_empty() && result.component("Wrapper").is_none() {
        errors.push("no Wrapper".into());
    }
    errors
}

#[test]
fn the_updates_ui_compiles_with_every_component_it_hands_out() {
    // As the shell does: each piece imported into a window of its own making
    let doc = r#"import { UpdateButton, UpdatesSettings } from "plugin/updates.slint";
export component Wrapper inherits Window {
    VerticalLayout { UpdateButton {} UpdatesSettings {} }
}"#;
    let mut compiler = slint_interpreter::Compiler::default();
    compiler.set_library_paths([(telescope_ui_kit::LIBRARY.to_string(), telescope_ui_kit::library_path())].into_iter().collect());
    compiler.set_file_loader(|path: &Path| {
        let found = path.ends_with("plugin/updates.slint").then(|| Ok(include_str!("../ui/updates.slint").to_string()));
        Box::pin(async move { found })
    });
    let result = spin_on(compiler.build_from_source(doc.to_string(), Path::new("/root/wrapper.slint").to_path_buf()));
    let errors: Vec<String> = result.diagnostics().filter(|d| d.level() == slint_interpreter::DiagnosticLevel::Error).map(|d| d.to_string()).collect();
    assert!(errors.is_empty(), "{}", errors.join("\n"));
    assert!(result.component("Wrapper").is_some());
}

#[test]
fn the_shortcuts_page_compiles() {
    let doc = r#"import { ShortcutsSettings } from "plugin/shortcuts.slint";
export component Wrapper inherits Window {
    ShortcutsSettings {}
}"#;
    let errors = errors(doc, "plugin/shortcuts.slint", include_str!("../ui/shortcuts.slint"));
    assert!(errors.is_empty(), "{}", errors.join("\n"));
}
