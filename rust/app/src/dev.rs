//! `telescope-rs dev`: this checkout as a dev desktop client next to the real Telescope (desktop/scripts/dev_desktop.py).
//!
//!   telescope-rs dev [--keep] [--reset] [--plugin FOLDER ...] [options for the app]
//!
//! It runs the app in a profile of its own, so it has its own config (and so its own pairings and computer identity), log and mic, no
//! menu or sign-in entry, and USB pairing aimed at the dev phone app (com.telescope.dev, from `./gradlew installDev`). Without
//! --keep the profile is fresh and deleted when the app quits; with it the same one is used every time (--reset deletes it). Each
//! --plugin FOLDER is loaded as it is, always on, and the app starts again whenever one of its files is saved.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use telescope_core::dev_profile::{PLUGINS_ENV, RESTART_EXIT, RESTART_STREAMING, RESUME_ENV, SUPERVISED_ENV};

/// What the launcher sets for the app, over the environment it was started with. A missing RESUME is taken out of it.
pub type Env = BTreeMap<String, String>;

/// Where the launcher keeps its folders, and for whom.
pub struct Launch {
    pub temp: PathBuf,
    pub user: String,
}

impl Launch {
    pub fn system() -> Launch {
        let user = std::env::var("USER").or_else(|_| std::env::var("USERNAME")).unwrap_or_default();
        Launch { temp: std::env::temp_dir(), user }
    }

    /// The profile --keep reuses.
    pub fn kept_folder(&self) -> PathBuf {
        self.temp.join(format!("telescope-dev-profile-{}", self.user))
    }
}

/// (the --plugin folders, the other arguments); the Err says what's wrong with a folder.
pub fn take_plugins(argv: &[String]) -> Result<(Vec<PathBuf>, Vec<String>), String> {
    let (mut folders, mut rest) = (Vec::new(), Vec::new());
    let mut words = argv.iter();
    while let Some(word) = words.next() {
        if word != "--plugin" && !word.starts_with("--plugin=") {
            rest.push(word.clone());
            continue;
        }
        let value = match word.split_once('=') {
            Some((_, v)) => v.to_string(),
            None => words.next().cloned().unwrap_or_default(),
        };
        let folder = expand_home(&value);
        let folder = std::fs::canonicalize(&folder).unwrap_or(folder);
        if value.is_empty() || !folder.join("plugin.json").is_file() {
            return Err(format!("--plugin needs a plugin's folder (the one with plugin.json in it), not '{value}'"));
        }
        folders.push(folder);
    }
    Ok((folders, rest))
}

fn expand_home(path: &str) -> PathBuf {
    match path.strip_prefix("~/").or(path.strip_prefix("~").filter(|r| r.is_empty())) {
        Some(rest) => std::env::home_dir().unwrap_or_default().join(rest),
        None => PathBuf::from(path),
    }
}

/// The launcher itself. `run_app` runs the app once with these arguments and settings and gives its exit code.
pub fn main(launch: &Launch, argv: &[String], run_app: &mut dyn FnMut(&[String], &Env) -> i32) -> Result<i32, String> {
    let (keep, reset) = (argv.iter().any(|a| a == "--keep"), argv.iter().any(|a| a == "--reset"));
    let others: Vec<String> = argv.iter().filter(|a| *a != "--keep" && *a != "--reset").cloned().collect();
    let (plugins, rest) = take_plugins(&others)?;
    if reset {
        let _ = std::fs::remove_dir_all(launch.kept_folder());
        println!("Removed {}", launch.kept_folder().display());
        if !keep {
            return Ok(0);
        }
    }
    let folder = if keep {
        let folder = launch.kept_folder();
        private_folder(&folder).map_err(|e| format!("Couldn't make {}: {e}", folder.display()))?;
        folder
    } else {
        fresh_folder(&launch.temp).map_err(|e| format!("Couldn't make a profile folder: {e}"))?
    };
    println!("Dev profile: {}", folder.display());
    let joined = std::env::join_paths(&plugins).map(|p| p.to_string_lossy().into_owned()).map_err(|e| e.to_string())?;
    let mut env = Env::from([
        (telescope_core::dev_profile::ENV.to_string(), folder.to_string_lossy().into_owned()),
        (SUPERVISED_ENV.to_string(), "1".to_string()),
        (PLUGINS_ENV.to_string(), joined),
    ]);
    for plugin in &plugins {
        println!("Plugin: {} (Telescope restarts when you save one of its files)", plugin.display());
    }
    let mut args = rest.clone();
    let code = loop {
        let code = run_app(&args, &env);
        if code != RESTART_EXIT && code != RESTART_STREAMING {
            break code;
        }
        env.insert(RESUME_ENV.to_string(), if code == RESTART_STREAMING { "1" } else { "" }.to_string());
        args = rest.iter().filter(|a| *a != "--minimized").cloned().chain(["--restarted".to_string()]).collect();
        println!("Restarting Telescope");
    };
    if !keep {
        let _ = std::fs::remove_dir_all(&folder);
    }
    Ok(code)
}

