//! test_config.py, test by test.

use super::*;
use crate::testing::scratch_dir;
use std::cell::Cell;

fn file_in(dir: &Path) -> ConfigFile {
    ConfigFile::at(dir.join(CONFIG_FILENAME))
}

fn file_with(text: &[u8]) -> (PathBuf, ConfigFile) {
    let dir = scratch_dir("config");
    let f = file_in(&dir);
    fs::write(f.path(), text).unwrap();
    (dir, f)
}

fn backups(dir: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = fs::read_dir(dir).unwrap().flatten().map(|e| e.path())
        .filter(|p| p.file_name().unwrap().to_string_lossy().starts_with(&format!("{CONFIG_FILENAME}.invalid-"))).collect();
    found.sort();
    found
}

#[cfg(unix)]
fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn fresh_config_is_empty_current_version() {
    let dir = scratch_dir("config");
    assert_eq!(file_in(&dir).load(), json!({"version": 3, "selected_device": null, "plugin_configs": {}, "devices": {}}));
}

#[test]
fn save_and_reload_roundtrip() {
    let f = file_in(&scratch_dir("config"));
    let mut cfg = f.load();
    cfg["selected_device"] = json!("Phone1");
    assert!(f.save(&mut cfg));
    assert_eq!(f.load()["selected_device"], json!("Phone1"));
    assert!(f.path().exists());
}

#[test]
fn save_is_atomic_no_leftover_tmp_file() {
    let f = file_in(&scratch_dir("config"));
    let mut cfg = f.load();
    f.save(&mut cfg);
    assert!(!f.path().with_extension("json.tmp").exists());
}

#[cfg(unix)]
#[test]
fn saved_config_is_private_whatever_the_umask() {
    // SAFETY: umask is process-wide; nothing else in this crate's tests creates files while it is 022 except tests that only read modes back
    let old = unsafe { libc::umask(0o022) };
    let f = file_in(&scratch_dir("config").join("fresh"));
    let saved = f.save(&mut empty());
    unsafe { libc::umask(old) };
    assert!(saved);
    assert_eq!(mode(f.path()), 0o600);
    assert_eq!(mode(f.path().parent().unwrap()), 0o700);
}

#[cfg(unix)]
#[test]
fn saving_tightens_a_config_left_readable_by_others() {
    use std::os::unix::fs::PermissionsExt;
    let dir = scratch_dir("config");
    let f = file_in(&dir);
    fs::write(f.path(), "{}").unwrap();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
    fs::set_permissions(f.path(), fs::Permissions::from_mode(0o644)).unwrap();
    assert!(f.save(&mut f.load()));
    assert_eq!(mode(f.path()), 0o600);
    assert_eq!(mode(&dir), 0o700);
}

#[cfg(unix)]
#[test]
fn a_backup_of_a_bad_config_is_private() {
    let (dir, f) = file_with(b"not valid json");
    f.load();
    let copies = backups(&dir);
    assert_eq!(copies.len(), 1);
    assert_eq!(mode(&copies[0]), 0o600);
}

#[test]
fn a_filesystem_that_refuses_chmod_still_saves() {
    let f = file_in(&scratch_dir("config"));
    let refuse = |_: &Path, _: u32| -> io::Result<()> { Err(io::Error::new(io::ErrorKind::PermissionDenied, "no Unix permissions here")) };
    let text = json::dumps(&empty(), Style::CONFIG);
    f.write_with(text.as_bytes(), &refuse).unwrap();
    assert_eq!(f.load(), empty());
}

#[test]
fn malformed_json_falls_back_to_empty() {
    let (_, f) = file_with(b"not valid json");
    assert_eq!(f.load()["version"], json!(3));
}

#[test]
fn malformed_json_is_backed_up_before_reset() {
    let (dir, f) = file_with(b"not valid json");
    f.load();
    let copies = backups(&dir);
    assert_eq!(copies.len(), 1);
    assert_eq!(fs::read(&copies[0]).unwrap(), b"not valid json");
}

#[test]
fn a_config_that_is_not_utf8_is_backed_up_not_fatal() {
    // "Café" in Latin-1
    let raw = b"{\"version\": 3, \"selected_device\": \"Caf\xe9\"}";
    let (dir, f) = file_with(raw);
    assert_eq!(f.load()["selected_device"], Value::Null);
    assert_eq!(fs::read(&backups(&dir)[0]).unwrap(), raw);
}

