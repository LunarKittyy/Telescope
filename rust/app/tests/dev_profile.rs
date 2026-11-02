//! test_dev_profile.py: a dev profile keeps everything apart from the real app. The launcher's own tests are in app/src/dev.rs.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use telescope_audio::virtual_mic;
use telescope_core::config::DEV_PROFILE_ENV;
use telescope_system::single_instance::Address;

/// The settings are read from the environment on each call, and every test here changes it.
static ENV: Mutex<()> = Mutex::new(());

#[derive(Debug)]
struct Probe {
    config: PathBuf,
    logs: PathBuf,
    instance: Address,
    real_instance: Address,
    mic_source: &'static str,
    mic_stale: Vec<u32>,
    fifo: PathBuf,
    pair_package: &'static str,
}

fn folder(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("telescope-devprofile-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// What the app would use with this profile (None: the real app's).
fn probe(tmp: &Path, profile: Option<&Path>) -> Probe {
    let _guard = ENV.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("XDG_CONFIG_HOME", tmp.join("xdg"));
    std::env::set_var("XDG_RUNTIME_DIR", tmp.join("run"));
    std::env::set_var("APPDATA", tmp.join("appdata"));
    match profile {
        Some(p) => std::env::set_var(DEV_PROFILE_ENV, p),
        None => std::env::remove_var(DEV_PROFILE_ENV),
    }
    // What the real app's mic left loaded, as pactl lists it
    let real_module = "12\tmodule-pipe-source\tsource_name=telescope_mic file=/run/telescope-mic.fifo";
    let first_build = "7\tmodule-null-sink\tsink_name=telescope_mic_sink";
    Probe {
        config: telescope_core::config::config_path(),
        logs: telescope_core::diagnostics::log_dir(),
        instance: Address::current(),
        real_instance: Address::for_profile(false),
        mic_source: virtual_mic::source(),
        mic_stale: virtual_mic::stale_modules(&format!("{real_module}\n{first_build}\n")),
        fifo: virtual_mic::fifo_path().unwrap(),
        pair_package: telescope_link::adb::pair_broadcast_package(),
    }
}

#[test]
fn a_dev_profile_keeps_everything_apart_from_the_real_app() {
    let tmp = folder("apart");
    let real = probe(&tmp, None);
    let profile = tmp.join("profile");
    let dev = probe(&tmp, Some(&profile));
    assert_eq!(dev.config.parent().unwrap(), profile);
    assert_eq!(dev.logs.parent().unwrap(), profile);
    assert_ne!(dev.instance, real.instance);
    assert_eq!(real.instance, real.real_instance);
    assert_ne!(dev.mic_source, real.mic_source);
    assert_ne!(dev.fifo, real.fifo);
    assert_eq!((dev.pair_package, real.pair_package), ("com.telescope.dev", "com.telescope"));
}

#[test]
fn setting_up_the_dev_mic_never_unloads_the_real_one() {
    let tmp = folder("mic");
    let dev = probe(&tmp, Some(&tmp.join("profile")));
    assert!(dev.mic_stale.is_empty(), "the real app's modules {:?} would be unloaded", dev.mic_stale);
    // And the real app's own run still finds what it left behind
    assert_eq!(probe(&tmp, None).mic_stale, [12, 7]);
}

#[cfg(not(windows))]
#[test]
fn without_a_profile_nothing_changes() {
    let tmp = folder("nothing");
    let real = probe(&tmp, None);
    assert_eq!(real.config, tmp.join("xdg").join("telescope").join("telescope_config.json"));
    assert!(real.fifo.to_string_lossy().ends_with("telescope-mic.fifo"));
    assert_eq!(real.mic_source, "telescope_mic");
}