fn private_folder(folder: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(folder)
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(folder)
}

fn fresh_folder(temp: &Path) -> std::io::Result<PathBuf> {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let folder = temp.join(format!("telescope-dev-{}-{nanos}", std::process::id()));
    private_folder(&folder)?;
    Ok(folder)
}

/// What the launcher needs of the app it started.
pub trait Child {
    /// The exit code once it has exited.
    fn poll(&mut self) -> Option<i32>;
    fn terminate(&mut self);
    fn kill(&mut self);
}

/// Waits for the app to quit, up to `stopped` being set (Ctrl+C or SIGTERM). Either way no copy is left behind: a still-running app is
/// asked to stop, and killed if it hasn't after `grace`. None when it was stopped from here.
pub fn wait_for(child: &mut dyn Child, stopped: &dyn Fn() -> bool, grace: Duration, pause: Duration) -> Option<i32> {
    let result = loop {
        if let Some(code) = child.poll() {
            break Some(code);
        }
        if stopped() {
            break None;
        }
        std::thread::sleep(pause);
    };
    if child.poll().is_none() {
        child.terminate();
        let until = Instant::now() + grace;
        while child.poll().is_none() {
            if Instant::now() >= until {
                child.kill();
                break;
            }
            std::thread::sleep(pause);
        }
    }
    result
}

struct Process(std::process::Child);

impl Child for Process {
    fn poll(&mut self) -> Option<i32> {
        // A copy killed by a signal has no code; the shell's 128 + signal is the closest thing
        self.0.try_wait().ok().flatten().map(|s| s.code().unwrap_or(1))
    }
    fn terminate(&mut self) {
        #[cfg(unix)]
        // SAFETY: asking a process we started to stop; a pid that has gone just fails
        unsafe {
            libc::kill(self.0.id() as i32, libc::SIGTERM);
        }
        #[cfg(not(unix))]
        let _ = self.0.kill();
    }
    fn kill(&mut self) {
        let _ = self.0.kill();
    }
}

