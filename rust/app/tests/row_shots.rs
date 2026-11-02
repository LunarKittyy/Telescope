//! Pictures of rows whose note wraps, for a look by eye: SHOTS=dir cargo test --test row_shots, at 1x and 2x.

use slint_interpreter::{Compiler, ComponentHandle};
use std::path::{Path, PathBuf};
use telescope_shell::offscreen;

fn spin_on<F: std::future::Future>(f: F) -> F::Output {
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    let mut f = std::pin::pin!(f);
    loop {
        if let std::task::Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
    }
}

/// A card from a plugin's ui file in a window, with `props` set the way Slint text writes them.
fn shoot(dir: &Path, name: &str, file: &str, component: &str, props: &str, size: (f32, f32)) {
    offscreen::install();
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join(file);
    let doc = format!(
        "import {{ {component} }} from \"{}\";\nexport component W inherits Window {{\n    width: {}px;\n    height: {}px;\n    {component} {{ width: parent.width; {props} }}\n}}",
        path.display(),
        size.0,
        size.1
    );
    let mut compiler = Compiler::default();
    compiler.set_library_paths([(telescope_ui_kit::LIBRARY.to_string(), telescope_ui_kit::library_path())].into_iter().collect());
    let result = spin_on(compiler.build_from_source(doc, PathBuf::from("/root/wrapper.slint")));
    let errors: Vec<String> = result.diagnostics().filter(|d| d.level() == slint_interpreter::DiagnosticLevel::Error).map(|d| d.to_string()).collect();
    assert!(errors.is_empty(), "{}", errors.join("\n"));
    let w = result.component("W").unwrap().create().unwrap();
    w.show().unwrap();
    for scale in [1.0f32, 2.0] {
        let pic = offscreen::draw(w.window(), size.0, size.1, scale).expect("a picture");
        pic.save_png(&dir.join(format!("{name}-{scale}x.png"))).unwrap();
    }
}

#[test]
fn shots() {
    let Ok(dir) = std::env::var("SHOTS") else { return };
    let dir = PathBuf::from(dir);
    std::fs::create_dir_all(&dir).unwrap();
    let missing = "RNNoise is missing from this copy of Telescope. Reinstalling brings it back.";
    let mic = "active: true; here: true; on: true; nr-enabled: false;";
    shoot(&dir, "mic-note", "crates/audio/ui/microphone.slint", "MicrophoneCard", &format!("{mic} nr-note: \"{missing}\";"), (460.0, 330.0));
    shoot(&dir, "mic-plain", "crates/audio/ui/microphone.slint", "MicrophoneCard", &format!("{mic} nr: true;"), (460.0, 330.0));
    shoot(
        &dir,
        "mic-status",
        "crates/audio/ui/microphone.slint",
        "MicrophoneCard",
        &format!("{mic} nr-note: \"{missing}\"; status-text: \"{missing} {missing}\";"),
        (460.0, 420.0),
    );
}