#[test]
fn a_config_saved_with_a_byte_order_mark_still_loads() {
    let (_, f) = file_with(b"\xef\xbb\xbf{\"version\": 3, \"selected_device\": \"Pixel\"}");
    assert_eq!(f.load()["selected_device"], json!("Pixel"));
}

#[test]
fn stale_version_config_is_backed_up_before_reset() {
    let original = br#"{"version": 1, "selected_device": "Old"}"#;
    let (dir, f) = file_with(original);
    assert_eq!(f.load(), empty());
    let copies = backups(&dir);
    assert_eq!(copies.len(), 1);
    assert_eq!(fs::read(&copies[0]).unwrap(), original);
}

#[test]
fn fresh_install_is_not_backed_up() {
    // No file exists yet: nothing to preserve, and no folder is made just by looking
    let dir = scratch_dir("config").join("never-made");
    file_in(&dir).load();
    assert!(!dir.exists());
}

#[test]
fn invalid_top_level_config_shapes_fall_back_to_empty() {
    for raw in [json!([]), json!("text"), json!(42), json!({"version": "two"})] {
        assert_eq!(migrate(raw.clone()), empty(), "{raw}");
    }
}

#[test]
fn current_version_config_keeps_custom_keys_and_fills_in_missing_sections() {
    let result = migrate(json!({"version": 3, "custom": true}));
    assert_eq!(result["custom"], json!(true));
    assert_eq!(result["plugin_configs"], json!({}));
    assert_eq!(result["devices"], json!({}));
    assert_eq!(result["selected_device"], Value::Null);
}

#[test]
fn future_version_config_is_preserved() {
    let future = json!({
        "version": 99, "custom": true,
        "plugin_configs": {"connection": {"mode": "wifi"}},
        "devices": {"Phone": {"active_ip": "1.2.3.4"}},
        "selected_device": "Phone",
    });
    assert_eq!(migrate(future.clone()), future);
}

fn env_for(windows: bool, set: bool) -> PathEnv {
    PathEnv {
        windows,
        dev_profile: None,
        appdata: set.then(|| "/appdata".into()),
        xdg_config_home: set.then(|| "/xdg".into()),
        home: PathBuf::from("/home/someone"),
    }
}

#[test]
fn linux_config_path_uses_xdg_or_home_fallback() {
    for xdg_set in [true, false] {
        let path = env_for(false, xdg_set).config_path();
        assert_eq!(path.file_name().unwrap(), "telescope_config.json");
        assert!(path.parent().unwrap().to_string_lossy().to_lowercase().contains("telescope"));
        let base = if xdg_set { "/xdg" } else { "/home/someone/.config" };
        assert_eq!(path, Path::new(base).join("telescope").join("telescope_config.json"));
    }
}

#[test]
fn windows_config_path_uses_appdata() {
    let path = env_for(true, true).config_path();
    assert_eq!(path.file_name().unwrap(), "telescope_config.json");
    assert_eq!(path, Path::new("/appdata").join("Telescope").join("telescope_config.json"));
}

#[test]
fn config_older_than_current_version_resets_to_empty() {
    for version in [0, 1] {
        let old = json!({
            "version": version,
            "selected_device": "Phone",
            "plugin_configs": {"connection": {"devices_list": [{"name": "Phone", "ip": "1.2.3.4"}]}},
        });
        assert_eq!(migrate(old), empty(), "version {version}");
    }
}

#[test]
fn config_missing_version_resets_to_empty() {
    assert_eq!(migrate(json!({"selected_device": "Phone"})), empty());
}

