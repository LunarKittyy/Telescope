//! The Phone card and Setup's sections compile against ui-kit the way the shell builds plugin UI, and every property the plugins
//! set and callback they listen for is really there.

mod common;

use std::path::Path;
use telescope_core::plugin::Plugin;
use telescope_core::ui::{SlintSource, UiSpec};
use telescope_setup::monitoring::MonitoringPlugin;

fn spin_on<F: std::future::Future>(f: F) -> F::Output {
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    let mut f = std::pin::pin!(f);
    loop {
        if let std::task::Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
    }
}

thread_local!(static INIT: std::sync::Once = const { std::sync::Once::new() });

/// Builds a piece of UI's component on its own and checks it has every property set and every callback listened for.
fn ui_has(ui: &UiSpec, callbacks: &[&str]) {
    let SlintSource::Text(source) = ui.source.clone() else { panic!("source text") };
    let doc = format!("import {{ {c} }} from \"plugin/card.slint\";\nexport {{ {c} }}", c = ui.component);
    let mut compiler = slint_interpreter::Compiler::default();
    compiler.set_library_paths([(telescope_ui_kit::LIBRARY.to_string(), telescope_ui_kit::library_path())].into_iter().collect());
    compiler.set_file_loader(move |path: &Path| {
        let found = path.ends_with("plugin/card.slint").then(|| Ok(source.clone()));
        Box::pin(async move { found })
    });
    let result = spin_on(compiler.build_from_source(doc, Path::new("/root/card.slint").to_path_buf()));
    let errors: Vec<String> = result.diagnostics().filter(|d| d.level() == slint_interpreter::DiagnosticLevel::Error).map(|d| d.to_string()).collect();
    assert!(errors.is_empty(), "{}", errors.join("\n"));
    INIT.with(|once| once.call_once(i_slint_backend_testing::init_no_event_loop));
    let instance = result.component(&ui.component).expect("the component").create().expect("an instance");
    for name in ui.props.as_object().expect("props").keys() {
        assert!(instance.get_property(name).is_ok(), "{} has no property {name}", ui.component);
    }
    for name in callbacks {
        assert!(instance.set_callback(name, |_| slint_interpreter::Value::Void).is_ok(), "{} has no callback {name}", ui.component);
    }
}

/// One test, because Slint's platform lives on the thread that first builds an instance.
#[test]
fn the_cards_have_what_their_plugins_use() {
    let mut monitoring = MonitoringPlugin::new();
    ui_has(&monitoring.card().expect("a card").ui, &["batt-moved", "temp-moved", "alert-toggled"]);

    let mut stats = telescope_setup::stats::StatsPlugin::new();
    ui_has(&stats.card().expect("a card").ui, &["view-picked"]);

    let callbacks: [(&str, &[&str]); 6] = [
        ("virtual-camera", &["check-clicked", "load-clicked", "unload-clicked", "persist-toggled", "extras-clicked", "uc-clicked",
                             "adb-clicked", "own-adb-clicked", "copy"]),
        ("video", &["hw-toggled", "saving-toggled", "zoom-committed", "zoom-step", "canvas-picked", "custom-committed", "custom-step",
                    "apply-clicked", "copy"]),
        ("microphone", &["gain-picked", "limiter-toggled"]),
        ("phone-app", &["apk-clicked", "pick-done", "pick-cancelled"]),
        ("look", &["hues-picked", "lang-picked", "restart-clicked", "translate-clicked"]),
        ("about", &["author-clicked", "diag-clicked", "bug-clicked", "log-clicked", "help-clicked"]),
    ];
    for linux in [true, false] {
        let mut setup = common::setup_plugin().for_platform(linux);
        let sections = setup.settings_sections();
        assert_eq!(sections.len(), callbacks.len());
        for (section, (id, callbacks)) in sections.iter().zip(callbacks) {
            assert_eq!(section.id, id);
            ui_has(&section.ui, callbacks);
        }
    }
}
