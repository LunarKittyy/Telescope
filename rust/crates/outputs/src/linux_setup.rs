//! Setting up v4l2loopback (platform/linux.py): loading it with Telescope's settings, at boot too if asked, adding and
//! removing the extra cameras, each behind one password prompt. Nothing here runs as root by itself: pkexec asks, sudo
//! is only tried where it can't ask, and otherwise the command to paste into a terminal comes back.

use telescope_core::i18n::{n_, tr};
use telescope_core::output::SetupResult;

pub const PHONE_DEV: &str = "/dev/video11";
pub const OBS_DEV: &str = "/dev/video10";
pub const PERSIST_MODPROBE_CONF: &str = "/etc/modprobe.d/99-telescope-v4l2loopback.conf";
pub const PERSIST_MODULES_CONF: &str = "/etc/modules-load.d/99-telescope-v4l2loopback.conf";
/// v4l2loopback's settings for loading it now and at boot, in this order.
pub const PARAMS: [(&str, &str); 4] = [
    ("devices", "2"),
    ("video_nr", "10,11"),
    ("card_label", "OBS Virtual Camera,Phone Camera"),
    ("exclusive_caps", "1"),
];

pub const NO_PROMPT: &str = n_("Telescope couldn't ask for your password here. Run this in a terminal, then try again.");
/// Compared, never shown, so it stays as it is.
pub const CANCELLED: &str = "Cancelled.";

/// What the setup reads and runs, so tests can stand in for the system.
pub trait System {
    /// (exit code, stdout, stderr); -1 when the program isn't there, -2 on a timeout.
    fn run(&self, cmd: &[String], timeout_s: u64) -> (i32, String, String);
    fn which(&self, program: &str) -> bool;
    fn exists(&self, path: &str) -> bool;
    /// The .conf files in /etc/modprobe.d with what they say (None for one that can't be read).
    fn modprobe_confs(&self) -> Vec<(String, Option<String>)>;
}

/// One step of a root operation.
#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    Run(String),
    /// Writes one line of text to a file.
    Write(String, String),
    /// Needs no root (a sleep between steps).
    Pause(String),
}

fn param(name: &str) -> &'static str {
    PARAMS.iter().find(|(k, _)| *k == name).map(|(_, v)| *v).unwrap_or("")
}

/// Python's shlex.quote.
pub fn quote(s: &str) -> String {
    if s.is_empty() {
        return "''".into();
    }
    if s.chars().all(|c| c.is_ascii_alphanumeric() || "@%+=:,./-_".contains(c)) {
        return s.into();
    }
    format!("'{}'", s.replace('\'', "'\"'\"'"))
}

/// The `options v4l2loopback ...` line as written to modprobe.d.
pub fn options_line() -> String {
    format!("options v4l2loopback devices={} video_nr={} card_label=\"{}\" exclusive_caps={}\n",
            param("devices"), param("video_nr"), param("card_label"), param("exclusive_caps"))
}

/// The script pkexec runs.
pub fn script(steps: &[Step]) -> String {
    steps.iter().map(|s| match s {
        Step::Write(path, line) => format!("printf '%s\\n' {} > {path}", quote(line)),
        Step::Run(cmd) | Step::Pause(cmd) => cmd.clone(),
    }).collect::<Vec<_>>().join(" && ")
}

/// The same steps as a command a person can paste into a terminal.
pub fn manual_command(steps: &[Step]) -> String {
    steps.iter().map(|s| match s {
        Step::Write(path, line) => format!("echo {} | sudo tee {path} > /dev/null", quote(line)),
        Step::Pause(cmd) => cmd.clone(),
        Step::Run(cmd) => format!("sudo {cmd}"),
    }).collect::<Vec<_>>().join(" && ")
}

fn ok(message: impl Into<String>) -> SetupResult {
    SetupResult { ok: true, message: message.into(), command: None }
}

fn failed(message: impl Into<String>) -> SetupResult {
    SetupResult { ok: false, message: message.into(), command: None }
}

fn modprobe_step() -> Step {
    Step::Run(format!("modprobe v4l2loopback devices={} video_nr={} card_label={} exclusive_caps={}",
                      param("devices"), param("video_nr"), quote(param("card_label")), param("exclusive_caps")))
}

