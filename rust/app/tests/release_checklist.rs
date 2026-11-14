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

// A file deleted since the last release is not checklist work, so it must not be reported as unmapped.
#[test]
fn deleted_files_are_not_reported_as_unmapped() {
    let repo = Path::new(env!("CARGO_TARGET_TMPDIR")).join("checklist-due-deleted");
    let _ = std::fs::remove_dir_all(&repo);
    std::fs::create_dir_all(repo.join(".github")).unwrap();
    std::fs::create_dir_all(repo.join("docs")).unwrap();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    std::fs::copy(root.join(".github/checklist_due.py"), repo.join(".github/checklist_due.py")).unwrap();
    std::fs::copy(root.join("docs/release-checklist.md"), repo.join("docs/release-checklist.md")).unwrap();
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .arg("-C").arg(&repo)
            .args(["-c", "user.name=test", "-c", "user.email=test@example.invalid", "-c", "commit.gpgsign=false"])
            .args(args)
            .output()
            .expect("git runs");
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    };
    git(&["init", "-q"]);
    std::fs::write(repo.join("scratch.txt"), "x").unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-qm", "base"]);
    git(&["tag", "v1"]);
    git(&["rm", "-q", "scratch.txt"]);
    git(&["commit", "-qm", "remove"]);
    let out = Command::new("python3").arg(repo.join(".github/checklist_due.py")).arg("v1").output().expect("python3 runs");
    assert!(out.status.success(), "{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
}
