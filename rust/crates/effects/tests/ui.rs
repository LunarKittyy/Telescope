//! The cards' .slint compile against ui-kit the way the shell builds plugin UI, and every property the plugin sets and
//! callback it listens for is really there.

use std::path::Path;
use telescope_core::plugin::Plugin;
use telescope_core::ui::SlintSource;
use telescope_effects::colour_grading::ColourGradingPlugin;
use telescope_effects::desk::DeskPlugin;
use telescope_effects::name_tag::NameTagPlugin;
use telescope_effects::notes::NotesPlugin;
use telescope_effects::transforms::TransformsPlugin;

fn spin_on<F: std::future::Future>(f: F) -> F::Output {
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    let mut f = std::pin::pin!(f);
    loop {
        if let std::task::Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
    }
}

fn compile(plugin: &mut dyn Plugin) -> slint_interpreter::ComponentDefinition {
    let card = plugin.card().expect("a card");
    let SlintSource::Text(source) = card.ui.source.clone() else { panic!("source text") };
    let doc = format!(
        "import {{ {c} }} from \"plugin/card.slint\";\nexport component Wrapper inherits Window {{ VerticalLayout {{ {c} {{ }} }} }}",
        c = card.ui.component
    );
    let mut compiler = slint_interpreter::Compiler::default();
    compiler.set_library_paths([(telescope_ui_kit::LIBRARY.to_string(), telescope_ui_kit::library_path())].into_iter().collect());
    compiler.set_file_loader(move |path: &Path| {
        let found = path.ends_with("plugin/card.slint").then(|| Ok(source.clone()));
        Box::pin(async move { found })
    });
    let result = spin_on(compiler.build_from_source(doc, Path::new("/root/wrapper.slint").to_path_buf()));
    let errors: Vec<String> = result.diagnostics().filter(|d| d.level() == slint_interpreter::DiagnosticLevel::Error).map(|d| d.to_string()).collect();
    assert!(errors.is_empty(), "{}", errors.join("\n"));
    result.component("Wrapper").expect("the wrapper")
}

#[test]
fn the_colour_grading_card_compiles() {
    compile(&mut ColourGradingPlugin::new());
}

#[test]
fn the_transforms_card_compiles() {
    compile(&mut TransformsPlugin::new());
}

/// Builds the card's own component and checks it has every property the plugin sets and every callback it listens for.
fn has_what_the_plugin_uses(plugin: &mut dyn Plugin, callbacks: &[&str]) {
    let card = plugin.card().expect("a card");
    let SlintSource::Text(source) = card.ui.source.clone() else { panic!("source text") };
    let mut compiler = slint_interpreter::Compiler::default();
    compiler.set_library_paths([(telescope_ui_kit::LIBRARY.to_string(), telescope_ui_kit::library_path())].into_iter().collect());
    let result = spin_on(compiler.build_from_source(source, Path::new("/root/card.slint").to_path_buf()));
    let errors: Vec<String> = result.diagnostics().filter(|d| d.level() == slint_interpreter::DiagnosticLevel::Error).map(|d| d.to_string()).collect();
    assert!(errors.is_empty(), "{}", errors.join("\n"));
    let instance = result.component(&card.ui.component).expect("the component").create().expect("an instance");
    for name in card.ui.props.as_object().expect("props").keys() {
        assert!(instance.get_property(name).is_ok(), "the card has no property {name}");
    }
    for name in callbacks {
        assert!(instance.set_callback(name, |_| slint_interpreter::Value::Void).is_ok(), "the card has no callback {name}");
    }
}

/// One test, because Slint's platform lives on the thread that first builds an instance.
#[test]
fn the_cards_have_what_their_plugins_use() {
    let mut notes = NotesPlugin::new();
    compile(&mut notes);
    has_what_the_plugin_uses(&mut notes, &[
        "set-picked", "new-clicked", "delete-clicked", "text-edited", "size-moved", "scroll-picked", "speed-moved", "back-clicked",
        "play-toggled", "next-clicked", "send-clicked", "clear-clicked",
    ]);
    let mut tag = NameTagPlugin::new();
    compile(&mut tag);
    has_what_the_plugin_uses(&mut tag, &["switch-toggled-by-hand", "text-edited", "choice-picked"]);
    let mut desk = DeskPlugin::new();
    compile(&mut desk);
    has_what_the_plugin_uses(&mut desk, &[
        "switch-toggled-by-hand", "mark-clicked", "done-clicked", "cancel-clicked", "reset-clicked", "clean-picked", "shape-picked", "stretch-moved",
    ]);
}
