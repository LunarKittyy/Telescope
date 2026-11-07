//! The Linux-only camera code (libgphoto2 and V4L2) is never named by code Windows builds: no Windows machine runs this, so the source is read.

use std::path::{Path, PathBuf};

/// Modules that exist on Linux only.
const LINUX_ONLY: [&str; 4] = ["gphoto_lib", "gphoto_stills", "webcam_capture::gphoto::", "webcam_capture::v4l2"];
/// How many lines above a use the `cfg(target_os = "linux")` that guards it can be.
const REACH: usize = 60;

fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn nothing_windows_builds_names_a_linux_only_camera_module_outside_a_cfg() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut files = Vec::new();
    sources(&root.join("crates/outputs/src"), &mut files);
    sources(&root.join("app/src"), &mut files);
    let mut bad = Vec::new();
    for file in files {
        let name = file.to_string_lossy().replace('\\', "/");
        // The Linux-only modules themselves, and tests, which only run where their modules exist
        if name.contains("/webcam_capture/") || name.ends_with("/tests.rs") || name.ends_with("_tests.rs") {
            continue;
        }
        let text = std::fs::read_to_string(&file).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            if !LINUX_ONLY.iter().any(|m| code.contains(m)) {
                continue;
            }
            let from = i.saturating_sub(REACH);
            if !lines[from..=i].iter().any(|l| l.contains("cfg(target_os = \"linux\")")) {
                bad.push(format!("{name}:{}: {}", i + 1, line.trim()));
            }
        }
    }
    assert!(bad.is_empty(), "named without a Linux cfg above:\n{}", bad.join("\n"));
}
