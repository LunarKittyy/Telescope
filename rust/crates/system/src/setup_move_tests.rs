//! test_move_to_installed.py, test by test.

use super::*;
use crate::update_guard::{failed_build, read_journal, write_journal};
use serde_json::json;
use std::cell::RefCell;
use std::collections::BTreeMap;
use telescope_core::testing::scratch_dir;

fn write(path: &Path, data: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, data).unwrap();
}

/// A folder where the installer put Telescope.
fn installed(root: &Path) -> PathBuf {
    let folder = root.join("Programs").join("Telescope");
    write(&folder.join(EXE_NAME), b"exe");
    write(&folder.join("unins000.exe"), b"un");
    folder
}

/// Everything an unzipped copy has, and files of the user's own beside them.
fn unzipped(folder: &Path, extras: &[&str]) -> PathBuf {
    for name in ["TelescopeDesktop.exe", "THIRD_PARTY_NOTICES.txt", "Telescope.apk", "TelescopeDesktop.old.exe", ".update.json"] {
        write(&folder.join(name), b"x");
    }
    for name in ["lib-123/PyQt6/Qt6Core.dll", "lib-99/PyQt6/Qt6Core.dll", "lib-99/python313.dll", "platform-tools/adb.exe",
                 "platform-tools/AdbWinApi.dll", "platform-tools/NOTICE", "unitycapture/UnityCaptureFilter64.dll", "unitycapture/LICENSE",
                 "virtualcamera/TelescopeCamera.dll", ".update-staging/x.dll"] {
        write(&folder.join(name), b"x");
    }
    for name in extras {
        write(&folder.join(name), b"theirs");
    }
    folder.to_path_buf()
}

fn names(folder: &Path) -> Vec<String> {
    let mut found: Vec<String> = std::fs::read_dir(folder).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    found.sort();
    found
}

/// Every path under folder, relative and with forward slashes, sorted.
fn tree(folder: &Path) -> Vec<String> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            out.push(entry.path().strip_prefix(base).unwrap().to_string_lossy().replace('\\', "/"));
            if entry.file_type().unwrap().is_dir() {
                walk(base, &entry.path(), out);
            }
        }
    }
    let mut out = Vec::new();
    walk(folder, folder, &mut out);
    out.sort();
    out
}

/// A stand-in for the installer: its arguments, and how it ends.
struct Run {
    code: i32,
    fail: Option<fn() -> SetupRun>,
    calls: RefCell<Vec<Vec<String>>>,
}

impl Run {
    fn ok() -> Run {
        Run { code: 0, fail: None, calls: RefCell::new(Vec::new()) }
    }
    fn call(&self, argv: &[String]) -> SetupRun {
        self.calls.borrow_mut().push(argv.to_vec());
        match self.fail {
            Some(fail) => fail(),
            None => SetupRun::Exited(self.code),
        }
    }
}

fn install(run: &Run, setup: &Path, old: &Path, location: Option<PathBuf>, folders: &[PathBuf]) -> Result<InstallResult, UpdateError> {
    let (run_it, locate) = (|argv: &[String]| run.call(argv), || location.clone());
    install_with_setup(setup, old, &SetupEnv { run: &run_it, locate: &locate, folders })
}

/// Ops that hold back the named files for a number of tries, as an exiting copy does.
struct Held {
    counts: RefCell<std::collections::HashMap<String, u32>>,
    order: RefCell<Vec<String>>,
}

impl Held {
    fn new(held: &[(&str, u32)]) -> Held {
        Held { counts: RefCell::new(held.iter().map(|(n, c)| (n.to_string(), *c)).collect()), order: RefCell::new(Vec::new()) }
    }
    fn remove(&self, path: &Path) -> io::Result<()> {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        self.order.borrow_mut().push(name.clone());
        let mut counts = self.counts.borrow_mut();
        if counts.get(&name).copied().unwrap_or(0) > 0 {
            *counts.get_mut(&name).unwrap() -= 1;
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "in use"));
        }
        update_guard::remove(path)
    }
}

