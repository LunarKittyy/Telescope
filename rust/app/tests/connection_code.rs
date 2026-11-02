//! Connection's pairing code on a real (offscreen) Slint window.

use i_slint_backend_testing::ElementHandle;
use serde_json::json;
use slint_interpreter::{Compiler, ComponentHandle, ComponentInstance, Value};
use std::path::Path;
use telescope_shell::offscreen;
use telescope_shell::source::from_json;

fn spin_on<F: std::future::Future>(f: F) -> F::Output {
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    let mut f = std::pin::pin!(f);
    loop {
        if let std::task::Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
    }
}

fn window(doc: &str) -> ComponentInstance {
    offscreen::install();
    let mut compiler = Compiler::default();
    compiler.set_library_paths([(telescope_ui_kit::LIBRARY.to_string(), telescope_ui_kit::library_path())].into_iter().collect());
    compiler.set_file_loader(|path: &Path| {
        let found = path.ends_with("plugin/connection.slint").then(|| Ok(include_str!("../../crates/link/ui/connection.slint").to_string()));
        Box::pin(async move { found })
    });
    let result = spin_on(compiler.build_from_source(doc.to_string(), Path::new("/root/wrapper.slint").to_path_buf()));
    let errors: Vec<String> = result.diagnostics().filter(|d| d.level() == slint_interpreter::DiagnosticLevel::Error).map(|d| d.to_string()).collect();
    assert!(errors.is_empty(), "{}", errors.join("\n"));
    let w = result.component("W").unwrap().create().unwrap();
    w.show().unwrap();
    w
}

// test_connection_dialogs.py::test_qr_widget_builds_matrix_and_renders, and the card's half of test_success_shows_the_phone_and_hands_the_result_over
#[test]
fn the_code_is_a_square_of_real_cells_and_goes_once_paired() {
    let (cells, size) = telescope_link::plugin::qr("hello");
    let w = window(r#"import { ConnectionCard } from "plugin/connection.slint";
export component W inherits Window {
    width: 600px;
    height: 700px;
    in property <[int]> cells;
    in property <int> size;
    in property <bool> paired;
    in property <bool> open;
    ConnectionCard { x: 0; y: 0; width: 500px; height: 10px; visible: true; add-open: open; qr-cells: cells; qr-size: size; paired: root.paired; add-subtitle: "Open it"; }
}"#);
    w.set_property("cells", from_json(&json!(cells))).unwrap();
    w.set_property("size", Value::Number(size as f64)).unwrap();
    offscreen::draw(w.window(), 600.0, 700.0, 1.0);
    w.set_property("open", Value::Bool(true)).unwrap();
    offscreen::pump();
    offscreen::draw(w.window(), 600.0, 700.0, 1.0);
    let code = ElementHandle::find_by_accessible_label(&w, "Pairing code").next().expect("the code");
    let side = code.size();
    assert!(side.width == side.height && side.width > 0.0, "{side:?}");
    assert_eq!(side.width, (size as f32 + 8.0) * 6.0);
    assert!(ElementHandle::find_by_accessible_label(&w, "Back").next().is_some());
    w.set_property("paired", Value::Bool(true)).unwrap();
    offscreen::pump();
    offscreen::draw(w.window(), 600.0, 700.0, 1.0);
    assert!(ElementHandle::find_by_accessible_label(&w, "Pairing code").next().is_none());
    assert!(ElementHandle::find_by_accessible_label(&w, "Done").next().is_some() && ElementHandle::find_by_accessible_label(&w, "Back").next().is_none());
}