#[test]
fn malformed_legacy_device_entry_does_not_crash_and_resets_to_empty() {
    let (_, f) = file_with(br#"{"version": 1, "plugin_configs": {"connection": {"devices_list": [{}]}}}"#);
    assert_eq!(f.load(), empty());
}

#[test]
fn malformed_plugin_configs_section_resets_alone() {
    let result = migrate(json!({
        "version": 3,
        "plugin_configs": ["not", "a", "dict"],
        "devices": {"Phone": {"active_ip": "1.2.3.4"}},
        "selected_device": "Phone",
    }));
    assert_eq!(result["plugin_configs"], json!({}));
    assert_eq!(result["devices"], json!({"Phone": {"active_ip": "1.2.3.4"}}));
    assert_eq!(result["selected_device"], json!("Phone"));
}

#[test]
fn malformed_devices_section_resets_alone() {
    let result = migrate(json!({
        "version": 3,
        "plugin_configs": {"connection": {"mode": "wifi"}},
        "devices": {"Phone": {"active_ip": 12345}},
        "selected_device": "Phone",
    }));
    assert_eq!(result["plugin_configs"], json!({"connection": {"mode": "wifi"}}));
    assert_eq!(result["devices"], json!({}));
    assert_eq!(result["selected_device"], json!("Phone"));
}

#[test]
fn devices_section_with_non_dict_entry_resets_alone() {
    assert_eq!(migrate(json!({"version": 3, "devices": {"Phone": "not-a-dict"}}))["devices"], json!({}));
}

#[test]
fn malformed_selected_device_resets_alone() {
    let result = migrate(json!({
        "version": 3,
        "plugin_configs": {"connection": {"mode": "wifi"}},
        "devices": {},
        "selected_device": 42,
    }));
    assert_eq!(result["selected_device"], Value::Null);
    assert_eq!(result["plugin_configs"], json!({"connection": {"mode": "wifi"}}));
}

#[test]
fn valid_current_version_config_round_trips_through_migrate() {
    let cfg = json!({
        "version": 3,
        "plugin_configs": {"connection": {"mode": "usb"}},
        "devices": {"Phone": {"active_ip": "10.0.0.1", "plugin_configs": {"transforms": {"zoom": 2}}}},
        "selected_device": "Phone",
    });
    assert_eq!(migrate(cfg.clone()), cfg);
}

#[test]
fn save_failure_returns_false_and_sets_version() {
    let dir = scratch_dir("config");
    let f = file_in(&dir);
    // A folder where the file goes: the rename over it fails, as os.replace does in the Python test
    fs::create_dir(f.path()).unwrap();
    let mut cfg = json!({"version": 0});
    assert!(!f.save(&mut cfg));
    assert_eq!(cfg["version"], json!(CONFIG_VERSION));
}

#[test]
fn v2_config_keeps_global_settings_but_drops_pairing_and_per_phone_settings() {
    let (_, f) = file_with(br#"{
        "version": 2,
        "selected_device": "Pixel",
        "plugin_configs": {"connection": {"devices": [{"name": "Pixel"}]}, "setup": {"canvas": [1920, 1080]}},
        "devices": {"Pixel": {"plugin_configs": {"camera_control": {"iso": 100}}}}
    }"#);
    let cfg = f.load();
    assert_eq!(cfg["version"], json!(3));
    assert_eq!(cfg["plugin_configs"], json!({"setup": {"canvas": [1920, 1080]}}));
    assert_eq!(cfg["devices"], json!({}));
    assert_eq!(cfg["selected_device"], Value::Null);
}

#[test]
fn a_briefly_locked_config_is_read_again_not_reset() {
    let f = file_in(&scratch_dir("config"));
    let mut cfg = f.load();
    cfg["selected_device"] = json!("Phone1");
    f.save(&mut cfg);
    let calls = Cell::new(0);
    let flaky = |path: &Path| {
        calls.set(calls.get() + 1);
        if calls.get() == 1 {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "locked"));
        }
        fs::read(path)
    };
    let loaded = f.read_with(Mode::Lenient, &flaky, &|_| {}).unwrap();
    assert_eq!(loaded["selected_device"], json!("Phone1"));
    assert_eq!(calls.get(), 2);
}

#[test]
fn a_config_that_stays_locked_at_start_is_never_saved_over_and_is_taken_in_once_readable() {
    let f = file_in(&scratch_dir("config"));
    let mut cfg = f.load();
    cfg["selected_device"] = json!("Phone1");
    f.save(&mut cfg);
    let locked = |_: &Path| Err(io::Error::new(io::ErrorKind::PermissionDenied, "locked"));
    assert_eq!(f.read_with(Mode::Start, &locked, &|_| {}).unwrap(), empty());
    // Readable again, but what the app holds is defaults: every save that reads first refuses
    assert!(f.load_strict().is_err());
    assert!(!f.update(|c| c["selected_device"] = Value::Null));
    assert_eq!(f.load()["selected_device"], json!("Phone1"));
    assert_eq!(f.reread_after_unread_start().unwrap().unwrap()["selected_device"], json!("Phone1"));
    assert!(f.reread_after_unread_start().is_none());
    assert!(f.load_strict().is_ok());
}

