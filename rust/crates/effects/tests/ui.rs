//! The cards' .slint compile against ui-kit the way the shell builds plugin UI, and every property the plugin sets and
//! callback it listens for is really there.

use std::path::Path;
use telescope_core::plugin::Plugin;
use telescope_core::ui::{SlintSource, UiSpec};
use telescope_effects::background::BackgroundPlugin;
use telescope_effects::chroma_key::ChromaKeyPlugin;
use telescope_effects::cinema::CinemaPlugin;
use telescope_effects::colour_grading::ColourGradingPlugin;
use telescope_effects::composite_plugin::CompositePlugin;
use telescope_effects::desk::DeskPlugin;
use telescope_effects::name_tag::NameTagPlugin;
use telescope_effects::notes::NotesPlugin;
use telescope_effects::fun_pack::FunPackPlugin;
use telescope_effects::on_air_plugin::OnAirPlugin;
use telescope_effects::photos::PhotosPlugin;
use telescope_effects::privacy_blur::PrivacyBlurPlugin;
use telescope_effects::qr_reader::QrReaderPlugin;
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
    ui_has(&card.ui, callbacks);
}

/// The same for any piece of UI a plugin hands over (a header button, a settings section).
fn ui_has(ui: &UiSpec, callbacks: &[&str]) {
    let SlintSource::Text(source) = ui.source.clone() else { panic!("source text") };
    // A file with two exported components (a card and its settings) hands over only the last one, so ask for this one by name
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
    let instance = result.component(&ui.component).expect("the component").create().expect("an instance");
    for name in ui.props.as_object().expect("props").keys() {
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
    let mut photos = PhotosPlugin::new();
    compile(&mut photos);
    has_what_the_plugin_uses(&mut photos, &[
        "take-clicked", "open-clicked", "choice-picked", "quality-moved", "number-edited", "timer-picked", "best-toggled", "folder-clicked",
    ]);
    let mut composite = CompositePlugin::new();
    compile(&mut composite);
    has_what_the_plugin_uses(&mut composite, &["switch-toggled-by-hand", "layout-picked", "edit-clicked"]);
    let mut qr = QrReaderPlugin::new();
    compile(&mut qr);
    has_what_the_plugin_uses(&mut qr, &["switch-toggled-by-hand", "notify-toggled", "copy-clicked"]);
    let mut background = BackgroundPlugin::new();
    compile(&mut background);
    has_what_the_plugin_uses(&mut background, &["mode-picked", "blur-moved", "edges-moved", "picture-clicked", "download-clicked"]);
    let mut privacy = PrivacyBlurPlugin::new();
    compile(&mut privacy);
    has_what_the_plugin_uses(&mut privacy, &["switch-toggled-by-hand", "mode-picked", "now-clicked", "download-clicked"]);
    // Cinema tools has a card, and its colours and sliders in the Video tab
    let mut cinema = CinemaPlugin::new();
    compile(&mut cinema);
    has_what_the_plugin_uses(&mut cinema, &["tool-toggled", "guide-picked", "scope-picked"]);
    let section = cinema.settings_sections().remove(0);
    ui_has(&section.ui, &["colour-picked", "peaking-moved", "zebra-moved", "size-picked"]);
    // On Air has no card: a button in the header and its tab in Settings
    let mut on_air = OnAirPlugin::new(std::sync::Arc::new(telescope_core::testing::FakeWeb::default()), None);
    ui_has(&on_air.header_widget().expect("a header button"), &["clicked"]);
    let section = on_air.settings_sections().remove(0);
    ui_has(&section.ui, &[
        "trigger-picked", "page-toggled", "new-link-clicked", "address-picked", "copy-clicked", "url-edited", "method-picked", "on-edited",
        "off-edited", "test-clicked",
    ]);
}

#[test]
fn the_chroma_key_card_compiles() {
    compile(&mut ChromaKeyPlugin::new());
}

#[test]
fn the_fun_pack_card_compiles() {
    compile(&mut FunPackPlugin::new());
}
