//! Puts the translations in the program: every <lang>.po and the template from the desktop app's locale folder, as bytes the language
//! loader reads (OUT_DIR/locale.rs). One file to ship and update, with no folder next to it to lose.

use std::path::{Path, PathBuf};

/// Where the catalogs live in the repository.
const LOCALE: &str = "../../desktop/telescope/locale";

fn main() {
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("cargo sets it"));
    let folder = manifest.join(LOCALE);
    println!("cargo:rerun-if-changed={}", folder.display());
    println!("cargo:rerun-if-changed=build.rs");
    let include = |path: &Path| format!("include_bytes!({:?})", path.to_string_lossy().replace('\\', "/"));
    let mut catalogs: Vec<(String, PathBuf)> = std::fs::read_dir(&folder)
        .map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "po")).filter_map(|p| Some((p.file_stem()?.to_string_lossy().into_owned(), p))).collect())
        .unwrap_or_default();
    catalogs.sort();
    let template = folder.join("telescope.pot");
    if catalogs.is_empty() || !template.is_file() {
        println!("cargo:warning=no translations found in {}: the app will only be in English", folder.display());
    }
    let mut out = String::new();
    if template.is_file() {
        out += &format!("pub static TEMPLATE: &[u8] = {};\n", include(&template));
    } else {
        out += "pub static TEMPLATE: &[u8] = &[];\n";
    }
    out += "pub static CATALOGS: &[(&str, &[u8])] = &[\n";
    for (code, path) in &catalogs {
        out += &format!("    ({code:?}, {}),\n", include(path));
    }
    out += "];\n";
    let target = PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets it")).join("locale.rs");
    std::fs::write(target, out).expect("write locale.rs");
}
