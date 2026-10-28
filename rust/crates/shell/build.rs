fn main() {
    let kit = telescope_ui_kit::library_path();
    for f in telescope_ui_kit::slint_files() {
        println!("cargo:rerun-if-changed={}", f.display());
    }
    let config = slint_build::CompilerConfiguration::new()
        .with_library_paths([(telescope_ui_kit::LIBRARY.to_string(), kit)].into_iter().collect());
    slint_build::compile_with_config("ui/main.slint", config).unwrap();
}
