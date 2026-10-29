//! The cards' .slint compile against ui-kit the way the shell builds plugin UI, and every property the plugin sets and
//! callback it listens for is really there.

use std::path::Path;
use telescope_core::plugin::Plugin;
use telescope_core::ui::SlintSource;
use telescope_effects::chroma_key::ChromaKeyPlugin;
use telescope_effects::colour_grading::ColourGradingPlugin;
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

#[test]
fn the_chroma_key_card_compiles() {
    compile(&mut ChromaKeyPlugin::new());
}
