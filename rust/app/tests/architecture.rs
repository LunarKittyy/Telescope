//! Crates only meet through core: no crate but the binary depends on another Phase 1 crate (the Rust side of test_architecture.py).

use std::path::PathBuf;

/// Which telescope crates each crate may use; anything else has to go through a trait in core.
const ALLOWED: &[(&str, &[&str])] = &[
    ("core", &[]),
    ("ui-kit", &[]),
    ("link", &["telescope-core"]),
    ("media", &["telescope-core"]),
    ("outputs", &["telescope-core"]),
    ("audio", &["telescope-core"]),
    ("system", &["telescope-core"]),
    ("plugin-host", &["telescope-core", "telescope-ui-kit"]),
    ("shell", &["telescope-core", "telescope-ui-kit"]),
];

fn telescope_deps(manifest: &str) -> Vec<String> {
    let mut in_deps = false;
    let mut deps = Vec::new();
    for line in manifest.lines().map(str::trim) {
        if line.starts_with('[') {
            in_deps = line.ends_with("dependencies]");
        } else if in_deps && line.starts_with("telescope-") {
            deps.push(line.split(['.', ' ', '=']).next().unwrap().to_string());
        }
    }
    deps
}

#[test]
fn crates_only_meet_through_core() {
    let crates = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../crates");
    let mut found: Vec<String> = std::fs::read_dir(&crates).unwrap().flatten()
        .filter(|e| e.path().join("Cargo.toml").exists())
        .map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    found.sort();
    for name in &found {
        let allowed = ALLOWED.iter().find(|(n, _)| n == name).map(|(_, a)| *a)
            .unwrap_or_else(|| panic!("crates/{name} isn't in ALLOWED: say which crates it may use"));
        let manifest = std::fs::read_to_string(crates.join(name).join("Cargo.toml")).unwrap();
        for dep in telescope_deps(&manifest) {
            assert!(allowed.contains(&dep.as_str()), "crates/{name} depends on {dep}; go through a trait in core instead");
        }
    }
}
