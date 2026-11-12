//! Compiling a generated document with slint-interpreter. Plugins' .slint text, the shell's own frame and the kit come
//! through one file loader, which also puts every text in the person's language first (source::translate).

use crate::reach::{normalise, Reach};
use crate::source::translate;
use slint_interpreter::{Compiler, ComponentDefinition};
use std::collections::HashMap;
use std::io;
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
    // A plugin may use Slint's own widgets, which need the style's std-widgets-impl and can't have the kit's menus in its place: that
    // window keeps Slint's menus. A file only the loader reads shows it by failing, or by a panic for the widgets that are built in
    let stock = doc.contains("std-widgets") || known.values().any(|text| text.contains("std-widgets"));
    if !stock {
        let with_menus = std::panic::AssertUnwindSafe(|| build(name, doc, known.clone(), component, true));
        if let Ok(Ok(def)) = std::panic::catch_unwind(with_menus) {
            return Ok(def);
        }
    }
    build(name, doc, known, component, false)
}

fn build(name: &str, doc: &str, known: Rc<HashMap<PathBuf, String>>, component: &str, kit_menus: bool) -> Result<ComponentDefinition, String> {
    let mut compiler = Compiler::default();
    compiler.set_library_paths([(telescope_ui_kit::LIBRARY.to_string(), telescope_ui_kit::library_path())].into_iter().collect());
    if kit_menus {
        // The kit's menus, in place of the style's (ui/menus/std-widgets-impl.slint)
        compiler.set_include_paths(vec![telescope_ui_kit::library_path().join("menus")]);
    }
    // Slint reads whatever an import names when this loader says nothing, so every refusal is an answer (Some(Err))
    let reach = Reach::new(&telescope_ui_kit::library_path(), telescope_ui_kit::LIBRARY, doc, Path::new(ROOT));
    compiler.set_file_loader(move |path: &Path| {
        let found = load(&known, &reach, path);
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

/// A file for the compiler: the shell's own, the kit's, or an added plugin's (only its own folder and what its files import from it).
/// `known` is already translated.
fn load(known: &HashMap<PathBuf, String>, reach: &Reach, path: &Path) -> Option<io::Result<String>> {
    let missing = || io::Error::new(io::ErrorKind::NotFound, "no such file");
    let slint_file = |path: &Path| path.extension().is_some_and(|e| e == "slint").then(|| std::fs::read_to_string(path).ok()).flatten();
    let path = &normalise(path);
    if path.starts_with(ROOT) {
        return Some(known.get(path).cloned().ok_or_else(missing));
    }
    if reach.in_kit(path) {
        // A real file of the kit: read it here so its texts get translated too
        return Some(slint_file(path).map(|text| translate(&text)).ok_or_else(missing));
    }
    let Some(root) = reach.plugin_folder(path) else {
        return Some(Err(io::Error::new(io::ErrorKind::PermissionDenied, format!("{} isn't a file a plugin may use", path.display()))));
    };
    let Some(text) = known.get(path).cloned().or_else(|| slint_file(path).map(|text| translate(&text))) else { return Some(Err(missing())) };
    Some(reach.vet(path, &root, &text).map(|()| text).map_err(|why| io::Error::new(io::ErrorKind::PermissionDenied, why)))
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

#[cfg(test)]
mod tests {
    use super::*;
    use telescope_core::testing::scratch_dir;

    /// Two plugin folders, as a plugin's card file would see them.
    fn folders() -> (PathBuf, PathBuf) {
        let root = scratch_dir("compile-reach");
        let (mine, other) = (root.join("mine"), root.join("other"));
        for dir in [&mine, &other] {
            std::fs::create_dir_all(dir.join("sub")).unwrap();
            std::fs::write(dir.join("plugin.json"), "{}").unwrap();
            std::fs::write(dir.join("helper.slint"), "export component Helper inherits Rectangle { }\n").unwrap();
            std::fs::write(dir.join("sub/image.png"), "").unwrap();
        }
        (mine, other)
    }

    /// Compiles `body` as the card file of the plugin in `mine`, the way the popout window does: the document only imports it.
    fn card(mine: &Path, body: &str) -> Result<(), String> {
        let file = mine.join("card.slint");
        std::fs::write(&file, body).unwrap();
        let doc = format!("export {{ Probe }} from {:?};\n", file.to_string_lossy());
        compile("probe.slint", &doc, Vec::new(), "Probe").map(drop)
    }

    #[test]
    fn a_plugins_files_reach_its_own_folder_and_the_kit() {
        let (mine, _) = folders();
        let own = "import { Helper } from \"helper.slint\";\nimport { Theme } from \"@ui-kit/ui-kit.slint\";\n\
                   export component Probe inherits Rectangle { Helper { } Image { source: @image-url(\"sub/image.png\"); } }\n";
        card(&mine, own).unwrap();
    }

    #[test]
    fn a_plugins_files_cant_import_another_plugins_or_any_other_file() {
        let (mine, other) = folders();
        let other_helper = other.join("helper.slint");
        for bad in [format!("import {{ Helper }} from \"../other/helper.slint\";\nexport component Probe inherits Rectangle {{ Helper {{ }} }}\n"),
                    format!("import {{ Helper }} from {:?};\nexport component Probe inherits Rectangle {{ Helper {{ }} }}\n", other_helper.to_string_lossy()),
                    "import { Helper } from \"/etc/helper.slint\";\nexport component Probe inherits Rectangle { Helper { } }\n".to_string()] {
            let error = card(&mine, &bad).unwrap_err();
            assert!(error.contains("outside the plugin") || error.contains("isn't a file a plugin may use"), "{bad}: {error}");
        }
    }

    #[test]
    fn a_plugins_images_stay_in_its_folder_too() {
        let (mine, _) = folders();
        for path in ["../other/sub/image.png", "/etc/hostname"] {
            let body = format!("export component Probe inherits Rectangle {{ Image {{ source: @image-url({path:?}); }} }}\n");
            let error = card(&mine, &body).unwrap_err();
            assert!(error.contains("outside the plugin"), "{path}: {error}");
        }
    }

    #[test]
    fn a_file_imported_by_a_plugins_file_is_checked_as_well() {
        let (mine, _) = folders();
        std::fs::write(mine.join("sub/inner.slint"), "export component Inner inherits Rectangle { Image { source: @image-url(\"../../other/sub/image.png\"); } }\n").unwrap();
        let body = "import { Inner } from \"sub/inner.slint\";\nexport component Probe inherits Rectangle { Inner { } }\n";
        let error = card(&mine, body).unwrap_err();
        assert!(error.contains("outside the plugin"), "{error}");
    }
}