fn removing(old: &Path, current: &Path, tries: u32, held: &Held, folders: &[PathBuf], adb_dest: PathBuf) -> bool {
    let remove = |p: &Path| held.remove(p);
    remove_unzipped_copy_with(old, current, tries, Duration::ZERO, folders, &Ops { remove: &remove, ours: &ours, adb_dest })
}

fn plain(old: &Path, current: &Path, adb_dest: PathBuf) -> bool {
    removing(old, current, 20, &Held::new(&[]), &[], adb_dest)
}

fn link_to(path: &Path, exe: &str) -> PathBuf {
    let mut bytes = b"L\x00\x00\x00".to_vec();
    bytes.extend([1u8; 40]);
    bytes.extend(exe.encode_utf16().flat_map(u16::to_le_bytes));
    bytes.extend([0, 0]);
    write(path, &bytes);
    path.to_path_buf()
}

#[test]
fn the_setup_runs_silently_and_the_installed_copy_takes_over() {
    let root = scratch_dir("move");
    let (inst, old) = (installed(&root), root.join("Downloads").join("Telescope"));
    std::fs::create_dir_all(&old).unwrap();
    let run = Run::ok();
    let result = install(&run, &root.join("TelescopeSetup.exe"), &old, Some(inst.clone()), &[]).unwrap();
    assert_eq!(*run.calls.borrow(), [[root.join("TelescopeSetup.exe").to_string_lossy().into_owned(), "/VERYSILENT".into(), "/SUPPRESSMSGBOXES".into(),
                                      "/NORESTART".into(), "/SP-".into()]]);
    assert_eq!(result.relaunch, [inst.join(EXE_NAME).to_string_lossy().into_owned(), "--after-update".into(), "--moved-from".into(), old.to_string_lossy().into_owned()]);
}

#[test]
fn the_move_clears_the_installed_folder_s_old_journal_and_notes_the_old_copy() {
    let root = scratch_dir("move");
    let (inst, old) = (installed(&root), root.join("Downloads").join("Telescope"));
    std::fs::create_dir_all(&old).unwrap();
    write_journal(&inst, &json!({"platform": "windows", "to_build": 150, "state": "trial", "started": true})).unwrap();
    install(&Run::ok(), &root.join("s.exe"), &old, Some(inst.clone()), &[]).unwrap();
    assert_eq!(read_journal(&inst), None, "else its first start rolls the fresh install back");
    assert_eq!(pending_move(&inst), Some(old.clone()), "in case its first start doesn't get to tidy up");
    assert_eq!(failed_build(&old), None, "nothing has failed to start yet");
}

#[test]
fn once_the_installed_copy_is_started_the_old_one_holds_that_build_back() {
    let old = scratch_dir("move").join("Telescope");
    std::fs::create_dir_all(&old).unwrap();
    mark_started_elsewhere(200, &old);
    assert_eq!(failed_build(&old), Some(200), "it's only seen if the installed copy didn't start");
}

#[test]
fn a_pending_move_is_finished_at_a_later_start_and_then_forgotten() {
    let root = scratch_dir("move");
    let (inst, old) = (installed(&root), unzipped(&root.join("Downloads").join("Telescope"), &[]));
    std::fs::write(inst.join(PENDING_MOVE), old.to_string_lossy().as_bytes()).unwrap();
    let held = Held::new(&[("Telescope.apk", 1000)]);
    let finish = || {
        let remove = |p: &Path| held.remove(p);
        let ops = Ops { remove: &remove, ours: &ours, adb_dest: root.join("adb") };
        finish_move_with(&old, &inst, &[], 20, Duration::ZERO, &ops)
    };
    assert!(!finish());
    assert_eq!(pending_move(&inst), Some(old.clone()), "still there: the next start tries again");
    held.counts.borrow_mut().insert("Telescope.apk".into(), 0);
    assert!(finish());
    assert!(!old.exists() && pending_move(&inst).is_none());
}

