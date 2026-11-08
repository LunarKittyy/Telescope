// The Add a phone dialog on both of its tabs, opened out of the Connection card, in both looks, for a look by eye:
// CARD_SHOTS=dir cargo nextest run -p telescope-app --test r1_dialog_shots, each as NAME-new.png and NAME-classic.png at 2x.
use serde_json::json;
use slint::ComponentHandle;
use slint_interpreter::{Compiler, Value};
use std::path::PathBuf;
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

#[test]
fn add_phone_dialog_shots() {
    let Ok(dir) = std::env::var("CARD_SHOTS") else { return };
    let dir = PathBuf::from(dir);
    std::fs::create_dir_all(&dir).unwrap();
    offscreen::install();
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../crates/link/ui/connection.slint");
    let doc = format!(
        "import {{ ConnectionCard }} from \"{}\";\nimport {{ Theme, WindowBox }} from \"@ui-kit/ui-kit.slint\";\nexport {{ Theme, WindowBox }}\nexport component W inherits Window {{\n    width: 560px;\n    height: 720px;\n    background: Theme.ground;\n    in-out property <int> page;\n    in property <[int]> cells;\n    in property <int> modules;\n    init => {{ WindowBox.width = self.width; WindowBox.height = self.height; }}\n    card := ConnectionCard {{ x: 16px; y: 16px; width: 528px; hue: Theme.plugin-hue(\"streaming\"); add-open: true; other-name: \"Browser camera\"; add-subtitle: \"Open Telescope on the phone first.\"; usb-text: \"Needs adb, which isn't installed. Wi-Fi works without it.\"; usb-kind: \"status_dim\"; wifi-text: \"Open Telescope on the phone, then scan this code.\"; qr-cells: root.cells; qr-size: root.modules; add-page <=> root.page; }}\n}}",
        path.display()
    );
    let mut compiler = Compiler::default();
    compiler.set_library_paths([(telescope_ui_kit::LIBRARY.to_string(), telescope_ui_kit::library_path())].into_iter().collect());
    let result = spin_on(compiler.build_from_source(doc, PathBuf::from("/root/wrapper.slint")));
    let errors: Vec<String> = result.diagnostics().filter(|d| d.level() == slint_interpreter::DiagnosticLevel::Error).map(|d| d.to_string()).collect();
    assert!(errors.is_empty(), "{}", errors.join("\n"));
    let def = result.component("W").unwrap();
    let (cells, size) = telescope_core::qr::encode("https://example.test/pair#abcdef");
    for classic in [false, true] {
        for page in [0, 1] {
            let w = def.create().unwrap();
            w.set_global_property("Theme", "classic", Value::Bool(classic)).unwrap();
            w.set_property("cells", from_json(&json!(cells))).unwrap();
            w.set_property("modules", Value::Number(size as f64)).unwrap();
            w.show().unwrap();
            offscreen::pump();
            w.set_property("page", Value::Number(page as f64)).unwrap();
            offscreen::pump();
            let pic = offscreen::draw(w.window(), 560.0, 720.0, 2.0).expect("a picture");
            pic.save_png(&dir.join(format!("add-phone-tab{page}-{}.png", if classic { "classic" } else { "new" }))).unwrap();
        }
    }
}
