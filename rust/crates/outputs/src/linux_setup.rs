//! Setting up v4l2loopback (platform/linux.py): loading it with Telescope's settings, at boot too if asked, adding and
//! removing the extra cameras, each behind one password prompt. Nothing here runs as root by itself: pkexec asks, sudo
//! is only tried where it can't ask, and otherwise the command to paste into a terminal comes back.

use telescope_core::i18n::Fields;
use telescope_core::i18n::{n_, tr};
use telescope_core::output::SetupResult;

pub const PHONE_DEV: &str = "/dev/video11";
pub const OBS_DEV: &str = "/dev/video10";
pub const PERSIST_MODPROBE_CONF: &str = "/etc/modprobe.d/99-telescope-v4l2loopback.conf";
pub const PERSIST_MODULES_CONF: &str = "/etc/modules-load.d/99-telescope-v4l2loopback.conf";
/// What OBS's own node is called.
pub const OBS_LABEL: &str = "OBS Virtual Camera";
/// What the cameras were called before they matched Windows. Old boot files and a module loaded under them still say so.
pub const OLD_LABEL: &str = "Phone Camera";

/// What apps list slot's camera as on Linux: the same as on Windows ("Telescope", "Telescope #2").
pub fn slot_label(slot: u32) -> String {
    crate::wincam::slot_label(slot)
}

/// What slot's camera was called by older versions ("Phone Camera", "Phone Camera 2").
pub fn old_slot_label(slot: u32) -> String {
    if slot == 0 { OLD_LABEL.into() } else { format!("{OLD_LABEL} {}", slot + 1) }
}

/// Both names of slot's camera, the current one first.
pub fn slot_labels(slot: u32) -> [String; 2] {
    [slot_label(slot), old_slot_label(slot)]
}

/// Which cameras the module is loaded with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    /// OBS's node and Telescope's own.
    Base,
    /// Those and the extra cameras too.
    Extras,
}

impl Shape {
    fn slots(self) -> u32 {
        match self {
            Shape::Base => 1,
            Shape::Extras => crate::MAX_SLOTS,
        }
    }
}