fn persist_steps() -> Vec<Step> {
    vec![Step::Write(PERSIST_MODPROBE_CONF.into(), options_line().trim_end().into()),
         Step::Write(PERSIST_MODULES_CONF.into(), "v4l2loopback".into())]
}

pub struct Setup<'a> {
    pub sys: &'a dyn System,
}

impl Setup<'_> {
    /// Runs a shell script as root with one password prompt. pkexec shows a graphical prompt. Without it (or without
    /// a polkit agent, exit 127) sudo can only be used non-interactively: a GUI app has no terminal to ask in.
    pub fn privileged(&self, steps: &[Step], timeout: u64, failed_text: &str) -> SetupResult {
        let script = script(steps);
        let sh = |pre: &[&str]| pre.iter().map(|s| s.to_string()).chain(["sh".into(), "-c".into(), script.clone()]).collect::<Vec<String>>();
        if self.sys.which("pkexec") {
            let (rc, _, err) = self.sys.run(&sh(&["pkexec"]), timeout);
            if rc == 0 {
                return ok("");
            }
            if rc == 126 {
                return failed(CANCELLED);
            }
            if rc != 127 {
                return failed(if err.trim().is_empty() { failed_text.to_string() } else { err.trim().to_string() });
            }
        }
        if self.sys.which("sudo") {
            let (rc, _, err) = self.sys.run(&sh(&["sudo", "-n"]), timeout);
            if rc == 0 {
                return ok("");
            }
            if !err.to_lowercase().contains("password") {
                return failed(if err.trim().is_empty() { failed_text.to_string() } else { err.trim().to_string() });
            }
        }
        SetupResult { ok: false, message: tr(NO_PROMPT), command: Some(manual_command(steps)) }
    }

    pub fn module_loaded(&self) -> bool {
        let (rc, out, _) = self.sys.run(&["lsmod".into()], 10);
        rc == 0 && out.contains("v4l2loopback")
    }

    /// Whether the v4l2loopback package is installed (loadable), loaded or not.
    pub fn module_installed(&self) -> bool {
        if self.module_loaded() {
            return true;
        }
        let (rc, _, _) = self.sys.run(&["modinfo".into(), "v4l2loopback".into()], 10);
        rc == -1 || rc == 0 // no modinfo on PATH: can't tell, so don't claim it's missing
    }

    pub fn devices_ready(&self) -> bool {
        self.sys.exists(PHONE_DEV)
    }

    pub fn is_loaded(&self) -> bool {
        self.module_loaded() && self.devices_ready()
    }

    /// Unload v4l2loopback. Fails if any app still holds the device open.
    pub fn unload(&self) -> SetupResult {
        let r = self.privileged(&[Step::Run("modprobe -r v4l2loopback".into())], 30, &tr("modprobe -r failed"));
        if r.ok { ok(tr("Module unloaded")) } else { r }
    }

    /// Unload and load again in one elevated run (one password prompt).
    pub fn reload(&self) -> SetupResult {
        let steps = [Step::Run("modprobe -r v4l2loopback".into()), Step::Pause("sleep 0.5".into()), modprobe_step()];
        let r = self.privileged(&steps, 90, &tr("reload failed"));
        if r.ok { ok(format!("{PHONE_DEV} + {OBS_DEV}")) } else { r }
    }

    /// Load v4l2loopback with Telescope's settings, and with persist at every boot too, behind one password prompt.
    /// Never unloads an already-running module, since that could break other setups.
    pub fn setup(&self, persist: bool) -> SetupResult {
        if self.module_loaded() {
            return failed(tr("v4l2loopback is already loaded with other settings, so {device} isn't there. Close any app using a virtual camera, click Unload and then Load.")
                .replace("{device}", PHONE_DEV));
        }
        for dev in [PHONE_DEV, OBS_DEV] {
            if self.sys.exists(dev) {
                return failed(tr("{device} already exists and is not a v4l2loopback device.").replace("{device}", dev));
            }
        }
        let (modprobe_conf, modules_conf) = self.persist_status();
        // Someone else's modprobe.d options would fight ours at boot; leave theirs alone.
        let persist = persist && !(modprobe_conf || modules_conf) && self.conflicting_confs().is_empty();
        let mut steps = if persist { persist_steps() } else { Vec::new() };
        steps.push(modprobe_step());
        let r = self.privileged(&steps, 60, &tr("modprobe failed"));
        if r.ok {
            return ok(tr("Loaded: {phone} + {obs}").replace("{phone}", PHONE_DEV).replace("{obs}", OBS_DEV));
        }
        if r.message.to_lowercase().contains("not found") {
            return failed(tr("v4l2loopback isn't installed. Install it via your package manager (v4l2loopback-dkms on Debian/Ubuntu/Arch, v4l2loopback via RPM Fusion on Fedora/Nobara), then try again."));
        }
        r
    }

    /// Add a v4l2loopback camera for each label to the running module, behind one password prompt.
    pub fn add_devices(&self, labels: &[String]) -> SetupResult {
        if !self.sys.which("v4l2loopback-ctl") {
            return failed(tr("This v4l2loopback can't add cameras while it runs (v4l2loopback-ctl is missing). Install v4l2loopback-utils or a newer v4l2loopback, then try again."));
        }
        let steps: Vec<Step> = labels.iter().map(|l| Step::Run(format!("v4l2loopback-ctl add -n {}", quote(l)))).collect();
        let r = self.privileged(&steps, 60, &tr("v4l2loopback-ctl add failed"));
        if r.ok { ok(labels.join(", ")) } else { r }
    }

    /// Take the cameras add_devices added (their /dev/video nodes) away again, behind one password prompt.
    pub fn remove_devices(&self, devices: &[String]) -> SetupResult {
        if !self.sys.which("v4l2loopback-ctl") {
            return failed(tr("v4l2loopback-ctl is missing. Unloading the module, or a restart, removes them too."));
        }
        let steps: Vec<Step> = devices.iter().map(|d| Step::Run(format!("v4l2loopback-ctl delete {}", quote(d)))).collect();
        let r = self.privileged(&steps, 60, &tr("v4l2loopback-ctl delete failed (an app may still have one open)"));
        if r.ok { ok(devices.join(", ")) } else { r }
    }

    /// Whether Telescope's own boot files exist: (modprobe.d, modules-load.d).
    pub fn persist_status(&self) -> (bool, bool) {
        (self.sys.exists(PERSIST_MODPROBE_CONF), self.sys.exists(PERSIST_MODULES_CONF))
    }

    /// Other modprobe.d files (not ours) that already set v4l2loopback options.
    pub fn conflicting_confs(&self) -> Vec<String> {
        let mut confs = self.sys.modprobe_confs();
        confs.sort();
        confs.into_iter().filter(|(path, text)| {
            path != PERSIST_MODPROBE_CONF && text.as_deref().is_some_and(|t| t.lines().any(|l| {
                let l = l.trim();
                !l.is_empty() && !l.starts_with('#') && l.contains("v4l2loopback")
            }))
        }).map(|(p, _)| p).collect()
    }

    /// Write the boot files; nothing to do if they're there already.
    pub fn persist_enable(&self) -> SetupResult {
        let (a, b) = self.persist_status();
        if a || b {
            return ok(tr("Already persisted: {path}").replace("{path}", PERSIST_MODPROBE_CONF));
        }
        let conflicts = self.conflicting_confs();
        if !conflicts.is_empty() {
            return failed(tr("Existing v4l2loopback config found in {paths} - remove it first").replace("{paths}", &conflicts.join(", ")));
        }
        let r = self.privileged(&persist_steps(), 30, &tr("Failed to write persistence files"));
        if !r.ok {
            return r;
        }
        ok(tr("Wrote {first} and {second}").replace("{first}", PERSIST_MODPROBE_CONF).replace("{second}", PERSIST_MODULES_CONF))
    }

    /// Remove Telescope's boot files; doesn't unload the running module.
    pub fn persist_disable(&self) -> SetupResult {
        let existing: Vec<&str> = [PERSIST_MODPROBE_CONF, PERSIST_MODULES_CONF].into_iter().filter(|p| self.sys.exists(p)).collect();
        if existing.is_empty() {
            return ok(tr("Nothing to remove"));
        }
        let cmd = format!("rm -f {}", existing.iter().map(|p| quote(p)).collect::<Vec<_>>().join(" "));
        let r = self.privileged(&[Step::Run(cmd)], 30, &tr("Failed to remove persistence files"));
        if !r.ok {
            return r;
        }
        ok(tr("Removed {paths}").replace("{paths}", &existing.join(", ")))
    }
}

