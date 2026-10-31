// test_release_checklist.py: .github/checklist_due.py's map of files to checklist sections names every tracked file, rust/ included.
use std::path::Path;
use std::process::Command;

// test_release_checklist.py::test_every_file_is_in_a_checklist_section_or_listed_as_shared_or_ignored
#[test]
fn every_file_is_in_a_checklist_section_or_listed_as_shared_or_ignored() {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.github/checklist_due.py");
    let out = ["python3", "python"].iter().find_map(|python| Command::new(python).arg(&script).arg("--check-map").output().ok())
        .expect("the map check is a Python script");
    assert!(out.status.success(), "{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
}