/// v4l2loopback's settings for loading it now and at boot, in this order.
pub fn params(shape: Shape) -> [(&'static str, String); 4] {
    let labels: Vec<String> = std::iter::once(OBS_LABEL.to_string()).chain((0..shape.slots()).map(slot_label)).collect();
    [
        ("devices", labels.len().to_string()),
        ("video_nr", (10..10 + labels.len()).map(|n| n.to_string()).collect::<Vec<_>>().join(",")),
        ("card_label", labels.join(",")),
        ("exclusive_caps", "1".into()),
    ]
}

pub const NO_PROMPT: &str = n_("Telescope couldn't ask for your password here. Run this in a terminal, then try again.");
pub use telescope_core::output::SETUP_CANCELLED as CANCELLED;

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

fn param(shape: Shape, name: &str) -> String {
    params(shape).into_iter().find(|(k, _)| *k == name).map(|(_, v)| v).unwrap_or_default()
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
pub fn options_line(shape: Shape) -> String {
    format!("options v4l2loopback devices={} video_nr={} card_label=\"{}\" exclusive_caps={}\n",
            param(shape, "devices"), param(shape, "video_nr"), param(shape, "card_label"), param(shape, "exclusive_caps"))
}

/// The options line in a boot file's text, if it has one.
fn options_in(text: &str) -> Option<&str> {
    text.lines().map(str::trim).find(|l| l.starts_with("options v4l2loopback"))
}

/// Whether a boot file's text asks for the extra cameras (under either name).
fn text_has_extras(text: &str) -> bool {
    options_in(text).is_some_and(|l| l.contains(&slot_label(1)) || l.contains(&old_slot_label(1)))
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

fn modprobe_step(shape: Shape) -> Step {
    Step::Run(format!("modprobe v4l2loopback devices={} video_nr={} card_label={} exclusive_caps={}",
                      param(shape, "devices"), param(shape, "video_nr"), quote(&param(shape, "card_label")), param(shape, "exclusive_caps")))
}

fn persist_steps(shape: Shape) -> Vec<Step> {
    vec![Step::Write(PERSIST_MODPROBE_CONF.into(), options_line(shape).trim_end().into()),
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
        let shape = self.boot_shape();
        let mut steps = vec![Step::Run("modprobe -r v4l2loopback".into()), Step::Pause("sleep 0.5".into()), modprobe_step(shape)];
        steps.extend(self.refresh_steps(shape));
        let r = self.privileged(&steps, 90, &tr("reload failed"));
        if r.ok { ok(format!("{PHONE_DEV} + {OBS_DEV}")) } else { r }
    }

    /// Load v4l2loopback with Telescope's settings, and with persist at every boot too, behind one password prompt.
    /// Never unloads an already-running module, since that could break other setups.
    pub fn setup(&self, persist: bool) -> SetupResult {
        if self.module_loaded() {
            return failed(tr("{device} is missing since v4l2loopback is already loaded with other settings. Close any app using a virtual camera, click Unload and then Load.")
                .field("device", PHONE_DEV));
        }
        for dev in [PHONE_DEV, OBS_DEV] {
            if self.sys.exists(dev) {
                return failed(tr("{device} already exists and is not a v4l2loopback device.").field("device", dev));
            }
        }
        let (modprobe_conf, modules_conf) = self.persist_status();
        // Someone else's modprobe.d options would fight ours at boot; leave theirs alone.
        let persist = persist && !(modprobe_conf || modules_conf) && self.conflicting_confs().is_empty();
        let shape = self.boot_shape();
        let mut steps = if persist { persist_steps(shape) } else { self.refresh_steps(shape) };
        steps.push(modprobe_step(shape));
        let r = self.privileged(&steps, 60, &tr("modprobe failed"));
        if r.ok {
            return ok(tr("Loaded: {phone} + {obs}").field("phone", PHONE_DEV).field("obs", OBS_DEV));
        }
        if r.message.to_lowercase().contains("not found") {
            return failed(tr("v4l2loopback isn't installed. Install it via your package manager (v4l2loopback-dkms on Debian/Ubuntu/Arch, v4l2loopback via RPM Fusion on Fedora/Nobara), then try again."));
        }
        r
    }

    /// Add a v4l2loopback camera for each label to the running module, behind one password prompt. Telescope's boot file,
    /// if there is one, asks for the extra cameras from then on.
    pub fn add_devices(&self, labels: &[String]) -> SetupResult {
        if !self.sys.which("v4l2loopback-ctl") {
            return failed(tr("This v4l2loopback can't add cameras while it runs (v4l2loopback-ctl is missing). Install v4l2loopback-utils or a newer v4l2loopback, then try again."));
        }
        let mut steps: Vec<Step> = labels.iter().map(|l| Step::Run(format!("v4l2loopback-ctl add -n {}", quote(l)))).collect();
        steps.extend(self.refresh_steps(Shape::Extras));
        let r = self.privileged(&steps, 60, &tr("v4l2loopback-ctl add failed"));
        if r.ok { ok(labels.join(", ")) } else { r }
    }

    /// Take the cameras add_devices added (their /dev/video nodes) away again, behind one password prompt. Telescope's
    /// boot file, if there is one, stops asking for them.
    pub fn remove_devices(&self, devices: &[String]) -> SetupResult {
        if !self.sys.which("v4l2loopback-ctl") {
            return failed(tr("v4l2loopback-ctl is missing. Unloading the module, or a restart, removes them too."));
        }
        let mut steps: Vec<Step> = devices.iter().map(|d| Step::Run(format!("v4l2loopback-ctl delete {}", quote(d)))).collect();
        steps.extend(self.refresh_steps(Shape::Base));
        let r = self.privileged(&steps, 60, &tr("v4l2loopback-ctl delete failed (an app may still have one open)"));
        if r.ok { ok(devices.join(", ")) } else { r }
    }

    /// Whether Telescope's own boot files exist: (modprobe.d, modules-load.d).
    pub fn persist_status(&self) -> (bool, bool) {
        (self.sys.exists(PERSIST_MODPROBE_CONF), self.sys.exists(PERSIST_MODULES_CONF))
    }

    /// Either boot file counts: the switch shows on (_refresh_persist_status).
    pub fn persisted(&self) -> bool {
        let (modprobe, modules) = self.persist_status();
        modprobe || modules
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

    /// What Telescope's own boot file asks for: the extra cameras if it lists them, else the base pair (also with no
    /// file, or one that can't be read).
    pub fn boot_shape(&self) -> Shape {
        match self.own_conf_text() {
            Some(text) if text_has_extras(&text) => Shape::Extras,
            _ => Shape::Base,
        }
    }

    /// The text of Telescope's own modprobe.d file, None if it isn't there or can't be read.
    fn own_conf_text(&self) -> Option<String> {
        if !self.sys.exists(PERSIST_MODPROBE_CONF) {
            return None;
        }
        self.sys.modprobe_confs().into_iter().find(|(path, _)| path == PERSIST_MODPROBE_CONF).and_then(|(_, text)| text)
    }

    /// The step that brings Telescope's own boot file (never anyone else's) up to shape, if it's there and says
    /// something else. Nothing when it isn't there: switching the boot files on is persist_enable's job.
    fn refresh_steps(&self, shape: Shape) -> Vec<Step> {
        if !self.sys.exists(PERSIST_MODPROBE_CONF) {
            return Vec::new();
        }
        let line = options_line(shape);
        // One we can't read might ask for the extras, so it's left alone rather than guessed over
        let Some(text) = self.own_conf_text() else { return Vec::new() };
        if options_in(&text) == Some(line.trim_end()) {
            return Vec::new();
        }
        vec![Step::Write(PERSIST_MODPROBE_CONF.into(), line.trim_end().into())]
    }

    /// Write the boot files, asking for the extra cameras too if they exist right now; nothing to do if the files
    /// are there already.
    pub fn persist_enable(&self, extras: bool) -> SetupResult {
        let (a, b) = self.persist_status();
        if a || b {
            return ok(tr("Already persisted: {path}").field("path", PERSIST_MODPROBE_CONF));
        }
        let conflicts = self.conflicting_confs();
        if !conflicts.is_empty() {
            return failed(tr("Existing v4l2loopback config found in {paths} - remove it first").field("paths", conflicts.join(", ")));
        }
        let shape = if extras { Shape::Extras } else { Shape::Base };
        let r = self.privileged(&persist_steps(shape), 30, &tr("Failed to write persistence files"));
        if !r.ok {
            return r;
        }
        ok(tr("Wrote {first} and {second}").field("first", PERSIST_MODPROBE_CONF).field("second", PERSIST_MODULES_CONF))
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
        ok(tr("Removed {paths}").field("paths", existing.join(", ")))
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

    // test_setup.py::test_refresh_persist_status
    #[test]
    fn either_boot_file_counts_as_persisted() {
        for (existing, persisted) in [(vec![PERSIST_MODPROBE_CONF], true), (vec![PERSIST_MODULES_CONF], true), (vec![], false)] {
            let fake = Fake { existing: existing.iter().map(|p| p.to_string()).collect(), ..Fake::default() };
            assert_eq!(Setup { sys: &fake }.persisted(), persisted, "{existing:?}");
        }
    }

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
        for shape in [Shape::Base, Shape::Extras] {
            let line = options_line(shape);
            assert!(line.starts_with("options v4l2loopback ") && line.ends_with('\n'));
            for (k, v) in params(shape) {
                assert!(line.contains(k) && line.contains(&v));
            }
        }
    }

    #[test]
    fn the_two_shapes_name_the_cameras_like_windows_does() {
        assert_eq!(options_line(Shape::Base),
                   "options v4l2loopback devices=2 video_nr=10,11 card_label=\"OBS Virtual Camera,Telescope\" exclusive_caps=1\n");
        assert_eq!(options_line(Shape::Extras),
                   "options v4l2loopback devices=5 video_nr=10,11,12,13,14 card_label=\"OBS Virtual Camera,Telescope,Telescope #2,Telescope #3,Telescope #4\" exclusive_caps=1\n");
        assert_eq!((old_slot_label(0), old_slot_label(3)), ("Phone Camera".to_string(), "Phone Camera 4".to_string()));
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
        assert_eq!(f.calls()[1].0, ["sudo", "-n", "sh", "-c", "modprobe -r v4l2loopback"]);
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
        for (_, v) in params(Shape::Base) {
            assert!(cmd[3].contains(&v));
        }
        assert!(cmd[3].contains("card_label=OBS Virtual Camera,Telescope exclusive_caps=1") || cmd[3].contains("card_label='OBS Virtual Camera,Telescope'"));
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
    #[cfg(unix)]
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
        assert_eq!(std::fs::read_to_string(dir.join("a-99-telescope-v4l2loopback.conf")).unwrap(), options_line(Shape::Base));
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
        assert!(setup(&f).persist_enable(false).ok);
        let f = Fake { confs: vec![("/etc/conflict.conf".into(), Some("options v4l2loopback x".into()))], ..Fake::default() };
        let r = setup(&f).persist_enable(false);
        assert!(!r.ok && r.message.contains("/etc/conflict.conf"));
    }

    // test_platform_linux.py::test_persist_enable_writes_both_files_and_surfaces_errors
    #[test]
    fn persist_enable_writes_both_files_and_surfaces_errors() {
        let f = Fake::with(&["pkexec"], &[(0, "", "")]);
        assert!(setup(&f).persist_enable(false).ok);
        let s = f.calls()[0].0.last().unwrap().clone();
        assert!(s.contains(PERSIST_MODPROBE_CONF) && s.contains(PERSIST_MODULES_CONF) && s.contains(options_line(Shape::Base).trim_end()));
        let f = Fake::with(&["pkexec"], &[(1, "", "")]);
        assert_eq!(setup(&f).persist_enable(false), failed("Failed to write persistence files"));
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
        let labels = vec!["Telescope #2".to_string(), "Telescope #3".to_string()];
        assert_eq!(setup(&f).add_devices(&labels), ok("Telescope #2, Telescope #3"));
        assert_eq!(f.calls().len(), 1);
        assert!(f.calls()[0].0.last().unwrap().contains("v4l2loopback-ctl add -n 'Telescope #2' && v4l2loopback-ctl add -n 'Telescope #3'"));
    }

    // test_platform_linux.py::test_adding_cameras_without_the_tool_says_what_to_install
    #[test]
    fn adding_cameras_without_the_tool_says_what_to_install() {
        let f = Fake::default();
        let r = setup(&f).add_devices(&["Telescope #2".into()]);
        assert!(!r.ok && r.message.contains("v4l2loopback-utils"));
    }

    // test_platform_linux.py::test_removing_cameras_deletes_each_in_one_privileged_script
    #[test]
    fn removing_cameras_deletes_each_in_one_privileged_script() {
        let f = Fake::with(&["pkexec", "v4l2loopback-ctl"], &[(0, "", "")]);
        let devs = vec!["/dev/video11".to_string(), "/dev/video12".to_string()];
        assert_eq!(setup(&f).remove_devices(&devs), ok("/dev/video11, /dev/video12"));
        assert_eq!(f.calls().len(), 1);
        assert!(f.calls()[0].0.last().unwrap().contains("v4l2loopback-ctl delete /dev/video11 && v4l2loopback-ctl delete /dev/video12"));
    }

    /// A fake with our boot file there, saying text, plus the pkexec that runs what it's given.
    fn with_boot_file(text: &str) -> Fake {
        let mut f = Fake::with(&["pkexec", "v4l2loopback-ctl"], &[(0, "", "")]);
        f.existing = vec![PERSIST_MODPROBE_CONF.into(), PERSIST_MODULES_CONF.into()];
        f.confs = vec![(PERSIST_MODPROBE_CONF.into(), Some(text.into()))];
        f
    }

    fn script_of(f: &Fake) -> String {
        f.calls()[0].0.last().unwrap().clone()
    }

    const OLD_BOOT_FILE: &str = "options v4l2loopback devices=2 video_nr=10,11 card_label=\"OBS Virtual Camera,Phone Camera\" exclusive_caps=1\n";

    fn base_write() -> String {
        format!("printf '%s\\n' {} > {PERSIST_MODPROBE_CONF}", quote(options_line(Shape::Base).trim_end()))
    }

    fn extras_write() -> String {
        format!("printf '%s\\n' {} > {PERSIST_MODPROBE_CONF}", quote(options_line(Shape::Extras).trim_end()))
    }

    #[test]
    fn an_old_boot_file_is_rewritten_when_loading_with_or_without_persist() {
        for persist in [true, false] {
            let f = with_boot_file(OLD_BOOT_FILE);
            assert!(setup(&f).setup(persist).ok);
            let s = script_of(&f);
            assert!(s.starts_with(&base_write()), "{s}");
            assert!(s.contains("card_label='OBS Virtual Camera,Telescope'"), "{s}");
            assert!(!s.contains(PERSIST_MODULES_CONF), "only our modprobe file is rewritten: {s}");
        }
    }

    #[test]
    fn an_old_boot_file_is_rewritten_when_reloading() {
        let f = with_boot_file(OLD_BOOT_FILE);
        assert!(setup(&f).reload().ok);
        let s = script_of(&f);
        assert!(s.starts_with("modprobe -r v4l2loopback && sleep 0.5 && modprobe v4l2loopback devices=2 "), "{s}");
        assert!(s.ends_with(&base_write()), "{s}");
        assert_eq!(f.calls().len(), 1, "one prompt");
    }

    #[test]
    fn a_boot_file_that_is_already_current_is_left_alone() {
        for text in [options_line(Shape::Base), options_line(Shape::Extras).trim_end().to_string()] {
            let f = with_boot_file(&text);
            assert!(setup(&f).reload().ok);
            assert!(!script_of(&f).contains("printf"));
            let f = with_boot_file(&text);
            assert!(setup(&f).setup(true).ok);
            assert!(!script_of(&f).contains("printf"));
        }
    }

    #[test]
    fn no_boot_file_is_made_when_loading_without_persist() {
        let f = Fake::with(&["pkexec"], &[(0, "", "")]);
        assert!(setup(&f).setup(false).ok);
        assert!(!script_of(&f).contains("printf"));
        let f = Fake::with(&["pkexec"], &[(0, "", "")]);
        assert!(setup(&f).reload().ok);
        assert!(!script_of(&f).contains("printf"));
    }

    #[test]
    fn someone_elses_files_are_never_rewritten() {
        let mut f = with_boot_file(OLD_BOOT_FILE);
        f.confs.push(("/etc/modprobe.d/other.conf".into(), Some("options v4l2loopback devices=1 card_label=\"Phone Camera\"\n".into())));
        assert!(setup(&f).reload().ok);
        let s = script_of(&f);
        assert_eq!(s.matches("printf").count(), 1);
        assert!(!s.contains("other.conf"));
        // not even when ours is missing
        let mut f = Fake::with(&["pkexec", "v4l2loopback-ctl"], &[(0, "", "")]);
        f.confs = vec![("/etc/modprobe.d/other.conf".into(), Some(OLD_BOOT_FILE.into()))];
        assert!(setup(&f).reload().ok && setup(&f).add_devices(&["Telescope #2".into()]).ok && setup(&f).remove_devices(&["/dev/video12".into()]).ok);
        assert!(f.calls().iter().all(|(c, _)| !c.last().unwrap().contains("printf")));
    }

    #[test]
    fn adding_cameras_asks_the_boot_file_for_them_after_they_are_added() {
        let f = with_boot_file(options_line(Shape::Base).trim_end());
        assert!(setup(&f).add_devices(&["Telescope #2".into(), "Telescope #3".into(), "Telescope #4".into()]).ok);
        let s = script_of(&f);
        assert!(s.ends_with(&extras_write()), "{s}");
        assert!(s.find("v4l2loopback-ctl add").unwrap() < s.find("printf").unwrap());
        // from an old file too, and nothing to write once it says so
        let f = with_boot_file(OLD_BOOT_FILE);
        assert!(setup(&f).add_devices(&["Telescope #2".into()]).ok);
        assert!(script_of(&f).ends_with(&extras_write()));
        let f = with_boot_file(options_line(Shape::Extras).trim_end());
        assert!(setup(&f).add_devices(&["Telescope #2".into()]).ok);
        assert!(!script_of(&f).contains("printf"));
    }

    #[test]
    fn removing_cameras_takes_them_out_of_the_boot_file() {
        let f = with_boot_file(options_line(Shape::Extras).trim_end());
        assert!(setup(&f).remove_devices(&["/dev/video12".into(), "/dev/video13".into()]).ok);
        let s = script_of(&f);
        assert!(s.ends_with(&base_write()), "{s}");
        assert!(s.find("v4l2loopback-ctl delete").unwrap() < s.find("printf").unwrap());
        let f = with_boot_file(options_line(Shape::Base).trim_end());
        assert!(setup(&f).remove_devices(&["/dev/video12".into()]).ok);
        assert!(!script_of(&f).contains("printf"));
    }

    #[test]
    fn cameras_added_or_removed_without_a_boot_file_make_none() {
        let f = Fake::with(&["pkexec", "v4l2loopback-ctl"], &[(0, "", "")]);
        assert!(setup(&f).add_devices(&["Telescope #2".into()]).ok && setup(&f).remove_devices(&["/dev/video12".into()]).ok);
        assert!(f.calls().iter().all(|(c, _)| !c.last().unwrap().contains("printf")));
    }

    #[test]
    fn switching_the_boot_files_on_matches_the_cameras_there_now() {
        let f = Fake::with(&["pkexec"], &[(0, "", "")]);
        assert!(setup(&f).persist_enable(true).ok);
        assert!(script_of(&f).contains(&quote(options_line(Shape::Extras).trim_end())));
        let f = Fake::with(&["pkexec"], &[(0, "", "")]);
        assert!(setup(&f).persist_enable(false).ok);
        assert!(script_of(&f).contains(&quote(options_line(Shape::Base).trim_end())));
    }

    #[test]
    fn loading_follows_the_shape_the_boot_file_asks_for() {
        let f = with_boot_file(options_line(Shape::Extras).trim_end());
        assert_eq!(setup(&f).boot_shape(), Shape::Extras);
        assert!(setup(&f).reload().ok);
        assert!(script_of(&f).contains("modprobe v4l2loopback devices=5 video_nr=10,11,12,13,14 card_label='OBS Virtual Camera,Telescope,Telescope #2,Telescope #3,Telescope #4' exclusive_caps=1"));
        let f = with_boot_file(options_line(Shape::Extras).trim_end());
        assert!(setup(&f).setup(false).ok);
        assert!(script_of(&f).contains("devices=5"));
        // base for the base shape, the old shape, no file and a file that can't be read
        let mut unreadable = with_boot_file("");
        unreadable.confs = vec![(PERSIST_MODPROBE_CONF.into(), None)];
        for f in [with_boot_file(options_line(Shape::Base).trim_end()), with_boot_file(OLD_BOOT_FILE), Fake::default(), unreadable] {
            assert_eq!(setup(&f).boot_shape(), Shape::Base);
        }
    }

    #[test]
    fn a_boot_file_that_cant_be_read_is_never_rewritten() {
        let mut f = with_boot_file("");
        f.confs = vec![(PERSIST_MODPROBE_CONF.into(), None)];
        assert!(setup(&f).reload().ok);
        assert!(!script_of(&f).contains(PERSIST_MODPROBE_CONF), "{}", script_of(&f));
    }
}