#[test]
fn a_damaged_pending_move_note_is_ignored_and_removed() {
    for contents in [&b"C:\\Users\\\xc3"[..], b"\xff\xfe", b"", b"relative\\path", b"C:\\a\x00b"] {
        let inst = installed(&scratch_dir("move"));
        std::fs::write(inst.join(PENDING_MOVE), contents).unwrap(); // a cut-off write, say
        assert_eq!(pending_move(&inst), None, "{contents:?}");
        assert!(!inst.join(PENDING_MOVE).exists(), "not tried again at every start: {contents:?}");
    }
}

#[test]
fn the_pending_move_note_is_written_whole() {
    let root = scratch_dir("move");
    let (inst, old) = (installed(&root), root.join("Downloads").join("Telescope"));
    std::fs::create_dir_all(&old).unwrap();
    install(&Run::ok(), &root.join("s.exe"), &old, Some(inst.clone()), &[]).unwrap();
    let notes: Vec<String> = names(&inst).into_iter().filter(|n| n.starts_with(".moved-from")).collect();
    assert_eq!(notes, [".moved-from"]);
}

#[test]
fn a_pending_move_to_a_folder_that_s_gone_is_forgotten() {
    let root = scratch_dir("move");
    let inst = installed(&root);
    std::fs::write(inst.join(PENDING_MOVE), root.join("gone").to_string_lossy().as_bytes()).unwrap();
    assert!(!finish_move_with(&root.join("gone"), &inst, &[], 20, Duration::ZERO, &Ops { remove: &update_guard::remove, ours: &ours, adb_dest: root.join("adb") }));
    assert_eq!(pending_move(&inst), None);
}

#[test]
fn an_old_copy_s_bundled_adb_is_kept_for_the_installed_one() {
    let root = scratch_dir("move");
    let (old, kept) = (unzipped(&root.join("Telescope"), &[]), root.join("LocalAppData").join("Telescope").join("platform-tools"));
    assert!(plain(&old, &installed(&root), kept.clone()));
    assert_eq!(names(&kept), ["AdbWinApi.dll", "NOTICE", "adb.exe"]);
    assert!(!old.exists());
}

#[test]
fn a_downloaded_adb_isn_t_replaced_by_an_old_copy_s() {
    let root = scratch_dir("move");
    let kept = root.join("LocalAppData").join("Telescope").join("platform-tools");
    write(&kept.join("adb.exe"), b"newer");
    assert!(plain(&unzipped(&root.join("Telescope"), &[]), &installed(&root), kept.clone()));
    assert_eq!(std::fs::read(kept.join("adb.exe")).unwrap(), b"newer");
}

#[test]
fn a_user_s_own_platform_tools_isn_t_taken_as_telescope_s_adb() {
    let root = scratch_dir("move");
    let downloads = unzipped(&root.join("Downloads"), &[]);
    write(&downloads.join("platform-tools").join("fastboot.exe"), b"theirs");
    plain(&downloads, &installed(&root), root.join("LocalAppData").join("Telescope").join("platform-tools"));
    assert!(!root.join("LocalAppData").join("Telescope").exists());
}

#[test]
fn a_failed_install_leaves_this_copy_as_it_is() {
    let cases: [(Run, Option<&str>, &str); 4] = [
        (Run { code: 5, ..Run::ok() }, None, "stopped (code 5)"),
        (Run { fail: Some(|| SetupRun::TimedOut), ..Run::ok() }, None, "took too long"),
        (Run { fail: Some(|| SetupRun::CouldNotStart(io::Error::from_raw_os_error(if cfg!(windows) { 5 } else { 13 }))), ..Run::ok() }, None, "Couldn't start the installer"),
        (Run::ok(), Some("nowhere"), "wasn't where expected"),
    ];
    for (run, locate, says) in cases {
        let root = scratch_dir("move");
        let old = root.join("old");
        write(&old.join(EXE_NAME), b"exe");
        let location = if locate.is_some() { None } else { Some(installed(&root)) };
        let err = install(&run, &root.join("s.exe"), &old, location, &[]).unwrap_err();
        assert!(err.0.contains(says), "{says}: {}", err.0);
        assert!(!err.0.contains("os error"), "the system's words, as Python's strerror: {}", err.0);
        assert!(old.join(EXE_NAME).exists());
    }
}

