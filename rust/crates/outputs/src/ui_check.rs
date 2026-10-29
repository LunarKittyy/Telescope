//! Tests: a card's .slint compiles against ui-kit, the way the shell builds plugin UI.

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

pub fn compiles(source: &'static str, file: &str, component: &str) {
    let doc = format!("import {{ {component} }} from \"plugin/{file}\";\nexport component Wrapper inherits Window {{ VerticalLayout {{ {component} {{}} }} }}");
    let mut compiler = slint_interpreter::Compiler::default();
    compiler.set_library_paths([(telescope_ui_kit::LIBRARY.to_string(), telescope_ui_kit::library_path())].into_iter().collect());
    let want = format!("plugin/{file}");
    compiler.set_file_loader(move |path: &Path| {
        let found = path.ends_with(&want).then(|| Ok(source.to_string()));
        Box::pin(async move { found })
    });
    let result = spin_on(compiler.build_from_source(doc, Path::new("/root/wrapper.slint").to_path_buf()));
    let errors: Vec<String> = result.diagnostics().filter(|d| d.level() == slint_interpreter::DiagnosticLevel::Error).map(|d| d.to_string()).collect();
    assert!(errors.is_empty(), "{}", errors.join("\n"));
    assert!(result.component("Wrapper").is_some());
}
