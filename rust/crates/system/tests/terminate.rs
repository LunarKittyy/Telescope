// A logout during the first start, from test_update_guard.py, with real signals at a real process and an update from the real installer.

#[cfg(unix)]
mod unix {
    use std::io::Write;
    use std::os::unix::process::ExitStatusExt;
    use std::path::{Path, PathBuf};
    use std::process::Child;
    use std::time::{Duration, Instant};
    use telescope_system::update_guard::{confirm_on_terminate_as, failed_build, launch, read_journal, recover, EXE_NAME, JOURNAL};
    use telescope_system::updates::install_windows;

    fn nobody() -> bool {
        false
    }

    /// The child's half: a start on trial, waiting for the system to end it.
    pub fn child(app: PathBuf) {
        // The build this copy is: 131 is the one on trial in on_trial()
        let build = std::env::var("TELESCOPE_TERMINATE_BUILD").ok().and_then(|b| b.parse().ok()).unwrap_or(131);
        confirm_on_terminate_as(app.clone(), build);
        // What a relaunched copy starts with, whatever this copy did about the signals
        let line = format!("grep -E 'Sig(Blk|Ign)' /proc/self/status > '{0}.tmp'; mv '{0}.tmp' '{0}'", app.join("child-signals").display());
        launch(&["/bin/sh".to_string(), "-c".to_string(), line]).unwrap();
        std::fs::write(app.join("ready"), "").unwrap();
        std::thread::sleep(Duration::from_secs(20));
    }

    /// A child with confirm_on_terminate installed, past the point where it hears signals.
    fn start_child(app: &Path) -> Child {
        start_child_as(app, 131)
    }

    fn start_child_as(app: &Path, build: i64) -> Child {
        let child = std::process::Command::new(std::env::current_exe().unwrap()).env("TELESCOPE_TERMINATE_APP", app).env("TELESCOPE_TERMINATE_BUILD", build.to_string()).spawn().unwrap();
        let waited = Instant::now();
        while !app.join("ready").exists() {
            assert!(waited.elapsed() < Duration::from_secs(10), "the child never got ready");
            std::thread::sleep(Duration::from_millis(10));
        }
        while !app.join("child-signals").exists() {
            assert!(waited.elapsed() < Duration::from_secs(10), "the relaunched copy never said");
            std::thread::sleep(Duration::from_millis(10));
        }
        child
    }

    fn send(child: &Child, signal: i32) {
        // SAFETY: a signal to the child this test started and hasn't reaped
        unsafe { libc::kill(child.id() as i32, signal) };
    }

    /// A Windows-layout app at 130, updated to 131 by the installer, its new version just started.
    fn on_trial(label: &str) -> PathBuf {
        let root = telescope_core::testing::scratch_dir(label);
        let app = root.join("app");
        std::fs::create_dir_all(app.join("lib-130")).unwrap();
        std::fs::write(app.join("lib-130/python311.dll"), "py 130").unwrap();
        std::fs::write(app.join(EXE_NAME), "exe 130").unwrap();
        let archive = root.join("Telescope-windows-portable.zip");
        let mut z = zip::ZipWriter::new(std::fs::File::create(&archive).unwrap());
        for (name, data) in [(EXE_NAME, "exe 131"), ("lib-131/python311.dll", "py 131")] {
            z.start_file(name, zip::write::SimpleFileOptions::default()).unwrap();
            z.write_all(data.as_bytes()).unwrap();
        }
        z.finish().unwrap();
        install_windows(&archive, &app, 131, &mut |a, b| std::fs::rename(a, b)).unwrap();
        assert_eq!(recover(&app, Duration::ZERO, &nobody, Duration::ZERO), None); // the new version starts
        assert_eq!(read_journal(&app).unwrap()["started"], true);
        app
    }