#[test]
fn installed_over_this_same_folder_deletes_nothing() {
    let root = scratch_dir("move");
    let inst = installed(&root);
    let result = install(&Run::ok(), &root.join("s.exe"), &inst, Some(inst.clone()), &[]).unwrap();
    assert!(!result.relaunch.contains(&"--moved-from".to_string()));
}

#[test]
fn the_installed_location_comes_from_the_setup_s_uninstall_entry() {
    struct Reg {
        opened: RefCell<Vec<(String, String, String)>>,
        value: Option<&'static str>,
    }
    impl Uninstall for Reg {
        fn read(&self, hive: &str, key: &str, name: &str) -> Option<String> {
            self.opened.borrow_mut().push((hive.into(), key.into(), name.into()));
            self.value.map(String::from)
        }
        fn write_text(&self, _key: &str, _name: &str, _value: &str) -> io::Result<()> {
            unreachable!()
        }
        fn write_number(&self, _key: &str, _name: &str, _value: u32) -> io::Result<()> {
            unreachable!()
        }
    }
    let reg = Reg { opened: RefCell::new(Vec::new()), value: Some(r"C:\Users\l\AppData\Local\Programs\Telescope\") };
    assert!(installed_location_in(&reg).is_some());
    assert_eq!(*reg.opened.borrow(), [("HKCU".to_string(),
        r"Software\Microsoft\Windows\CurrentVersion\Uninstall\{BF097F77-F8B9-4F70-B23C-0B500DB1A07C}_is1".to_string(), "InstallLocation".to_string())]);
    assert_eq!(installed_location_in(&Reg { opened: RefCell::new(Vec::new()), value: None }), None);
    assert_eq!(installed_location_in(&Reg { opened: RefCell::new(Vec::new()), value: Some("") }), None);
}

/// The uninstall entry's values, as the fake winreg in test_updates.py keeps them.
#[derive(Default)]
struct Entry(RefCell<BTreeMap<String, String>>, RefCell<Vec<String>>);

impl Entry {
    fn with(values: &[(&str, &str)]) -> Entry {
        Entry(RefCell::new(values.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()), RefCell::default())
    }
    fn get(&self, name: &str) -> String {
        self.0.borrow().get(name).cloned().unwrap_or_default()
    }
}

impl Uninstall for Entry {
    fn read(&self, _hive: &str, _key: &str, name: &str) -> Option<String> {
        self.0.borrow().get(name).cloned()
    }
    fn write_text(&self, _key: &str, name: &str, value: &str) -> io::Result<()> {
        self.1.borrow_mut().push(name.into());
        self.0.borrow_mut().insert(name.into(), value.into());
        Ok(())
    }
    fn write_number(&self, key: &str, name: &str, value: u32) -> io::Result<()> {
        self.write_text(key, name, &value.to_string())
    }
}

// test_updates.py::test_note_installed_version_updates_the_apps_entry
#[test]
fn note_installed_version_updates_the_apps_entry() {
    let dir = scratch_dir("note-version");
    let reg = Entry::with(&[("InstallLocation", dir.to_str().unwrap()), ("DisplayVersion", "3.3.1"), ("MajorVersion", "3"), ("MinorVersion", "3")]);
    assert!(note_installed_version_in(&dir, "3.4.1", &reg));
    assert_eq!(reg.get("DisplayVersion"), "3.4.1");
    assert_eq!((reg.get("MajorVersion"), reg.get("MinorVersion")), ("3".to_string(), "4".to_string()));
    let writes = reg.1.borrow().len();
    assert!(!note_installed_version_in(&dir, "3.4.1", &reg), "already right");
    assert_eq!(reg.1.borrow().len(), writes, "nothing written");
    // A part that isn't all digits leaves its number as it was
    assert!(note_installed_version_in(&dir, "3.5rc1.0", &reg));
    assert_eq!((reg.get("MajorVersion"), reg.get("MinorVersion")), ("3".to_string(), "4".to_string()));
}

// test_updates.py::test_note_installed_version_leaves_another_copys_entry
#[test]
fn note_installed_version_leaves_another_copys_entry() {
    let root = scratch_dir("note-other");
    let installed = root.join("Programs/Telescope");
    std::fs::create_dir_all(&installed).unwrap();
    let reg = Entry::with(&[("InstallLocation", installed.to_str().unwrap()), ("DisplayVersion", "3.3.1")]);
    assert!(!note_installed_version_in(&root, "3.4.1", &reg));
    assert_eq!(reg.get("DisplayVersion"), "3.3.1");
    assert!(!note_installed_version_in(&root, "3.4.1", &Entry::default()), "not installed at all");
}

#[test]
fn the_app_id_matches_the_installer_script() {
    let iss = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../installer/Telescope.iss")).unwrap();
    // Inno writes AppId={{GUID}, the first brace escaped
    assert!(iss.contains(&format!("AppId={{{SETUP_APP_ID}\n")));
}

#[test]
fn the_installer_clears_a_trial_and_old_exe_a_crashed_migration_left() {
    let iss = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../installer/Telescope.iss")).unwrap();
    let section = iss.split("[InstallDelete]").nth(1).expect("an [InstallDelete] section").split("\n[").next().unwrap();
    for name in [update_guard::JOURNAL, ".update.json.tmp", update_guard::OLD_EXE_NAME] {
        assert!(section.contains(&format!("Name: \"{{app}}\\{name}\"")), "{name}");
    }
}

#[test]
fn the_unzipped_copy_goes_once_the_installed_one_runs() {
    let root = scratch_dir("move");
    let old = unzipped(&root.join("Telescope"), &[]);
    assert!(plain(&old, &installed(&root), root.join("adb")));
    assert!(!old.exists());
}

#[test]
fn a_zip_unpacked_into_downloads_only_loses_telescope_s_files() {
    let root = scratch_dir("move");
    let downloads = unzipped(&root.join("Downloads"), &["holiday.jpg", "Telescope-windows-portable.zip", "lib.txt"]);
    std::fs::create_dir(downloads.join("Games")).unwrap();
    assert!(plain(&downloads, &installed(&root), root.join("adb")));
    assert_eq!(names(&downloads), ["Games", "Telescope-windows-portable.zip", "holiday.jpg", "lib.txt"]);
}

#[test]
fn a_rust_build_s_files_go_too() {
    let root = scratch_dir("move");
    let downloads = unzipped(&root.join("Downloads"), &["holiday.jpg"]);
    for name in ["ui-kit/ui-kit.slint", "ui-kit/theme.slint", "ui-kit/icons.slint", "ui-kit/icons/x.svg", "ui-kit/widgets/card.slint",
                 "avcodec-62.dll", "avutil-60.dll", "swresample-6.dll", "swscale-9.dll", "rnnoise.dll"] {
        write(&downloads.join(name), b"x");
    }
    assert!(plain(&downloads, &installed(&root), root.join("adb")));
    assert_eq!(names(&downloads), ["holiday.jpg"]);
}

#[test]
fn folders_and_libraries_that_only_look_like_a_rust_build_s_stay() {
    let root = scratch_dir("move");
    let downloads = unzipped(&root.join("Downloads"), &["avcodec.dll", "avcodec-x.dll", "swscale-9.txt"]);
    write(&downloads.join("ui-kit/ui-kit.slint"), b"theirs");
    write(&downloads.join("ui-kit/mine.txt"), b"theirs");
    assert!(plain(&downloads, &installed(&root), root.join("adb")));
    assert_eq!(names(&downloads), ["avcodec-x.dll", "avcodec.dll", "swscale-9.txt", "ui-kit"]);
}

#[test]
fn the_user_s_own_folders_with_telescope_like_names_stay() {
    let root = scratch_dir("move");
    let downloads = unzipped(&root.join("Downloads"), &[]);
    for name in ["platform-tools/fastboot.exe", "lib-photos/cat.jpg", "lib-7/notes.txt", "unitycapture/mine.txt"] {
        write(&downloads.join(name), b"theirs");
    }
    assert!(plain(&downloads, &installed(&root), root.join("adb")));
    assert_eq!(tree(&downloads), [
        "lib-7", "lib-7/notes.txt", "lib-photos", "lib-photos/cat.jpg",
        "platform-tools", "platform-tools/AdbWinApi.dll", "platform-tools/NOTICE", "platform-tools/adb.exe", "platform-tools/fastboot.exe",
        "unitycapture", "unitycapture/LICENSE", "unitycapture/UnityCaptureFilter64.dll", "unitycapture/mine.txt"]);
}

#[test]
fn the_exe_goes_last_so_a_cut_short_clean_up_still_runs() {
    let root = scratch_dir("move");
    let old = unzipped(&root.join("Telescope"), &[]);
    let held = Held::new(&[("Telescope.apk", 1000)]);
    assert!(!removing(&old, &installed(&root), 2, &held, &[], root.join("adb")));
    assert!(!held.order.borrow().contains(&"TelescopeDesktop.exe".to_string()));
    assert!(old.join(EXE_NAME).exists());
    let held = Held::new(&[]);
    assert!(removing(&old, &installed(&root.join("x")), 20, &held, &[], root.join("adb")));
    assert_eq!(held.order.borrow().last().map(String::as_str), Some("TelescopeDesktop.exe"));
}

#[test]
fn a_folder_it_can_t_look_into_keeps_the_exe() {
    let root = scratch_dir("move");
    let old = unzipped(&root.join("Telescope"), &[]);
    let ours_but = |entry: &Path| {
        if entry.file_name().is_some_and(|n| n == "platform-tools") {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "Access is denied"));
        }
        ours(entry)
    };
    let ops = Ops { remove: &update_guard::remove, ours: &ours_but, adb_dest: root.join("adb") };
    assert!(!remove_unzipped_copy_with(&old, &installed(&root), 2, Duration::ZERO, &[], &ops));
    assert!(old.join(EXE_NAME).exists());
}

