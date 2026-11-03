//! The adb commands the desktop needs (platform/__init__.py): finding adb, devices, forwards, the reverse to the
//! pairing server, the pairing broadcast, installing the phone app, and on Windows adb's server as this app's child.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use telescope_core::i18n::tr;

pub const PAIR_BROADCAST_ACTION: &str = "com.telescope.action.PAIR";
/// The dev phone app a dev profile pairs with (dev_profile.PHONE_PACKAGE).
pub const DEV_PHONE_PACKAGE: &str = "com.telescope.dev";
pub const ADB_PORT: u16 = 5037;
const RUN_TIMEOUT: Duration = Duration::from_secs(10);
/// Windows gives each console program a window of its own unless asked not to (CREATE_NO_WINDOW).
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// What a command came back with: (exit code, stdout, stderr); -1 couldn't run, -2 timed out.
pub type Outcome = (i32, String, String);
/// The third argument is what goes to the command's stdin (none: stdin is closed).
pub type Runner = Box<dyn Fn(&[String], Duration, Option<&[u8]>) -> Outcome + Send + Sync>;

/// The package the pairing broadcast goes to: the dev app in a dev profile.
pub fn pair_broadcast_package() -> &'static str {
    if telescope_core::config::dev_profile().is_some() { DEV_PHONE_PACKAGE } else { "com.telescope" }
}

/// The folder this app's own files sit in.
fn app_dir() -> PathBuf {
    std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)).unwrap_or_default()
}

/// Where an adb shipped next to the app would be.
pub fn platform_tools_dir() -> PathBuf {
    app_dir().join("platform-tools")
}

/// Windows: the adb Telescope downloaded for this user (adb_download.downloaded_dir()).
pub fn downloaded_dir() -> PathBuf {
    downloaded_dir_in(std::env::var_os("LOCALAPPDATA"), std::env::home_dir())
}

fn downloaded_dir_in(local_app_data: Option<std::ffi::OsString>, home: Option<PathBuf>) -> PathBuf {
    let base = local_app_data.filter(|v| !v.is_empty()).map(PathBuf::from)
        .unwrap_or_else(|| home.unwrap_or_default().join("AppData").join("Local"));
    base.join("Telescope").join("platform-tools")
}

/// The phone app next to the app, or in a dev profile the dev build from the source tree.
pub fn bundled_apk_path() -> Option<PathBuf> {
    let path = if telescope_core::config::dev_profile().is_some() {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../android/app/build/outputs/apk/dev/app-dev.apk")
    } else {
        app_dir().join("Telescope.apk")
    };
    path.exists().then_some(path)
}

fn which(name: &str) -> Option<PathBuf> {
    let exe = if cfg!(windows) { format!("{name}.exe") } else { name.to_string() };
    std::env::split_paths(&std::env::var_os("PATH")?).map(|dir| dir.join(&exe)).find(|p| p.is_file())
}

/// adb from the app's folder, then the one downloaded for this user (Windows), then whatever is on PATH.
/// TELESCOPE_ADB names one outright.
pub fn adb_exe_in(tools: &Path, downloaded: Option<&Path>, path_lookup: impl Fn(&str) -> Option<PathBuf>) -> Option<PathBuf> {
    let local = tools.join(if cfg!(windows) { "adb.exe" } else { "adb" });
    if local.exists() {
        return Some(local);
    }
    if let Some(dl) = downloaded.map(|d| d.join("adb.exe")).filter(|p| p.is_file()) {
        return Some(dl);
    }
    path_lookup("adb")
}

pub fn adb_exe() -> Option<PathBuf> {
    if let Some(named) = std::env::var_os("TELESCOPE_ADB").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(named));
    }
    let downloaded = cfg!(windows).then(downloaded_dir);
    adb_exe_in(&platform_tools_dir(), downloaded.as_deref(), which)
}

/// Runs a command, never failing: a program that's missing, can't run or hangs comes back as a failed outcome.
pub fn run(cmd: &[String], timeout: Duration, input: Option<&[u8]>) -> Outcome {
    server_first(cmd, &ensure_adb_server, &|cmd| execute(cmd, timeout, input))
}