    pub fn sigterm_during_the_first_start_confirms_instead_of_rolling_back() {
        for signal in [libc::SIGTERM, libc::SIGHUP] {
            let app = on_trial(&format!("terminate-{signal}"));
            let mut child = start_child(&app);
            let signals = std::fs::read_to_string(app.join("child-signals")).unwrap();
            for line in signals.lines() {
                let mask = u64::from_str_radix(line.rsplit('\t').next().unwrap(), 16).unwrap();
                let held = (1u64 << (libc::SIGTERM - 1)) | (1u64 << (libc::SIGHUP - 1));
                assert_eq!(mask & held, 0, "a relaunched copy has to hear a logout: {line}");
            }
            send(&child, signal);
            assert_eq!(child.wait().unwrap().signal(), Some(signal), "it dies as the signal meant");
            assert!(read_journal(&app).is_none(), "confirmed");
            assert_eq!(recover(&app, Duration::ZERO, &nobody, Duration::ZERO), None, "nothing to roll back");
            assert_eq!(failed_build(&app), None);
            assert_eq!(std::fs::read_to_string(app.join(EXE_NAME)).unwrap(), "exe 131");
        }
    }

    pub fn the_old_copy_waiting_to_restart_leaves_the_way_back_when_it_gets_a_logout() {
        let app = on_trial("terminate-old");
        let mut child = start_child_as(&app, 130); // the copy that installed 131, not the one on trial
        send(&child, libc::SIGTERM);
        assert_eq!(child.wait().unwrap().signal(), Some(libc::SIGTERM));
        assert_eq!(read_journal(&app).unwrap()["state"], "trial", "still on trial");
        assert!(recover(&app, Duration::ZERO, &nobody, Duration::ZERO).is_some(), "the new build failing still rolls back");
        assert_eq!(failed_build(&app), Some(131));
        assert_eq!(std::fs::read_to_string(app.join(EXE_NAME)).unwrap(), "exe 130");
    }

    pub fn a_start_that_dies_with_no_signal_still_rolls_back() {
        let app = on_trial("terminate-killed");
        let mut child = start_child(&app); // installed, but never run: a hard kill or a crash
        send(&child, libc::SIGKILL);
        assert_eq!(child.wait().unwrap().signal(), Some(libc::SIGKILL));
        assert!(recover(&app, Duration::ZERO, &nobody, Duration::ZERO).is_some());
        assert_eq!(failed_build(&app), Some(131));
        assert_eq!(std::fs::read_to_string(app.join(EXE_NAME)).unwrap(), "exe 130");
    }

    pub fn sigterm_with_no_update_in_progress_leaves_no_journal_behind() {
        let app = telescope_core::testing::scratch_dir("terminate-idle");
        let mut child = start_child(&app);
        send(&child, libc::SIGHUP);
        assert_eq!(child.wait().unwrap().signal(), Some(libc::SIGHUP));
        assert!(!app.join(JOURNAL).exists());
    }
}

#[cfg(unix)]
const TESTS: [(&str, fn()); 4] = [
    ("sigterm_during_the_first_start_confirms_instead_of_rolling_back", unix::sigterm_during_the_first_start_confirms_instead_of_rolling_back),
    ("the_old_copy_waiting_to_restart_leaves_the_way_back_when_it_gets_a_logout", unix::the_old_copy_waiting_to_restart_leaves_the_way_back_when_it_gets_a_logout),
    ("a_start_that_dies_with_no_signal_still_rolls_back", unix::a_start_that_dies_with_no_signal_still_rolls_back),
    ("sigterm_with_no_update_in_progress_leaves_no_journal_behind", unix::sigterm_with_no_update_in_progress_leaves_no_journal_behind),
];

#[cfg(unix)]
fn main() {
    if let Some(app) = std::env::var_os("TELESCOPE_TERMINATE_APP") {
        return unix::child(app.into());
    }
    // Nextest lists the tests with --list, then runs each by name
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--list") {
        if !args.iter().any(|a| a == "--ignored") {
            TESTS.iter().for_each(|(name, _)| println!("{name}: test"));
        }
        return;
    }
    let only = args.iter().find(|a| !a.starts_with('-'));
    TESTS.iter().filter(|(name, _)| only.is_none_or(|o| o == name)).for_each(|(_, test)| test());
    println!("test result: ok");
}

#[cfg(not(unix))]
fn main() {}
