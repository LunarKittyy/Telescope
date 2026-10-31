//! Compiling a generated document with slint-interpreter. Plugins' .slint text, the shell's own frame and the kit come
//! through one file loader, which also puts every text in the person's language first (source::translate).

use crate::source::translate;
use slint_interpreter::{Compiler, ComponentDefinition};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// Where the files the shell makes up live. Nothing is read from here; the loader answers for it.
pub const ROOT: &str = "/telescope-ui";

/// The shell's own .slint files, by their path under ROOT.
pub fn shell_files() -> Vec<(String, String)> {
    vec![
        ("shell/frame.slint".into(), include_str!("../ui/frame.slint").into()),
        ("shell/settings.slint".into(), include_str!("../ui/settings.slint").into()),
        ("shell/preview.slint".into(), include_str!("../ui/preview.slint").into()),
    ]
}

/// Compiles `doc` (as ROOT/<name>) with `files` (paths under ROOT to source text) and returns `component`.
pub fn compile(name: &str, doc: &str, files: Vec<(String, String)>, component: &str) -> Result<ComponentDefinition, String> {
    let mut known: HashMap<PathBuf, String> = HashMap::new();
    for (path, text) in shell_files().into_iter().chain(files) {
        known.insert(Path::new(ROOT).join(path), translate(&text));
    }
    let known = Rc::new(known);
    let mut compiler = Compiler::default();
    compiler.set_library_paths([(telescope_ui_kit::LIBRARY.to_string(), telescope_ui_kit::library_path())].into_iter().collect());
    compiler.set_file_loader(move |path: &Path| {
        let found = known.get(path).cloned().or_else(|| {
            // A real file (the kit, an added plugin's folder): read it here so its texts get translated too
            (path.extension().is_some_and(|e| e == "slint")).then(|| std::fs::read_to_string(path).ok().map(|t| translate(&t))).flatten()
        });
        let found = found.map(Ok);
        Box::pin(async move { found })
    });
    let path = Path::new(ROOT).join(name);
    let result = spin_on(compiler.build_from_source(doc.to_string(), path));
    let mut errors = String::new();
    for d in result.diagnostics() {
        if d.level() == slint_interpreter::DiagnosticLevel::Error {
            errors.push_str(&format!("{d}\n"));
        } else {
            log::debug!("{d}");
        }
    }
    match result.component(component) {
        Some(def) if errors.is_empty() => Ok(def),
        _ => Err(if errors.is_empty() { format!("{component} isn't in {name}") } else { errors }),
    }
}

/// Compiling never waits on anything (the loader answers straight away), so polling once more is enough.
pub fn spin_on<F: std::future::Future>(f: F) -> F::Output {
    let waker = std::task::Waker::noop();
    let mut cx = std::task::Context::from_waker(waker);
    let mut f = std::pin::pin!(f);
    loop {
        if let std::task::Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
    }
}