#[test]
fn it_refuses_anything_that_isnt_an_unzipped_telescope() {
    // The copy that's running; another installed copy, which its own uninstaller removes; a folder with no exe in it
    for make in 0..3 {
        let root = scratch_dir("move");
        let current = installed(&root);
        let target = match make {
            0 => current.clone(),
            1 => {
                let folder = unzipped(&root.join("Other"), &[]);
                write(&folder.join("unins000.exe"), b"u");
                folder
            }
            _ => {
                let folder = root.join("NotTelescope");
                write(&folder.join("notes.txt"), b"x");
                folder
            }
        };
        let before = tree(&target);
        assert!(!plain(&target, &current, root.join("adb")), "case {make}");
        assert_eq!(tree(&target), before, "case {make}");
    }
}

#[test]
fn files_still_held_by_the_exiting_copy_are_tried_again() {
    let root = scratch_dir("move");
    let old = unzipped(&root.join("Telescope"), &[]);
    let held = Held::new(&[("TelescopeDesktop.exe", 2)]);
    assert!(removing(&old, &installed(&root), 20, &held, &[], root.join("adb")));
    assert!(!old.exists());

    let old = unzipped(&root.join("Telescope2"), &[]);
    let held = Held::new(&[("TelescopeDesktop.exe", 99)]); // never let go
    assert!(!removing(&old, &installed(&root.join("x")), 3, &held, &[], root.join("adb")));
    assert!(old.join(EXE_NAME).exists());
}