/// Anything run through adb goes after a check that its server is up.
fn server_first(cmd: &[String], ensure_server: &dyn Fn(), execute: &dyn Fn(&[String]) -> Outcome) -> Outcome {
    if cmd.first().is_some_and(|program| Path::new(program).file_stem().is_some_and(|s| s.eq_ignore_ascii_case("adb"))) {
        ensure_server();
    }
    execute(cmd)
}

fn execute(cmd: &[String], timeout: Duration, input: Option<&[u8]>) -> Outcome {
    let Some((program, args)) = cmd.split_first() else { return (-1, String::new(), "nothing to run".into()) };
    let mut command = Command::new(program);
    command.args(args).stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() }).stdout(Stdio::piped()).stderr(Stdio::piped());
    hide_window(&mut command);
    let mut child = match command.spawn() {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (-1, String::new(), format!("Not found: {program}")),
        Err(e) => return (-1, String::new(), e.to_string()),
    };
    // Feed stdin on its own thread too, then close it so the command sees the end
    if let (Some(input), Some(mut stdin)) = (input, child.stdin.take()) {
        let input = input.to_vec();
        std::thread::spawn(move || {
            let _ = std::io::Write::write_all(&mut stdin, &input);
        });
    }
    // Read both pipes on their own threads so a chatty command can't fill one and stall
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let out = std::thread::spawn(move || read_all(stdout.as_mut()));
    let err = std::thread::spawn(move || read_all(stderr.as_mut()));
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if started.elapsed() < timeout => std::thread::sleep(Duration::from_millis(5)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let (out, err) = (out.join().unwrap_or_default(), err.join().unwrap_or_default());
    match status {
        None => (-2, String::new(), "Timed out".into()),
        Some(status) => match (String::from_utf8(out), String::from_utf8(err)) {
            (Ok(o), Ok(e)) => (status.code().unwrap_or(-1), o, e),
            _ => (-1, String::new(), "output isn't text".into()),
        },
    }
}

fn read_all(pipe: Option<&mut impl std::io::Read>) -> Vec<u8> {
    let mut buf = Vec::new();
    if let Some(p) = pipe {
        let _ = p.read_to_end(&mut buf);
    }
    buf
}

#[cfg(windows)]
fn hide_window(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_window(_command: &mut Command) {}

// adb's first command starts a background server that outlives whoever ran it. On Windows a running adb.exe locks its
// folder, so this app starts the server itself as its own child and stops it at quit. A server another program runs
// is left alone and used.
pub const OWN_ADB_SERVER: bool = cfg!(windows);
static ADB_SERVER: Mutex<Option<std::process::Child>> = Mutex::new(None);

pub fn adb_server_running(port: u16) -> bool {
    std::net::TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), Duration::from_millis(300)).is_ok()
}

/// Windows: starts adb's server as this app's child unless one is up already.
pub fn ensure_adb_server() {
    ensure_server_with(OWN_ADB_SERVER, &|| adb_server_running(ADB_PORT), &|exe| server_command(exe).spawn().ok(), &|child| {
        // A crash then takes the server along too, so its folder isn't left locked
        crate::winjob::kill_with_us(child);
    }, &adb_exe);
}