static STOPPED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(unix)]
extern "C" fn on_signal(_: libc::c_int) {
    STOPPED.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// Runs this program as the app with `args`, until it quits; stops it too if the launcher is stopped.
fn run_app(args: &[String], env: &Env) -> i32 {
    let Ok(exe) = std::env::current_exe() else { return 1 };
    let mut command = std::process::Command::new(exe);
    command.args(args).env_remove(RESUME_ENV).envs(env);
    let Ok(child) = command.spawn() else { return 1 };
    let mut child = Process(child);
    let stopped = || STOPPED.load(std::sync::atomic::Ordering::SeqCst);
    // The app's own exit code, or 130 for Ctrl+C and 143 for SIGTERM, the shell's way of saying so
    wait_for(&mut child, &stopped, Duration::from_secs(10), Duration::from_millis(100)).unwrap_or(130)
}

/// `telescope-rs dev ...`: the exit code.
pub fn run(argv: &[String]) -> i32 {
    #[cfg(unix)]
    // SAFETY: the handler only sets a flag
    unsafe {
        libc::signal(libc::SIGINT, on_signal as *const () as usize);
        libc::signal(libc::SIGTERM, on_signal as *const () as usize);
    }
    match main(&Launch::system(), argv, &mut run_app) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use telescope_core::testing::scratch_dir;

    fn words(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_launcher_runs_the_app_in_a_profile_and_throws_a_fresh_one_away() {
        let launch = Launch { temp: scratch_dir("launcher"), user: "luna".into() };
        let seen: RefCell<Vec<(Vec<String>, PathBuf)>> = RefCell::new(Vec::new());
        let mut run = |args: &[String], env: &Env| {
            let folder = PathBuf::from(&env[telescope_core::dev_profile::ENV]);
            std::fs::write(folder.join("telescope_config.json"), "{}").unwrap();
            seen.borrow_mut().push((args.to_vec(), folder));
            0
        };
        assert_eq!(main(&launch, &words(&["-platform", "offscreen"]), &mut run), Ok(0));
        let (args, folder) = seen.borrow()[0].clone();
        assert_eq!(args, words(&["-platform", "offscreen"]));
        assert!(!folder.exists());

        assert_eq!(main(&launch, &words(&["--keep"]), &mut run), Ok(0));
        let kept = seen.borrow()[1].1.clone();
        assert!(kept == launch.kept_folder() && kept.join("telescope_config.json").exists());
        assert_eq!(main(&launch, &words(&["--keep"]), &mut run), Ok(0));
        assert_eq!(seen.borrow()[2].1, kept);
        assert_eq!(main(&launch, &words(&["--reset"]), &mut run), Ok(0));
        assert!(!kept.exists());
    }

    #[test]
    fn the_launcher_loads_plugins_being_written_and_starts_the_app_again_when_asked() {
        let launch = Launch { temp: scratch_dir("launcher-plugins"), user: "luna".into() };
        let plugin = launch.temp.join("my_plugin");
        std::fs::create_dir(&plugin).unwrap();
        std::fs::write(plugin.join("plugin.json"), "{}").unwrap();
        let plugin = std::fs::canonicalize(plugin).unwrap();
        let runs: RefCell<Vec<(Vec<String>, String, Option<String>, String)>> = RefCell::new(Vec::new());
        let codes = RefCell::new(vec![76, 75, 3].into_iter());
        let mut run = |args: &[String], env: &Env| {
            runs.borrow_mut().push((args.to_vec(), env[PLUGINS_ENV].clone(), env.get(RESUME_ENV).cloned(), env[SUPERVISED_ENV].clone()));
            codes.borrow_mut().next().unwrap()
        };
        let argv = words(&["--plugin", plugin.to_str().unwrap(), "--minimized"]);
        assert_eq!(main(&launch, &argv, &mut run), Ok(3));
        let p = plugin.to_string_lossy().into_owned();
        assert_eq!(*runs.borrow(), [
            (words(&["--minimized"]), p.clone(), None, "1".to_string()),
            (words(&["--restarted"]), p.clone(), Some("1".to_string()), "1".to_string()), // it was streaming
            (words(&["--restarted"]), p, Some(String::new()), "1".to_string()),
        ]);
        let e = main(&launch, &words(&["--plugin", launch.temp.to_str().unwrap()]), &mut run).unwrap_err();
        assert!(e.contains("plugin.json"), "{e}");
    }

    #[test]
    fn the_launcher_stops_the_app_when_it_is_stopped() {
        struct Fake<'a> {
            running: bool,
            terminated: &'a Cell<u32>,
        }
        impl Child for Fake<'_> {
            fn poll(&mut self) -> Option<i32> {
                (!self.running).then_some(0)
            }
            fn terminate(&mut self) {
                self.terminated.set(self.terminated.get() + 1);
                self.running = false;
            }
            fn kill(&mut self) {
                panic!("it stopped when asked");
            }
        }
        let terminated = Cell::new(0);
        let mut child = Fake { running: true, terminated: &terminated };
        // Ctrl+C: the wait ends without the app having quit
        assert_eq!(wait_for(&mut child, &|| true, Duration::from_secs(1), Duration::ZERO), None);
        assert_eq!(terminated.get(), 1);
        // An app that quits by itself is left alone
        let mut done = Fake { running: false, terminated: &terminated };
        assert_eq!(wait_for(&mut done, &|| false, Duration::from_secs(1), Duration::ZERO), Some(0));
        assert_eq!(terminated.get(), 1);
    }
}
