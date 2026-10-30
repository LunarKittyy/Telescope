//! What TELESCOPE_RS_REAL_CONFIG=1 switches to: the real app's own folders. Each place the Python app keeps something is asked of the
//! Python app and of this one in the same environment, and the answers have to be the same, on Linux and on Windows.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use telescope_core::config::{PathEnv, DEV_PROFILE_ENV};

/// The paths come from the environment on each call, and the tests here change it.
static ENV: Mutex<()> = Mutex::new(());

const SCRIPT: &str = r#"
import json, os, sys
from pathlib import Path
sys.path.insert(0, sys.argv[1])
platform = sys.argv[2]
home = sys.argv[3]
import update_guard
from telescope import config, diagnostics, plugin_loader, plugin_updates, single_instance, vision_pack
from telescope.platform import adb_download, virtual_mic
Path.home = classmethod(lambda cls: Path(home))
import tempfile
tempfile.tempdir = os.environ["TELESCOPE_TEST_TEMP"]
sys.platform = platform
paths = {
    "config": config.config_path(),
    "logs": diagnostics.log_dir(),
    "plugins": plugin_loader.plugins_dir(),
    "plugin_updates": plugin_updates.cache_path(),
    "control_key": single_instance.control_key_path(),
    "vision": vision_pack.default_dir(),
    "adb": adb_download.downloaded_dir(),
}
if platform != "win32":
    paths["mic_fifo"] = virtual_mic.fifo_path()
address = update_guard.instance_address()[1]
paths["instance"] = str(address[1]) if platform == "win32" else address[1:].decode()
print(json.dumps({k: str(v).replace(chr(92), "/") for k, v in paths.items()}))
"#;

fn desktop() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../desktop")
}

fn folder(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("telescope-realprofile-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn slashes(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// What Python answers for each place, with `platform` as sys.platform; None when there's no Python to ask.
fn python(platform: &str, home: &Path, temp: &Path, set: &[(&str, &Path)]) -> Option<serde_json::Value> {
    let mut cmd = Command::new("python3");
    cmd.args(["-I", "-c", SCRIPT]).arg(desktop()).arg(platform).arg(home);
    for name in ["XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_RUNTIME_DIR", "APPDATA", "LOCALAPPDATA", DEV_PROFILE_ENV] {
        cmd.env_remove(name);
    }
    cmd.env("QT_QPA_PLATFORM", "offscreen").env("TELESCOPE_TEST_TEMP", temp).env("HOME", home);
    for (name, value) in set {
        cmd.env(name, value);
    }
    let out = cmd.output().ok()?;
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    serde_json::from_slice(out.stdout.split(|b| *b == b'\n').rfind(|l| !l.is_empty())?).ok()
}

/// A Python-side answer for `key` against what this app says.
fn same(python: &serde_json::Value, key: &str, ours: &Path, what: &str) {
    assert_eq!(slashes(ours), python[key].as_str().unwrap(), "{key} ({what})");
}

#[test]
fn the_real_profile_keeps_everything_where_the_python_app_does_on_linux() {
    let _guard = ENV.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = folder("linux");
    let (home, temp) = (tmp.join("home"), tmp.join("tmp"));
    std::fs::create_dir_all(&temp).unwrap();
    for with_env in [true, false] {
        let (xdg_config, xdg_data, runtime) = (tmp.join("xdg-config"), tmp.join("xdg-data"), tmp.join("run"));
        let set: Vec<(&str, &Path)> = if with_env {
            vec![("XDG_CONFIG_HOME", &xdg_config), ("XDG_DATA_HOME", &xdg_data), ("XDG_RUNTIME_DIR", &runtime)]
        } else {
            Vec::new()
        };
        let Some(py) = python("linux", &home, &temp, &set) else {
            eprintln!("no python3 here, so the real profile's folders aren't compared with the Python app's");
            return;
        };
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
        same(&py, "config", &config, what);
        same(&py, "logs", &telescope_core::diagnostics::log_dir(), what);
        same(&py, "plugins", &telescope_plugin_host::package::plugins_dir(), what);
        same(&py, "plugin_updates", &telescope_plugin_host::updates::cache_path(), what);
        same(&py, "control_key", &telescope_system::single_instance::control_key_path(&config), what);
        same(&py, "vision", &telescope_vision::pack::default_dir(), what);
        same(&py, "adb", &telescope_link::adb::downloaded_dir(), what);
        let address = match telescope_system::single_instance::Address::current() {
            telescope_system::single_instance::Address::Abstract(name) => name,
            telescope_system::single_instance::Address::Port(port) => port.to_string(),
        };
        assert_eq!(address, py["instance"].as_str().unwrap(), "the single-instance address ({what})");
        // The private folder is made in the temp folder when there is no runtime folder: only its place is asked
        if with_env {
            same(&py, "mic_fifo", &telescope_audio::virtual_mic::fifo_path().unwrap(), what);
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
    for with_env in [true, false] {
        let set: Vec<(&str, &Path)> = if with_env { vec![("APPDATA", &appdata), ("LOCALAPPDATA", &local)] } else { Vec::new() };
        let Some(py) = python("win32", &home, &temp, &set) else {
            eprintln!("no python3 here, so the real profile's folders aren't compared with the Python app's");
            return;
        };
        let what = if with_env { "with %APPDATA%" } else { "without" };
        let env = PathEnv {
            windows: true,
            dev_profile: None,
            appdata: with_env.then(|| appdata.to_string_lossy().into_owned()),
            xdg_config_home: None,
            home: home.clone(),
        };
        same(&py, "config", &env.config_path(), what);
        let config = env.config_path();
        same(&py, "plugins", &config.parent().unwrap().join("plugins"), what);
        same(&py, "plugin_updates", &config.parent().unwrap().join("plugin-updates.json"), what);
        same(&py, "control_key", &telescope_system::single_instance::control_key_path(&config), what);
        same(&py, "logs", &telescope_core::diagnostics::log_dir_for(None, &temp, true, "luna"), what);
        assert_eq!(telescope_system::single_instance::INSTANCE_PORT.to_string(), py["instance"].as_str().unwrap(), "the single-instance port");
        let local_text = with_env.then(|| local.to_string_lossy().into_owned());
        same(&py, "adb", &telescope_system::setup_move::downloaded_adb_dir(local_text.as_deref(), &home), what);
    }
}