fn server_command(exe: &Path) -> Command {
    let mut command = Command::new(exe);
    // "nodaemon" keeps the server in this process instead of the detached copy adb forks by default
    command.args(["nodaemon", "server"]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    hide_window(&mut command);
    command
}

fn ensure_server_with(own: bool, running: &dyn Fn() -> bool, spawn: &dyn Fn(&Path) -> Option<std::process::Child>, tie: &dyn Fn(&std::process::Child),
                      find: &dyn Fn() -> Option<PathBuf>) {
    if !own {
        return;
    }
    let mut held = ADB_SERVER.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(child) = held.as_mut() {
        if matches!(child.try_wait(), Ok(None)) {
            return;
        }
    }
    *held = None;
    let Some(exe) = find() else { return };
    if running() {
        return;
    }
    let Some(mut child) = spawn(&exe) else { return };
    tie(&child);
    // It listens within a second or so; the adb command after this would start its own otherwise
    for _ in 0..30 {
        if running() || !matches!(child.try_wait(), Ok(None)) {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    *held = Some(child);
}

/// Stops the server this app started (at quit, and before an update replaces platform-tools).
pub fn stop_adb_server() {
    let child = ADB_SERVER.lock().unwrap_or_else(|e| e.into_inner()).take();
    if let Some(mut child) = child {
        if matches!(child.try_wait(), Ok(None)) {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// adb behind a runner, so tests can say what it answers.
pub struct Adb {
    pub exe: Option<String>,
    pub runner: Runner,
}

impl Adb {
    /// The adb found now, run for real.
    pub fn system() -> Adb {
        Adb { exe: adb_exe().map(|p| p.to_string_lossy().into_owned()), runner: Box::new(run) }
    }

    pub fn available(&self) -> bool {
        self.exe.is_some()
    }

    fn command(&self, serial: Option<&str>, args: &[&str]) -> Option<Vec<String>> {
        let mut cmd = vec![self.exe.clone()?];
        if let Some(s) = serial {
            cmd.extend(["-s".to_string(), s.to_string()]);
        }
        cmd.extend(args.iter().map(|a| a.to_string()));
        Some(cmd)
    }

    fn run(&self, serial: Option<&str>, args: &[&str], timeout: Duration) -> Outcome {
        self.run_with_input(serial, args, timeout, None)
    }

    fn run_with_input(&self, serial: Option<&str>, args: &[&str], timeout: Duration, input: Option<&[u8]>) -> Outcome {
        match self.command(serial, args) {
            // No adb: subprocess would have nothing to run, so this is a failed command, not a crash
            None => (-1, String::new(), "adb not found".into()),
            Some(cmd) => (self.runner)(&cmd, timeout, input),
        }
    }

    /// (serial, state) for everything adb sees: "device" (usable), "unauthorized", "offline", ...
    pub fn device_states(&self) -> Vec<(String, String)> {
        let (rc, out, _) = self.run(None, &["devices"], RUN_TIMEOUT);
        if rc != 0 {
            return Vec::new();
        }
        out.lines().skip(1).filter_map(|line| {
            let parts: Vec<&str> = line.trim().split('\t').collect();
            (parts.len() == 2).then(|| (parts[0].to_string(), parts[1].to_string()))
        }).collect()
    }

    /// (serial, state, model) for everything adb sees; model is what `adb devices -l` names it, or "".
    pub fn device_models(&self) -> Vec<(String, String, String)> {
        let (rc, out, _) = self.run(None, &["devices", "-l"], RUN_TIMEOUT);
        if rc != 0 {
            return Vec::new();
        }
        out.lines().skip(1).filter_map(|line| {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() < 2 {
                return None;
            }
            let model = parts[2..].iter().find_map(|p| p.strip_prefix("model:")).unwrap_or("");
            Some((parts[0].to_string(), parts[1].to_string(), model.replace('_', " ")))
        }).collect()
    }

    /// The phone's own model name, which `adb devices -l` mangles; "" if adb can't say.
    pub fn model(&self, serial: &str) -> String {
        let (rc, out, _) = self.run(Some(serial), &["shell", "getprop", "ro.product.model"], Duration::from_secs(5));
        if rc == 0 { out.trim().to_string() } else { String::new() }
    }

    /// Serials of devices that accepted USB debugging.
    pub fn devices(&self) -> Vec<String> {
        self.device_states().into_iter().filter(|(_, s)| s == "device").map(|(serial, _)| serial).collect()
    }

    /// The same port on both sides; (ok, what happened).
    pub fn forward(&self, port: u16, serial: Option<&str>) -> (bool, String) {
        let (rc, _, err) = self.run(serial, &["forward", &format!("tcp:{port}"), &format!("tcp:{port}")], RUN_TIMEOUT);
        if rc == 0 { (true, format!("Port {port} forwarded")) } else { (false, err) }
    }

    /// A free local port (adb picks it) forwarded to the phone's remote port.
    pub fn forward_auto(&self, remote_port: u16, serial: Option<&str>) -> Option<u16> {
        let (rc, out, _) = self.run(serial, &["forward", "tcp:0", &format!("tcp:{remote_port}")], RUN_TIMEOUT);
        if rc != 0 {
            return None;
        }
        out.trim().lines().last()?.trim().parse().ok()
    }

    pub fn unforward(&self, port: u16, serial: Option<&str>) {
        self.run(serial, &["forward", "--remove", &format!("tcp:{port}")], RUN_TIMEOUT);
    }

    /// The phone's own localhost:port tunnelled to this machine's, so a USB-only phone reaches the pairing server.
    pub fn reverse(&self, port: u16, serial: Option<&str>) -> (bool, String) {
        let (rc, _, err) = self.run(serial, &["reverse", &format!("tcp:{port}"), &format!("tcp:{port}")], RUN_TIMEOUT);
        if rc == 0 { (true, format!("Port {port} reversed")) } else { (false, err) }
    }

    pub fn unreverse(&self, port: u16, serial: Option<&str>) {
        self.run(serial, &["reverse", "--remove", &format!("tcp:{port}")], RUN_TIMEOUT);
    }

    /// Pushes the base64 pairing offer to the Telescope app only; whether adb managed, not whether the phone got it.
    /// The offer holds the phone's token, so it goes to the phone's shell on stdin, not in adb's arguments, which any
    /// user on this machine can read.
    pub fn broadcast_pair(&self, payload_b64: &str, serial: Option<&str>, package: &str) -> (bool, String) {
        let line = format!("am broadcast -a {PAIR_BROADCAST_ACTION} -p '{package}' --es payload '{payload_b64}'\n");
        let (rc, _, err) = self.run_with_input(serial, &["shell"], RUN_TIMEOUT, Some(line.as_bytes()));
        if rc == 0 { (true, "Broadcast sent".into()) } else { (false, err) }
    }

    /// `adb install -r` onto one phone; the error is what to tell the user.
    pub fn install(&self, serial: &str, apk: &str, timeout: Duration) -> Result<(), String> {
        let (rc, out, err) = self.run(Some(serial), &["install", "-r", apk], timeout);
        let output = format!("{out}{err}").trim().to_string();
        if rc == 0 && output.contains("Success") {
            return Ok(());
        }
        if output.contains("INSTALL_FAILED_UPDATE_INCOMPATIBLE") {
            // The installed app was signed with another key (the debug builds from before release signing)
            return Err(tr("The Telescope app on the phone was signed differently. Uninstall it on the phone, then try again."));
        }
        if output.contains("INSTALL_FAILED_VERSION_DOWNGRADE") {
            return Err(tr("The phone already has a newer Telescope app."));
        }
        Err(output.lines().last().map(str::to_string).filter(|l| !l.is_empty()).unwrap_or_else(|| tr("adb install failed")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn off_windows_adb_runs_its_own_server() {
        assert_eq!(std::hint::black_box(OWN_ADB_SERVER), cfg!(windows));
        ensure_server_with(false, &|| false, &|_| panic!("started a server"), &|_| {}, &|| Some("adb".into()));
    }

    #[test]
    fn adb_commands_make_sure_the_server_is_up_first() {
        let calls = std::cell::RefCell::new(Vec::new());
        let ran = (0, String::new(), String::new());
        for cmd in [words(&["C:/pt/adb.exe", "devices"]), words(&["lsmod"])] {
            server_first(&cmd, &|| calls.borrow_mut().push("server"), &|_| {
                calls.borrow_mut().push("run");
                ran.clone()
            });
        }
        assert_eq!(*calls.borrow(), ["server", "run", "run"]);
    }

    #[test]
    fn platform_tools_dir_uses_executable_when_frozen() {
        assert_eq!(platform_tools_dir(), std::env::current_exe().unwrap().parent().unwrap().join("platform-tools"));
    }

    #[test]
    fn bundled_apk_path_returns_existing_file() {
        let apk = std::env::current_exe().unwrap().parent().unwrap().join("Telescope.apk");
        std::fs::write(&apk, b"apk").unwrap();
        assert_eq!(bundled_apk_path(), Some(apk.clone()));
        std::fs::remove_file(&apk).unwrap();
        assert_eq!(bundled_apk_path(), None);
    }

    fn fake(outcome: Outcome) -> (Adb, Arc<Mutex<Vec<Vec<String>>>>) {
        let calls: Arc<Mutex<Vec<Vec<String>>>> = Arc::default();
        let seen = calls.clone();
        let adb = Adb { exe: Some("adb".into()), runner: Box::new(move |cmd, _, _| {
            seen.lock().unwrap().push(cmd.to_vec());
            outcome.clone()
        }) };
        (adb, calls)
    }

    fn words(cmd: &[&str]) -> Vec<String> {
        cmd.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn run_returns_process_result() {
        let script = if cfg!(windows) { words(&["cmd", "/C", "echo out & echo err 1>&2 & exit 7"]) }
                     else { words(&["sh", "-c", "printf out; printf err >&2; exit 7"]) };
        let (rc, out, err) = run(&script, Duration::from_secs(5), None);
        assert_eq!(rc, 7);
        assert_eq!((out.trim(), err.trim()), ("out", "err"));
    }

    /// A command that runs for a while.
    fn lingers() -> Command {
        let mut command = if cfg!(windows) { Command::new("ping") } else { Command::new("sleep") };
        if cfg!(windows) { command.args(["-n", "30", "127.0.0.1"]) } else { command.arg("30") };
        command
    }

    #[test]
    fn run_normalizes_expected_subprocess_failures() {
        assert_eq!(run(&words(&["no-such-program-anywhere"]), Duration::from_secs(1), None),
                   (-1, String::new(), "Not found: no-such-program-anywhere".into()));
        let waits = lingers();
        let waits: Vec<String> = std::iter::once(waits.get_program()).chain(waits.get_args()).map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(run(&waits, Duration::from_millis(100), None), (-2, String::new(), "Timed out".into()));
    }

    #[test]
    fn run_reports_a_command_that_cant_run_as_a_failure() {
        let dir = telescope_core::testing::scratch_dir("adb-run");
        let file = dir.join("not-executable");
        std::fs::write(&file, "x").unwrap();
        assert_eq!(run(&[file.to_string_lossy().into_owned()], Duration::from_secs(1), None).0, -1);
        // Output that isn't text
        let noise = dir.join("noise");
        std::fs::write(&noise, b"\xff").unwrap();
        let shows = if cfg!(windows) { ["cmd", "/C", "type"] } else { ["sh", "-c", "cat \"$0\""] };
        let cmd: Vec<String> = shows.iter().map(|s| s.to_string()).chain([noise.to_string_lossy().into_owned()]).collect();
        assert_eq!(run(&cmd, Duration::from_secs(5), None).0, -1);
    }

    #[test]
    fn adb_exe_prefers_bundled_binary() {
        let dir = telescope_core::testing::scratch_dir("adb-exe");
        let bundled = dir.join(if cfg!(windows) { "adb.exe" } else { "adb" });
        std::fs::write(&bundled, "").unwrap();
        assert_eq!(adb_exe_in(&dir, None, |_| Some("/path/adb".into())), Some(bundled));
    }

    #[test]
    fn adb_exe_falls_back_to_path() {
        let dir = telescope_core::testing::scratch_dir("adb-path");
        let exe = adb_exe_in(&dir, Some(&dir), |name| Some(format!("/usr/bin/{name}").into()));
        assert_eq!(exe, Some("/usr/bin/adb".into()));
        let found = Adb { exe: exe.map(|p| p.to_string_lossy().into_owned()), runner: Box::new(|_, _, _| (0, String::new(), String::new())) };
        assert!(found.available());
        let missing = adb_exe_in(&dir, Some(&dir), |_| None);
        assert!(!Adb { exe: missing.map(|p| p.to_string_lossy().into_owned()), runner: Box::new(|_, _, _| (0, String::new(), String::new())) }.available());
    }

    #[test]
    fn adb_exe_takes_the_downloaded_one_before_path() {
        let (tools, dl) = (telescope_core::testing::scratch_dir("adb-t"), telescope_core::testing::scratch_dir("adb-dl"));
        std::fs::write(dl.join("adb.exe"), "").unwrap();
        assert_eq!(adb_exe_in(&tools, Some(&dl), |_| None), Some(dl.join("adb.exe")));
    }

    #[test]
    fn it_lives_in_local_app_data() {
        let local = telescope_core::testing::scratch_dir("local-app-data");
        assert_eq!(downloaded_dir_in(Some(local.clone().into()), None), local.join("Telescope").join("platform-tools"));
        assert_eq!(downloaded_dir_in(Some("".into()), Some("/home/x".into())), Path::new("/home/x/AppData/Local/Telescope/platform-tools"));
    }

    #[test]
    fn adb_is_found_in_the_app_folder_then_the_download_then_path() {
        let root = telescope_core::testing::scratch_dir("adb-order");
        let (app, local) = (root.join("app").join("platform-tools"), root.join("local"));
        let downloaded_dir = downloaded_dir_in(Some(local.into()), None);
        let path = |_: &str| Some(PathBuf::from(r"C:\sdk\adb.exe"));
        assert_eq!(adb_exe_in(&app, Some(&downloaded_dir), path), Some(PathBuf::from(r"C:\sdk\adb.exe")));
        std::fs::create_dir_all(&downloaded_dir).unwrap();
        std::fs::write(downloaded_dir.join("adb.exe"), "adb").unwrap();
        assert_eq!(adb_exe_in(&app, Some(&downloaded_dir), path), Some(downloaded_dir.join("adb.exe")));
        let bundled = app.join(if cfg!(windows) { "adb.exe" } else { "adb" });
        std::fs::create_dir_all(&app).unwrap();
        std::fs::write(&bundled, "adb").unwrap();
        assert_eq!(adb_exe_in(&app, Some(&downloaded_dir), path), Some(bundled));
    }

    #[test]
    fn adb_devices_returns_only_authorized_serials() {
        let (adb, _) = fake((0, "List of devices attached\nphone-1\tdevice\nphone-2\tunauthorized\nemulator-5554\tdevice\nnoise\n".into(), String::new()));
        assert_eq!(adb.devices(), ["phone-1", "emulator-5554"]);
    }

    #[test]
    fn adb_devices_returns_empty_on_command_failure() {
        assert!(fake((1, String::new(), "bad".into())).0.devices().is_empty());
    }

    #[test]
    fn adb_device_models_names_each_phone() {
        let output = "List of devices attached\n\
            10AF2Q28ND003F0        device usb:1-1 product:V2413_EEA model:V2413 device:V2413 transport_id:1\n\
            phone-2                unauthorized usb:1-2 transport_id:2\n\
            emulator-5554          device product:sdk_gphone64 model:sdk_gphone64_x86_64 transport_id:3\n";
        let (adb, _) = fake((0, output.into(), String::new()));
        let got: Vec<(String, String, String)> = adb.device_models();
        let want = [("10AF2Q28ND003F0", "device", "V2413"), ("phone-2", "unauthorized", ""), ("emulator-5554", "device", "sdk gphone64 x86 64")];
        assert_eq!(got, want.map(|(a, b, c)| (a.to_string(), b.to_string(), c.to_string())));
    }

    #[test]
    fn adb_model_asks_the_phone() {
        let (adb, calls) = fake((0, "SM-G975F\n".into(), String::new()));
        assert_eq!(adb.model("phone-1"), "SM-G975F");
        assert_eq!(*calls.lock().unwrap(), [words(&["adb", "-s", "phone-1", "shell", "getprop", "ro.product.model"])]);
        assert_eq!(fake((1, String::new(), "error: device offline".into())).0.model("phone-1"), "");
    }

    #[test]
    fn with_serial() {
        let (adb, _) = fake((0, String::new(), String::new()));
        assert_eq!(adb.command(None, &["forward"]).unwrap(), words(&["adb", "forward"]));
        assert_eq!(adb.command(Some("phone"), &["forward"]).unwrap(), words(&["adb", "-s", "phone", "forward"]));
    }

    #[test]
    fn adb_forward_builds_serial_specific_command() {
        let (adb, calls) = fake((0, String::new(), String::new()));
        assert_eq!(adb.forward(8080, Some("phone")), (true, "Port 8080 forwarded".into()));
        assert_eq!(*calls.lock().unwrap(), [words(&["adb", "-s", "phone", "forward", "tcp:8080", "tcp:8080"])]);
    }

    #[test]
    fn adb_forward_surfaces_error_and_unforward_ignores_it() {
        let (adb, calls) = fake((1, String::new(), "forward failed".into()));
        assert_eq!(adb.forward(9000, None), (false, "forward failed".into()));
        adb.unforward(9000, Some("serial"));
        assert_eq!(calls.lock().unwrap().last().unwrap(), &words(&["adb", "-s", "serial", "forward", "--remove", "tcp:9000"]));
    }

    #[test]
    fn adb_forward_auto_reads_the_port_adb_picked() {
        let (adb, calls) = fake((0, "41234\n".into(), String::new()));
        assert_eq!(adb.forward_auto(8766, Some("SER")), Some(41234));
        assert_eq!(*calls.lock().unwrap(), [words(&["adb", "-s", "SER", "forward", "tcp:0", "tcp:8766"])]);
        assert_eq!(fake((0, "junk\n".into(), String::new())).0.forward_auto(8766, None), None);
    }

    #[test]
    fn adb_reverse_builds_serial_specific_command() {
        let (adb, calls) = fake((0, String::new(), String::new()));
        assert_eq!(adb.reverse(8765, Some("phone")), (true, "Port 8765 reversed".into()));
        assert_eq!(*calls.lock().unwrap(), [words(&["adb", "-s", "phone", "reverse", "tcp:8765", "tcp:8765"])]);
    }

    #[test]
    fn adb_reverse_surfaces_error_and_unreverse_ignores_it() {
        let (adb, calls) = fake((1, String::new(), "reverse failed".into()));
        assert_eq!(adb.reverse(9000, None), (false, "reverse failed".into()));
        adb.unreverse(9000, Some("serial"));
        assert_eq!(calls.lock().unwrap().last().unwrap(), &words(&["adb", "-s", "serial", "reverse", "--remove", "tcp:9000"]));
    }

    #[test]
    fn adb_broadcast_pair_keeps_the_offer_out_of_the_arguments() {
        let seen: Arc<Mutex<Vec<(Vec<String>, Option<Vec<u8>>)>>> = Arc::default();
        let log = seen.clone();
        let adb = Adb { exe: Some("adb".into()), runner: Box::new(move |cmd, _, input| {
            log.lock().unwrap().push((cmd.to_vec(), input.map(<[u8]>::to_vec)));
            (0, String::new(), String::new())
        }) };
        assert_eq!(adb.broadcast_pair("cGF5bG9hZA==", Some("phone"), "com.telescope"), (true, "Broadcast sent".into()));
        let calls = seen.lock().unwrap();
        assert_eq!(calls[0].0, words(&["adb", "-s", "phone", "shell"]));
        assert!(!calls[0].0.iter().any(|a| a.contains("cGF5bG9hZA==")));
        let line = String::from_utf8(calls[0].1.clone().unwrap()).unwrap();
        assert_eq!(line, "am broadcast -a com.telescope.action.PAIR -p 'com.telescope' --es payload 'cGF5bG9hZA=='\n");
    }

    #[cfg(unix)]
    #[test]
    fn execute_feeds_stdin_and_closes_it() {
        let cmd = words(&["cat"]);
        assert_eq!(execute(&cmd, Duration::from_secs(5), Some(b"hello\n")), (0, "hello\n".into(), String::new()));
        assert_eq!(execute(&cmd, Duration::from_secs(5), None), (0, String::new(), String::new()));
    }

    #[test]
    fn adb_broadcast_pair_surfaces_error() {
        let (adb, _) = fake((1, String::new(), "device offline".into()));
        assert_eq!(adb.broadcast_pair("cGF5bG9hZA==", None, "com.telescope"), (false, "device offline".into()));
    }

    #[test]
    fn adb_helpers_degrade_when_adb_is_missing() {
        let adb = Adb { exe: None, runner: Box::new(|_, _, _| panic!("ran without adb")) };
        assert!(adb.devices().is_empty());
        let (ok, err) = adb.forward(8766, None);
        assert!(!ok && err.contains("adb"));
    }

    #[test]
    fn adb_install_explains_the_common_failures() {
        let cases: [(Outcome, Result<(), &str>); 5] = [
            ((0, "Performing Streamed Install\nSuccess\n".into(), String::new()), Ok(())),
            ((1, String::new(), "adb: failed to install x.apk: Failure [INSTALL_FAILED_UPDATE_INCOMPATIBLE: ...]".into()),
             Err("The Telescope app on the phone was signed differently. Uninstall it on the phone, then try again.")),
            ((1, String::new(), "Failure [INSTALL_FAILED_VERSION_DOWNGRADE]".into()), Err("The phone already has a newer Telescope app.")),
            ((1, "one\n".into(), "Failure [bad apk]\n".into()), Err("Failure [bad apk]")),
            ((1, String::new(), String::new()), Err("adb install failed")),
        ];
        for (outcome, want) in cases {
            let (adb, calls) = fake(outcome);
            assert_eq!(adb.install("SER", "x.apk", Duration::from_secs(120)), want.map_err(str::to_string));
            assert_eq!(*calls.lock().unwrap(), [words(&["adb", "-s", "SER", "install", "-r", "x.apk"])]);
        }
    }

    #[test]
    fn telescope_starts_adbs_server_as_its_own_child_once() {
        let exe = PathBuf::from("C:/Telescope/platform-tools/adb.exe");
        let (started, tied) = (Mutex::new(Vec::new()), Mutex::new(Vec::new()));
        let (up, output) = (Mutex::new(false), Mutex::new(None));
        for _ in 0..3 {
            ensure_server_with(true, &|| *up.lock().unwrap(), &|exe| {
                let cmd = server_command(exe);
                started.lock().unwrap().push(std::iter::once(cmd.get_program()).chain(cmd.get_args()).map(|a| a.to_string_lossy().into_owned()).collect::<Vec<_>>());
                *up.lock().unwrap() = true;
                // Stands in for the server: its output ends when it does
                let mut child = lingers().stdout(Stdio::piped()).spawn().unwrap();
                *output.lock().unwrap() = child.stdout.take();
                Some(child)
            }, &|child| tied.lock().unwrap().push(child.id()), &|| Some(exe.clone()));
        }
        assert_eq!(*started.lock().unwrap(), [words(&["C:/Telescope/platform-tools/adb.exe", "nodaemon", "server"])]);
        assert_eq!(tied.lock().unwrap().len(), 1);

        stop_adb_server();
        assert!(ADB_SERVER.lock().unwrap().is_none());
        let mut output = output.lock().unwrap().take().unwrap();
        let (done, ended) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = std::io::copy(&mut output, &mut std::io::sink());
            let _ = done.send(());
        });
        assert!(ended.recv_timeout(Duration::from_secs(5)).is_ok(), "the server was terminated");
        // Someone else's server is left alone
        ensure_server_with(true, &|| true, &|_| panic!("started a server"), &|_| {}, &|| Some("adb".into()));
        stop_adb_server();
    }

    #[test]
    fn the_broadcast_goes_to_the_dev_app_in_a_dev_profile() {
        let want = if telescope_core::config::dev_profile().is_some() { DEV_PHONE_PACKAGE } else { "com.telescope" };
        assert_eq!(pair_broadcast_package(), want);
    }
}