#[test]
fn only_the_packaged_windows_app_counts_as_unzipped() {
    let root = scratch_dir("move");
    let nobody = |_: &Path| false;
    // A packaged copy has its exe in the folder
    write(&root.join(EXE_NAME), b"x");
    assert!(is_unzipped_copy(&Place::of_with(&root, true, None, &nobody)));
    write(&root.join("unins000.exe"), b"u");
    assert!(!is_unzipped_copy(&Place::of_with(&root, true, None, &nobody)));
    let local = root.join("AppData").join("Local");
    let setups_folder = local.join("Programs").join("Telescope");
    write(&setups_folder.join(EXE_NAME), b"x");
    // The setup would close this copy halfway through
    assert!(!is_unzipped_copy(&Place::of_with(&setups_folder, true, local.to_str(), &nobody)));
    // Run from source, nothing packaged
    assert!(!is_unzipped_copy(&Place::of_with(&root.join("nope"), true, None, &nobody)));
    // Not Windows
    let packaged = scratch_dir("move");
    write(&packaged.join(EXE_NAME), b"x");
    assert!(!is_unzipped_copy(&Place::of_with(&packaged, false, None, &nobody)));
}

#[test]
fn a_copy_on_a_usb_stick_or_a_share_stays_where_it_is() {
    let root = scratch_dir("move");
    write(&root.join(EXE_NAME), b"x");
    let on = |kind: u32| is_unzipped_copy(&Place::of_with(&root, true, None, &|_| carried_by("C:", &|_| kind)));
    assert!(on(3), "a fixed drive");
    assert!(!on(2) && !on(4), "removable, network");
    assert!(carried_by(r"\\server\share", &|_| 3), "a share by its name");
}

