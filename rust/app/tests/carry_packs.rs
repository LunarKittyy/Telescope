// .github/carry_packs.py: the nightly release keeps the last packs, so an older nightly still finds the one it asks for.
use std::path::Path;
use std::process::Command;

fn run(old: &Path, out: &Path, keep: &str) {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.github/carry_packs.py");
    let done = ["python3", "python"].iter().find_map(|python| {
        Command::new(python).arg(&script).arg("--old").arg(old).arg("--out").arg(out).args(["--keep", keep]).output().ok()
    }).expect("the script is Python");
    assert!(done.status.success(), "{}", String::from_utf8_lossy(&done.stderr));
}

fn folder(name: &str, files: &[&str]) -> std::path::PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for f in files {
        std::fs::write(dir.join(f), f).unwrap();
    }
    dir
}

fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
    v.sort();
    v
}

#[test]
fn the_previous_pack_stays_next_to_the_new_one() {
    let old = folder("carry-old-1", &["Telescope-vision-linux-aaaa.zip", "Telescope-vision-windows-aaaa.zip", "Telescope-matting-linux-cccc.zip"]);
    let out = folder("carry-out-1", &["Telescope-vision-linux-bbbb.zip", "Telescope-vision-windows-bbbb.zip", "Telescope-matting-linux-cccc.zip", "Telescope.apk"]);
    run(&old, &out, "3");
    assert_eq!(names(&out), [
        "Telescope-matting-linux-cccc.zip", "Telescope-vision-linux-aaaa.zip", "Telescope-vision-linux-bbbb.zip",
        "Telescope-vision-windows-aaaa.zip", "Telescope-vision-windows-bbbb.zip", "Telescope.apk", "pack-history.json",
    ]);
}

#[test]
fn only_the_newest_few_are_kept_and_the_first_nightly_has_nothing_to_carry() {
    let old = folder("carry-old-2", &["pack-history.json", "Telescope-vision-linux-3333.zip", "Telescope-vision-linux-2222.zip", "Telescope-vision-linux-1111.zip"]);
    std::fs::write(old.join("pack-history.json"), r#"["Telescope-vision-linux-3333.zip","Telescope-vision-linux-2222.zip","Telescope-vision-linux-1111.zip"]"#).unwrap();
    let out = folder("carry-out-2", &["Telescope-vision-linux-4444.zip"]);
    run(&old, &out, "3");
    let kept = names(&out);
    assert!(kept.contains(&"Telescope-vision-linux-3333.zip".to_string()) && kept.contains(&"Telescope-vision-linux-2222.zip".to_string()));
    assert!(!kept.contains(&"Telescope-vision-linux-1111.zip".to_string()), "{kept:?}");
    let fresh = folder("carry-out-3", &["Telescope-vision-linux-4444.zip"]);
    run(&Path::new(env!("CARGO_TARGET_TMPDIR")).join("carry-missing"), &fresh, "3");
    assert_eq!(names(&fresh), ["Telescope-vision-linux-4444.zip", "pack-history.json"]);
}