/// The system as it is.
pub struct Real;

impl System for Real {
    fn run(&self, cmd: &[String], timeout_s: u64) -> (i32, String, String) {
        use std::process::{Command, Stdio};
        let child = Command::new(&cmd[0]).args(&cmd[1..]).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn();
        let mut child = match child {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (-1, String::new(), format!("Not found: {}", cmd[0])),
            Err(e) => return (-1, String::new(), e.to_string()),
        };
        let until = std::time::Instant::now() + std::time::Duration::from_secs(timeout_s);
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if std::time::Instant::now() < until => std::thread::sleep(std::time::Duration::from_millis(20)),
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return (-2, String::new(), "Timed out".into());
                }
            }
        }
        match child.wait_with_output() {
            Ok(out) => (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into(), String::from_utf8_lossy(&out.stderr).into()),
            Err(e) => (-1, String::new(), e.to_string()),
        }
    }

    fn which(&self, program: &str) -> bool {
        std::env::var_os("PATH").is_some_and(|path| std::env::split_paths(&path).any(|d| d.join(program).is_file()))
    }

    fn exists(&self, path: &str) -> bool {
        std::path::Path::new(path).exists()
    }

    fn modprobe_confs(&self) -> Vec<(String, Option<String>)> {
        let Ok(entries) = std::fs::read_dir("/etc/modprobe.d") else { return Vec::new() };
        entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "conf"))
            .map(|p| (p.to_string_lossy().to_string(), std::fs::read_to_string(&p).ok())).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    struct Fake {
        tools: Vec<&'static str>,
        results: RefCell<Vec<(i32, String, String)>>,
        calls: RefCell<Vec<(Vec<String>, u64)>>,
        existing: Vec<String>,
        confs: Vec<(String, Option<String>)>,
        lsmod: Option<(i32, String)>,
    }

    impl Fake {
        fn with(tools: &[&'static str], results: &[(i32, &str, &str)]) -> Fake {
            Fake {
                tools: tools.to_vec(),
                results: RefCell::new(results.iter().map(|(a, b, c)| (*a, b.to_string(), c.to_string())).collect()),
                ..Fake::default()
            }
        }
        fn calls(&self) -> Vec<(Vec<String>, u64)> {
            self.calls.borrow().iter().filter(|(c, _)| c[0] != "lsmod").cloned().collect()
        }
    }

    impl System for Fake {
        fn run(&self, cmd: &[String], timeout_s: u64) -> (i32, String, String) {
            if cmd[0] == "lsmod" {
                let (rc, out) = self.lsmod.clone().unwrap_or((0, String::new()));
                return (rc, out, String::new());
            }
            self.calls.borrow_mut().push((cmd.to_vec(), timeout_s));
            let mut r = self.results.borrow_mut();
            if r.len() > 1 { r.remove(0) } else { r[0].clone() }
        }
        fn which(&self, program: &str) -> bool {
            self.tools.contains(&program)
        }
        fn exists(&self, path: &str) -> bool {
            self.existing.iter().any(|e| e == path)
        }
        fn modprobe_confs(&self) -> Vec<(String, Option<String>)> {
            self.confs.clone()
        }
    }

    fn setup(f: &Fake) -> Setup<'_> {
        Setup { sys: f }
    }

    // test_platform_linux.py::test_options_line_contains_every_runtime_parameter
    #[test]
    fn options_line_contains_every_runtime_parameter() {
        let line = options_line();
        assert!(line.starts_with("options v4l2loopback ") && line.ends_with('\n'));
        for (k, v) in PARAMS {
            assert!(line.contains(k) && line.contains(v));
        }
    }

    // test_platform_linux.py::test_module_loaded
    #[test]
    fn module_loaded() {
        for (rc, out, expected) in [(0, "v4l2loopback 123 0\n", true), (0, "other 123 0\n", false), (1, "v4l2loopback", false)] {
            let f = Fake { lsmod: Some((rc, out.into())), ..Fake::default() };
            assert_eq!(setup(&f).module_loaded(), expected);
        }
    }

    // test_platform_linux.py::test_device_and_combined_loaded_checks
    #[test]
    fn device_and_combined_loaded_checks() {
        let f = Fake { lsmod: Some((0, "v4l2loopback".into())), existing: vec![PHONE_DEV.into()], ..Fake::default() };
        assert!(setup(&f).devices_ready() && setup(&f).is_loaded());
        let f = Fake { existing: vec![PHONE_DEV.into()], ..Fake::default() };
        assert!(!setup(&f).is_loaded());
    }

    // test_platform_linux.py::test_pkexec_runs_the_whole_operation_behind_one_prompt
    #[test]
    fn pkexec_runs_the_whole_operation_behind_one_prompt() {
        let f = Fake::with(&["pkexec", "sudo"], &[(0, "", "")]);
        assert_eq!(setup(&f).unload(), ok("Module unloaded"));
        assert_eq!(f.calls(), [(vec!["pkexec".into(), "sh".into(), "-c".into(), "modprobe -r v4l2loopback".into()], 30)]);
    }

    // test_platform_linux.py::test_cancelling_the_prompt_is_not_an_error_to_explain
    #[test]
    fn cancelling_the_prompt_is_not_an_error_to_explain() {
        let f = Fake::with(&["pkexec", "sudo"], &[(126, "", "")]);
        assert_eq!(setup(&f).unload(), failed(CANCELLED));
        assert_eq!(f.calls().len(), 1);
    }

    // test_platform_linux.py::test_no_polkit_agent_tries_cached_sudo_then_hands_back_the_command
    #[test]
    fn no_polkit_agent_tries_cached_sudo_then_hands_back_the_command() {
        let f = Fake::with(&["pkexec", "sudo"], &[(127, "", "no agent"), (1, "", "sudo: a password is required")]);
        let r = setup(&f).unload();
        assert_eq!((r.ok, r.message.as_str()), (false, NO_PROMPT));
        assert_eq!(r.command.as_deref(), Some("sudo modprobe -r v4l2loopback"));
        assert_eq!(f.calls()[1].0[..2], ["sudo".to_string(), "-n".to_string()]);
        let f = Fake::with(&["pkexec", "sudo"], &[(127, "", ""), (0, "", "")]);
        assert!(setup(&f).unload().ok);
    }

    // test_platform_linux.py::test_sudo_is_never_run_where_it_could_ask_for_a_password
    #[test]
    fn sudo_is_never_run_where_it_could_ask_for_a_password() {
        let f = Fake::with(&["sudo"], &[(1, "", "sudo: a password is required")]);
        let r = setup(&f).reload();
        assert!(f.calls().iter().all(|(c, _)| c[..2] == ["sudo".to_string(), "-n".to_string()]));
        assert!(r.command.unwrap().starts_with("sudo modprobe -r v4l2loopback && sleep 0.5 && sudo modprobe v4l2loopback"));
        let f = Fake::with(&[], &[(0, "", "")]);
        assert_eq!(setup(&f).unload().command.as_deref(), Some("sudo modprobe -r v4l2loopback"));
        assert!(f.calls().is_empty());
    }

    // test_platform_linux.py::test_other_failures_pass_through
    #[test]
    fn other_failures_pass_through() {
        let f = Fake::with(&["pkexec"], &[(1, "", "")]);
        assert_eq!(setup(&f).unload(), failed("modprobe -r failed"));
        assert_eq!(setup(&f).reload(), failed("reload failed"));
    }

    // test_platform_linux.py::test_reload_uses_shared_parameters
    #[test]
    fn reload_uses_shared_parameters() {
        let f = Fake::with(&["pkexec"], &[(0, "", "")]);
        assert_eq!(setup(&f).reload(), ok("/dev/video11 + /dev/video10"));
        let (cmd, timeout) = f.calls()[0].clone();
        assert_eq!(cmd[..3], ["pkexec".to_string(), "sh".to_string(), "-c".to_string()]);
        for (_, v) in PARAMS {
            assert!(cmd[3].contains(v));
        }
        assert_eq!(timeout, 90);
    }

    // test_platform_linux.py::test_load_refuses_existing_module_or_conflicting_device
    #[test]
    fn load_refuses_existing_module_or_conflicting_device() {
        let f = Fake { lsmod: Some((0, "v4l2loopback".into())), ..Fake::default() };
        let r = setup(&f).setup(false);
        assert!(!r.ok && r.message.contains("click Unload and then Load"));
        let f = Fake { existing: vec![OBS_DEV.into()], ..Fake::default() };
        assert_eq!(setup(&f).setup(false), failed("/dev/video10 already exists and is not a v4l2loopback device."));
    }

    // test_platform_linux.py::test_load_invokes_modprobe_and_handles_failure
    #[test]
    fn load_invokes_modprobe_and_handles_failure() {
        let f = Fake::with(&["pkexec"], &[(0, "", "")]);
        assert_eq!(setup(&f).setup(false), ok("Loaded: /dev/video11 + /dev/video10"));
        let (cmd, timeout) = f.calls()[0].clone();
        assert!(cmd[3].starts_with("modprobe v4l2loopback ") && !cmd[3].contains("printf"));
        assert_eq!(timeout, 60);
        let f = Fake::with(&["pkexec"], &[(1, "", "denied")]);
        assert_eq!(setup(&f).setup(false), failed("denied"));
        let f = Fake::with(&["pkexec"], &[(1, "", "modprobe: FATAL: Module v4l2loopback not found")]);
        assert!(setup(&f).setup(false).message.contains("isn't installed"));
    }

    // test_platform_linux.py::test_setup_with_persist_writes_the_boot_config_in_the_same_prompt
    #[test]
    fn setup_with_persist_writes_the_boot_config_in_the_same_prompt() {
        let f = Fake::with(&["pkexec"], &[(0, "", "")]);
        assert!(setup(&f).setup(true).ok);
        assert_eq!(f.calls().len(), 1);
        let s = &f.calls()[0].0[3];
        assert!(s.contains(PERSIST_MODPROBE_CONF) && s.contains(PERSIST_MODULES_CONF));
        assert!(s.find("printf").unwrap() < s.find("modprobe v4l2loopback").unwrap());
    }

    // test_platform_linux.py::test_persist_script_writes_the_exact_boot_config
    #[test]
    fn persist_script_writes_the_exact_boot_config() {
        let f = Fake::with(&["pkexec"], &[(0, "", "")]);
        assert!(setup(&f).setup(true).ok);
        let s = f.calls()[0].0[3].clone();
        let dir = telescope_core::testing::scratch_dir("persist");
        let local = s.replace("/etc/modprobe.d/", "a-").replace("/etc/modules-load.d/", "b-");
        let writes = local.split(" && modprobe").next().unwrap();
        let st = std::process::Command::new("sh").arg("-c").arg(writes).current_dir(&dir).status().unwrap();
        assert!(st.success());
        assert_eq!(std::fs::read_to_string(dir.join("a-99-telescope-v4l2loopback.conf")).unwrap(), options_line());
        assert_eq!(std::fs::read_to_string(dir.join("b-99-telescope-v4l2loopback.conf")).unwrap(), "v4l2loopback\n");
    }

    // test_platform_linux.py::test_setup_leaves_someone_elses_boot_config_alone
    #[test]
    fn setup_leaves_someone_elses_boot_config_alone() {
        let mut f = Fake::with(&["pkexec"], &[(0, "", "")]);
        f.confs = vec![("/etc/modprobe.d/other.conf".into(), Some("options v4l2loopback devices=1".into()))];
        assert!(setup(&f).setup(true).ok);
        assert!(!f.calls()[0].0[3].contains("printf"));
    }

    // test_platform_linux.py::test_persist_status_reports_each_file
    #[test]
    fn persist_status_reports_each_file() {
        let f = Fake { existing: vec![PERSIST_MODPROBE_CONF.into()], ..Fake::default() };
        assert_eq!(setup(&f).persist_status(), (true, false));
    }

    // test_platform_linux.py::test_find_conflicting_configs_skips_ours_comments_and_read_errors
    #[test]
    fn find_conflicting_configs_skips_ours_comments_and_read_errors() {
        let f = Fake {
            confs: vec![
                ("/etc/modprobe.d/missing.conf".into(), None),
                ("/etc/modprobe.d/conflict.conf".into(), Some("options v4l2loopback devices=1\n".into())),
                ("/etc/modprobe.d/comment.conf".into(), Some("# options v4l2loopback devices=1\n".into())),
                (PERSIST_MODPROBE_CONF.into(), Some("options v4l2loopback devices=2".into())),
            ],
            ..Fake::default()
        };
        assert_eq!(setup(&f).conflicting_confs(), ["/etc/modprobe.d/conflict.conf"]);
    }

    // test_platform_linux.py::test_persist_enable_is_idempotent_and_refuses_conflicts
    #[test]
    fn persist_enable_is_idempotent_and_refuses_conflicts() {
        let f = Fake { existing: vec![PERSIST_MODPROBE_CONF.into()], ..Fake::default() };
        assert!(setup(&f).persist_enable().ok);
        let f = Fake { confs: vec![("/etc/conflict.conf".into(), Some("options v4l2loopback x".into()))], ..Fake::default() };
        let r = setup(&f).persist_enable();
        assert!(!r.ok && r.message.contains("/etc/conflict.conf"));
    }

    // test_platform_linux.py::test_persist_enable_writes_both_files_and_surfaces_errors
    #[test]
    fn persist_enable_writes_both_files_and_surfaces_errors() {
        let f = Fake::with(&["pkexec"], &[(0, "", "")]);
        assert!(setup(&f).persist_enable().ok);
        let s = f.calls()[0].0.last().unwrap().clone();
        assert!(s.contains(PERSIST_MODPROBE_CONF) && s.contains(PERSIST_MODULES_CONF) && s.contains(options_line().trim_end()));
        let f = Fake::with(&["pkexec"], &[(1, "", "")]);
        assert_eq!(setup(&f).persist_enable(), failed("Failed to write persistence files"));
    }

    // test_platform_linux.py::test_persist_disable_noop_remove_and_error
    #[test]
    fn persist_disable_noop_remove_and_error() {
        let f = Fake::default();
        assert_eq!(setup(&f).persist_disable(), ok("Nothing to remove"));
        let mut f = Fake::with(&["pkexec"], &[(0, "", "")]);
        f.existing = vec![PERSIST_MODPROBE_CONF.into(), PERSIST_MODULES_CONF.into()];
        let r = setup(&f).persist_disable();
        assert!(r.ok && r.message.starts_with("Removed "));
        assert!(f.calls()[0].0[3].starts_with("rm -f "));
        let mut f = Fake::with(&["pkexec"], &[(1, "", "denied")]);
        f.existing = vec![PERSIST_MODPROBE_CONF.into()];
        assert_eq!(setup(&f).persist_disable(), failed("denied"));
    }

    // test_platform_linux.py::test_adding_cameras_runs_one_privileged_script
    #[test]
    fn adding_cameras_runs_one_privileged_script() {
        let f = Fake::with(&["pkexec", "v4l2loopback-ctl"], &[(0, "", "")]);
        let labels = vec!["Phone Camera 2".to_string(), "Phone Camera 3".to_string()];
        assert_eq!(setup(&f).add_devices(&labels), ok("Phone Camera 2, Phone Camera 3"));
        assert_eq!(f.calls().len(), 1);
        assert!(f.calls()[0].0.last().unwrap().contains("v4l2loopback-ctl add -n 'Phone Camera 2' && v4l2loopback-ctl add -n 'Phone Camera 3'"));
    }

    // test_platform_linux.py::test_adding_cameras_without_the_tool_says_what_to_install
    #[test]
    fn adding_cameras_without_the_tool_says_what_to_install() {
        let f = Fake::default();
        let r = setup(&f).add_devices(&["Phone Camera 2".into()]);
        assert!(!r.ok && r.message.contains("v4l2loopback-utils"));
    }

    // test_platform_linux.py::test_removing_cameras_deletes_each_in_one_privileged_script
    #[test]
    fn removing_cameras_deletes_each_in_one_privileged_script() {
        let f = Fake::with(&["pkexec", "v4l2loopback-ctl"], &[(0, "", "")]);
        let devs = vec!["/dev/video11".to_string(), "/dev/video12".to_string()];
        assert_eq!(setup(&f).remove_devices(&devs), ok("/dev/video11, /dev/video12"));
        assert!(f.calls()[0].0.last().unwrap().contains("v4l2loopback-ctl delete /dev/video11 && v4l2loopback-ctl delete /dev/video12"));
    }
}