#[test]
fn a_desktop_shortcut_to_the_old_copy_gets_the_installed_one_s_in_its_place() {
    let root = scratch_dir("move");
    let desktop = root.join("Desktop");
    std::fs::create_dir_all(&desktop).unwrap();
    let (inst, old) = (installed(&root), unzipped(&root.join("Downloads").join("Telescope"), &[]));
    let dead = link_to(&desktop.join("TelescopeDesktop.exe - Shortcut.lnk"), &old.join(EXE_NAME).to_string_lossy().to_uppercase());
    let other = link_to(&desktop.join("Notes.lnk"), &root.join("notes.exe").to_string_lossy());
    let run = Run::ok();
    install(&run, &root.join("s.exe"), &old, Some(inst.clone()), std::slice::from_ref(&desktop)).unwrap();
    assert_eq!(run.calls.borrow()[0].last().unwrap(), "/MERGETASKS=desktopicon");
    assert!(removing(&old, &inst, 20, &Held::new(&[]), &[desktop], root.join("adb")));
    assert!(!dead.exists() && other.exists());
}

#[test]
fn no_desktop_shortcut_means_no_desktop_icon() {
    let root = scratch_dir("move");
    let (inst, old) = (installed(&root), root.join("Telescope"));
    std::fs::create_dir_all(&old).unwrap();
    let run = Run::ok();
    install(&run, &root.join("s.exe"), &old, Some(inst), std::slice::from_ref(&root)).unwrap();
    assert!(!run.calls.borrow()[0].contains(&"/MERGETASKS=desktopicon".to_string()));
}

#[test]
fn a_shortcut_stays_while_the_old_exe_does() {
    let root = scratch_dir("move");
    let desktop = root.join("Desktop");
    std::fs::create_dir_all(&desktop).unwrap();
    let old = unzipped(&root.join("Telescope"), &[]);
    let link = link_to(&desktop.join("Telescope.lnk"), &old.join(EXE_NAME).to_string_lossy());
    let held = Held::new(&[("TelescopeDesktop.exe", 1000)]);
    assert!(!removing(&old, &installed(&root), 2, &held, &[desktop], root.join("adb")));
    assert!(link.exists());
}

#[test]
fn shortcuts_are_found_whatever_the_case_and_encoding_of_the_path() {
    type Encode = fn(&str) -> Vec<u8>;
    let encoders: [Encode; 3] = [
        |t| t.encode_utf16().flat_map(u16::to_le_bytes).collect(),
        |t| std::iter::once(0).chain(t.encode_utf16().flat_map(u16::to_le_bytes)).collect(),
        |t| t.chars().map(|c| c as u32 as u8).collect(), // latin-1
    ];
    for encode in encoders {
        let root = scratch_dir("move");
        let exe = root.join("Åsa").join("Downloads").join("Telescope").join(EXE_NAME);
        let mut bytes = b"L\x00\x00\x00".to_vec();
        bytes.extend(encode(&exe.to_string_lossy().to_uppercase()));
        bytes.extend([0, 0]);
        write(&root.join("x.lnk"), &bytes);
        assert_eq!(shortcuts_to(&exe, std::slice::from_ref(&root)), [root.join("x.lnk")]);
        assert!(shortcuts_to(&root.join("Other").join(EXE_NAME), std::slice::from_ref(&root)).is_empty());
    }
}