#[test]
fn a_config_nested_too_deep_is_backed_up_not_fatal() {
    let text = format!("{}{}", "[".repeat(100_000), "]".repeat(100_000));
    let (dir, f) = file_with(text.as_bytes());
    assert_eq!(f.load(), empty());
    assert_eq!(backups(&dir).len(), 1);
}

#[test]
fn loading_a_bad_config_again_before_saving_backs_it_up_once() {
    let (dir, mut f) = file_with(b"not valid json");
    thread_local!(static N: Cell<u32> = const { Cell::new(0) });
    // A different stamp each time, so a second copy would show as a second file
    f.clock = || N.with(|n| { n.set(n.get() + 1); format!("20250101T00000{}", n.get()) });
    for _ in 0..3 {
        f.load();
    }
    assert_eq!(backups(&dir).len(), 1);
}

#[test]
fn a_bad_config_whose_backup_failed_is_never_saved_over() {
    let (dir, mut f) = file_with(b"not valid json");
    f.clock = || "20250101T000000".to_string();
    // A folder where the copy should go, so the copy can't be written
    let blocker = dir.join(format!("{CONFIG_FILENAME}.invalid-20250101T000000"));
    fs::create_dir(&blocker).unwrap();
    assert!(f.load_strict().is_err());
    assert!(!f.update(|c| c["selected_device"] = json!("Phone1")));
    assert_eq!(fs::read(f.path()).unwrap(), b"not valid json");
    // The start holds defaults and leaves saving closed, even though the file reads fine again
    assert_eq!(f.load_at_start(), empty());
    assert!(!f.update(|c| c["selected_device"] = json!("Phone1")));
    assert_eq!(fs::read(f.path()).unwrap(), b"not valid json");
    // Once the copy can be made it is, and saving opens
    fs::remove_dir(&blocker).unwrap();
    assert_eq!(f.reread_after_unread_start().unwrap().unwrap(), empty());
    assert_eq!(fs::read(&blocker).unwrap(), b"not valid json");
    assert!(f.update(|c| c["selected_device"] = json!("Phone1")));
}

#[test]
fn a_reset_is_reported_once_with_where_the_copy_went() {
    let (_, f) = file_with(b"");
    assert_eq!(f.take_reset_notice(), None);
    fs::write(f.path(), "not valid json").unwrap();
    f.load();
    f.load();
    let notice = f.take_reset_notice().unwrap();
    assert!(notice.contains(".invalid-"), "{notice}");
    assert_eq!(f.take_reset_notice(), None);
}

#[test]
fn the_config_path_is_where_the_python_app_kept_it_on_linux_and_windows() {
    // What the Python app's config_path() gave for each of these, with sys.platform and the environment set to match
    let cases = [
        (false, false, false, "/home/someone/.config/telescope/telescope_config.json"),
        (false, true, false, "/xdg/telescope/telescope_config.json"),
        (true, false, false, "/home/someone/Telescope/telescope_config.json"),
        (true, true, false, "/appdata/Roaming/Telescope/telescope_config.json"),
        (false, true, true, "/tmp/dev-profile/telescope_config.json"),
        (true, true, true, "/tmp/dev-profile/telescope_config.json"),
    ];
    for (windows, env_set, dev, expected) in cases {
        let want = PathEnv {
            windows,
            dev_profile: dev.then(|| PathBuf::from("/tmp/dev-profile")),
            appdata: env_set.then(|| "/appdata/Roaming".to_string()),
            xdg_config_home: env_set.then(|| "/xdg".to_string()),
            home: PathBuf::from("/home/someone"),
        };
        assert_eq!(want.config_path().to_string_lossy().replace('\\', "/"), expected, "windows {windows}, env {env_set}, dev {dev}");
    }
}
