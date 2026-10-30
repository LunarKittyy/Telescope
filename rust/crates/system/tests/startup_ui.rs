//! The Startup sections' .slint compile against ui-kit the way the shell builds plugin UI, and every property the plugin sets
//! and callback it listens for is really there.

use std::path::Path;
use std::sync::Arc;
use telescope_core::plugin::Plugin;
use telescope_core::system::Autostart;
use telescope_core::ui::SlintSource;
use telescope_system::startup::StartupPlugin;

fn spin_on<F: std::future::Future>(f: F) -> F::Output {
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    let mut f = std::pin::pin!(f);
    loop {
        if let std::task::Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
    }
}

/// The interpreter hands out a file's last component, so the one to look at goes to the end of it.
fn component_last(source: &str, component: &str) -> String {
    let start = source.find(&format!("export component {component} ")).expect("the component");
    let end = source[start + 1..].find("\nexport component ").map_or(source.len(), |i| start + 1 + i + 1);
    format!("{}{}{}", &source[..start], &source[end..], &source[start..end])
}

struct Never;

impl Autostart for Never {
    fn enabled(&self) -> bool {
        false
    }
    fn set_enabled(&self, _on: bool, _minimized: bool) -> Result<(), String> {
        Ok(())
    }
}

/// One test, because Slint's platform lives on the thread that first builds an instance.
#[test]
fn the_sections_have_what_the_plugin_uses() {
    let mut plugin = StartupPlugin::new(Arc::new(Never));
    let sections = plugin.settings_sections();
    assert_eq!(sections.len(), 2);
    let callbacks: [&[&str]; 2] = [&["open-toggled"], &["start-picked", "stop-picked", "delay-committed", "delay-step", "unit-picked", "notify-toggled"]];
    for (section, callbacks) in sections.iter().zip(callbacks) {
        let SlintSource::Text(source) = section.ui.source.clone() else { panic!("source text") };
        let source = component_last(&source, &section.ui.component);
        let mut compiler = slint_interpreter::Compiler::default();
        compiler.set_library_paths([(telescope_ui_kit::LIBRARY.to_string(), telescope_ui_kit::library_path())].into_iter().collect());
        let result = spin_on(compiler.build_from_source(source, Path::new("/root/startup.slint").to_path_buf()));
        let errors: Vec<String> = result.diagnostics().filter(|d| d.level() == slint_interpreter::DiagnosticLevel::Error).map(|d| d.to_string()).collect();
        assert!(errors.is_empty(), "{}", errors.join("\n"));
        let instance = result.component(&section.ui.component).expect("the component").create().expect("an instance");
        for name in section.ui.props.as_object().expect("props").keys() {
            assert!(instance.get_property(name).is_ok(), "{} has no property {name}", section.ui.component);
        }
        for name in callbacks {
            assert!(instance.set_callback(name, |_| slint_interpreter::Value::Void).is_ok(), "{} has no callback {name}", section.ui.component);
        }
    }
}
