//! TELESCOPE_RS_REAL_CONFIG=1 uses the real folders: each one is where the Python app kept it, so updating finds everyone's files.
//! The places were recorded from the Python app's own functions, with sys.platform and the environment set to match.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use telescope_core::config::{PathEnv, DEV_PROFILE_ENV};

/// The paths come from the environment on each call, and the tests here change it.
static ENV: Mutex<()> = Mutex::new(());

fn folder(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("telescope-realprofile-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn slashes(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// What the Python app answered for `key`, with `{tmp}` standing for the test's own folder, against what this app says.
fn same(expected: &[(&str, &str)], key: &str, tmp: &Path, ours: &Path, what: &str) {
    let want = expected.iter().find(|(k, _)| *k == key).unwrap_or_else(|| panic!("no {key} recorded")).1;
    assert_eq!(slashes(ours), want.replace("{tmp}", &slashes(tmp)), "{key} ({what})");
}

#[cfg(not(windows))] // Python on Windows has no os.getuid to stand in for Linux with; the other way round works
#[test]
fn the_real_profile_keeps_everything_where_the_python_app_does_on_linux() {
    let _guard = ENV.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = folder("linux");
    let (home, temp) = (tmp.join("home"), tmp.join("tmp"));
    std::fs::create_dir_all(&temp).unwrap();
    // The single-instance address and the private folders carry the user id
    let uid = unsafe { libc::getuid() }.to_string();
    let with_xdg = [
        ("config", "{tmp}/xdg-config/telescope/telescope_config.json"),
        ("plugins", "{tmp}/xdg-config/telescope/plugins"),
        ("plugin_updates", "{tmp}/xdg-config/telescope/plugin-updates.json"),
        ("control_key", "{tmp}/xdg-config/telescope/.control-key"),
        ("vision", "{tmp}/xdg-data/telescope/vision"),
        ("adb", "{tmp}/home/AppData/Local/Telescope/platform-tools"),
        ("mic_fifo", "{tmp}/run/telescope-mic.fifo"),
    ];
    let without = [
        ("config", "{tmp}/home/.config/telescope/telescope_config.json"),
        ("plugins", "{tmp}/home/.config/telescope/plugins"),
        ("plugin_updates", "{tmp}/home/.config/telescope/plugin-updates.json"),
        ("control_key", "{tmp}/home/.config/telescope/.control-key"),
        ("vision", "{tmp}/home/.local/share/telescope/vision"),
        ("adb", "{tmp}/home/AppData/Local/Telescope/platform-tools"),
    ];
    for with_env in [true, false] {
        let (xdg_config, xdg_data, runtime) = (tmp.join("xdg-config"), tmp.join("xdg-data"), tmp.join("run"));
        let set: Vec<(&str, &Path)> = if with_env {
            vec![("XDG_CONFIG_HOME", &xdg_config), ("XDG_DATA_HOME", &xdg_data), ("XDG_RUNTIME_DIR", &runtime)]
        } else {
            Vec::new()
        };
        let expected: &[(&str, &str)] = if with_env { &with_xdg } else { &without };
        for name in ["XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_RUNTIME_DIR", "APPDATA", "LOCALAPPDATA", DEV_PROFILE_ENV] {
            std::env::remove_var(name);
        }
        for (name, value) in &set {
            std::env::set_var(name, value);
        }
        std::env::set_var("HOME", &home);
        std::env::set_var("TMPDIR", &temp);
        let what = if with_env { "with XDG folders" } else { "without" };
        let config = telescope_core::config::config_path();
        same(expected, "config", &tmp, &config, what);
        // The log folder is named for the user, which the test can't choose: only its place is compared
        let logs = telescope_core::diagnostics::log_dir();
        assert_eq!(logs.parent().unwrap(), temp, "logs ({what})");
        assert!(logs.file_name().unwrap().to_string_lossy().starts_with("telescope-"), "logs ({what})");
        same(expected, "plugins", &tmp, &telescope_plugin_host::package::plugins_dir(), what);
        same(expected, "plugin_updates", &tmp, &telescope_plugin_host::updates::cache_path(), what);
        same(expected, "control_key", &tmp, &telescope_system::single_instance::control_key_path(&config), what);
        same(expected, "vision", &tmp, &telescope_vision::pack::default_dir(), what);
        same(expected, "adb", &tmp, &telescope_link::adb::downloaded_dir(), what);
        let address = match telescope_system::single_instance::Address::current() {
            telescope_system::single_instance::Address::Abstract(name) => name,
            telescope_system::single_instance::Address::Port(port) => port.to_string(),
        };
        assert_eq!(address, format!("telescope-{uid}"), "the single-instance address ({what})");
        // The private folder is made in the temp folder when there is no runtime folder: only its place is asked
        if with_env {
            same(expected, "mic_fifo", &tmp, &telescope_audio::virtual_mic::fifo_path().unwrap(), what);
        }
    }
}

#[test]
fn the_real_profile_keeps_everything_where_the_python_app_does_on_windows() {
    let _guard = ENV.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = folder("windows");
    let (home, temp) = (tmp.join("home"), tmp.join("tmp"));
    std::fs::create_dir_all(&temp).unwrap();
    let (appdata, local) = (tmp.join("AppData").join("Roaming"), tmp.join("AppData").join("Local"));
    let with_appdata = [
        ("config", "{tmp}/AppData/Roaming/Telescope/telescope_config.json"),
        ("logs", "{tmp}/tmp/Telescope"),
        ("plugins", "{tmp}/AppData/Roaming/Telescope/plugins"),
        ("plugin_updates", "{tmp}/AppData/Roaming/Telescope/plugin-updates.json"),
        ("control_key", "{tmp}/AppData/Roaming/Telescope/.control-key"),
        ("adb", "{tmp}/AppData/Local/Telescope/platform-tools"),
    ];
    let without = [
        ("config", "{tmp}/home/Telescope/telescope_config.json"),
        ("logs", "{tmp}/tmp/Telescope"),
        ("plugins", "{tmp}/home/Telescope/plugins"),
        ("plugin_updates", "{tmp}/home/Telescope/plugin-updates.json"),
        ("control_key", "{tmp}/home/Telescope/.control-key"),
        ("adb", "{tmp}/home/AppData/Local/Telescope/platform-tools"),
    ];
    for with_env in [true, false] {
        let expected: &[(&str, &str)] = if with_env { &with_appdata } else { &without };
        let what = if with_env { "with %APPDATA%" } else { "without" };
        let env = PathEnv {
            windows: true,
            dev_profile: None,
            appdata: with_env.then(|| appdata.to_string_lossy().into_owned()),
            xdg_config_home: None,
            home: home.clone(),
        };
        same(expected, "config", &tmp, &env.config_path(), what);
        let config = env.config_path();
        same(expected, "plugins", &tmp, &config.parent().unwrap().join("plugins"), what);
        same(expected, "plugin_updates", &tmp, &config.parent().unwrap().join("plugin-updates.json"), what);
        same(expected, "control_key", &tmp, &telescope_system::single_instance::control_key_path(&config), what);
        same(expected, "logs", &tmp, &telescope_core::diagnostics::log_dir_for(None, &temp, true, "luna"), what);
        assert_eq!(telescope_system::single_instance::INSTANCE_PORT, 47823, "the single-instance port");
        let local_text = with_env.then(|| local.to_string_lossy().into_owned());
        same(expected, "adb", &tmp, &telescope_system::setup_move::downloaded_adb_dir(local_text.as_deref(), &home), what);
    }
}