#[test]
fn lib_folders_without_a_build_number_or_without_qt_stay() {
    let root = scratch_dir("move");
    let downloads = unzipped(&root.join("Downloads"), &[]);
    for name in ["lib-abc/PyQt6/x.dll", "lib-456/notes.txt"] {
        write(&downloads.join(name), b"theirs");
    }
    assert!(plain(&downloads, &installed(&root), root.join("adb")));
    assert_eq!(names(&downloads), ["lib-456", "lib-abc"]);
}

#[test]
fn the_setup_installing_over_the_unzipped_folder_notes_no_move() {
    let root = scratch_dir("move");
    let old = unzipped(&root.join("Telescope"), &[]);
    let result = install(&Run::ok(), &root.join("s.exe"), &old, Some(old.clone()), &[]).unwrap();
    assert_eq!(result.relaunch, [old.join(EXE_NAME).to_string_lossy().into_owned(), "--after-update".into()]);
    assert_eq!(pending_move(&old), None);
    assert!(!plain(&old, &old, root.join("adb")), "and it wouldn't delete itself either");
    assert!(old.join(EXE_NAME).exists());
}

#[test]
fn an_installed_folder_without_the_exe_is_an_error() {
    let root = scratch_dir("move");
    let folder = root.join("Programs").join("Telescope");
    std::fs::create_dir_all(&folder).unwrap();
    let old = root.join("old");
    std::fs::create_dir_all(&old).unwrap();
    let err = install(&Run::ok(), &root.join("s.exe"), &old, Some(folder.clone()), &[]).unwrap_err();
    assert!(err.0.contains("wasn't where expected"));
    assert!(!folder.join(PENDING_MOVE).exists());
}

#[cfg(unix)]
#[test]
fn symlinks_in_the_unzipped_copy_are_never_followed() {
    use std::os::unix::fs::symlink;
    let root = scratch_dir("move");
    let outside = root.join("Outside");
    write(&outside.join("PyQt6").join("keep.dll"), b"mine");
    write(&outside.join("adb.exe"), b"mine");
    write(&outside.join("telescope.log"), b"mine");
    let old = unzipped(&root.join("Telescope"), &[]);
    symlink(&outside, old.join("lib-456")).unwrap();
    symlink(&outside, old.join("platform-tools-link")).unwrap();
    std::fs::remove_file(old.join("THIRD_PARTY_NOTICES.txt")).unwrap();
    symlink(outside.join("adb.exe"), old.join("THIRD_PARTY_NOTICES.txt")).unwrap();
    symlink(outside.join("telescope.log"), old.join("telescope.log")).unwrap();
    assert!(plain(&old, &installed(&root), root.join("adb")));
    assert_eq!(tree(&outside), ["PyQt6", "PyQt6/keep.dll", "adb.exe", "telescope.log"]);
    assert_eq!(std::fs::read(outside.join("telescope.log")).unwrap(), b"mine", "the log link goes, not what it points at");
    assert_eq!(names(&old), ["THIRD_PARTY_NOTICES.txt", "lib-456", "platform-tools-link"]);
}

#[test]
fn the_adb_folder_matches_adb_download_py() {
    let home = Path::new("/home/someone");
    assert_eq!(downloaded_adb_dir(Some("C:/Users/l/AppData/Local"), home), Path::new("C:/Users/l/AppData/Local/Telescope/platform-tools"));
    assert_eq!(downloaded_adb_dir(None, home), home.join("AppData").join("Local").join("Telescope").join("platform-tools"));
    assert_eq!(downloaded_adb_dir(Some(""), home), downloaded_adb_dir(None, home));
}
