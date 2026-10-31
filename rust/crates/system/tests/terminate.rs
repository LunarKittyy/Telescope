// test_update_guard.py::test_sigterm_during_the_first_start_confirms_instead_of_rolling_back, with real signals at a real process.

#[cfg(unix)]
fn main() {
    use serde_json::json;
    use std::os::unix::process::ExitStatusExt;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};
    use telescope_system::update_guard::{confirm_on_terminate, failed_build, read_journal, recover, write_journal};

    // The child's half: a start on trial, waiting for the system to end it
    if let Some(app) = std::env::var_os("TELESCOPE_TERMINATE_APP") {
        let app = PathBuf::from(app);
        confirm_on_terminate(app.clone());
        // What a relaunched copy starts with, whatever this copy did about the signals
        let line = format!("grep -E 'Sig(Blk|Ign)' /proc/self/status > '{0}.tmp'; mv '{0}.tmp' '{0}'", app.join("child-signals").display());
        telescope_system::update_guard::launch(&["/bin/sh".to_string(), "-c".to_string(), line]).unwrap();
        std::fs::write(app.join("ready"), "").unwrap();
        std::thread::sleep(Duration::from_secs(20));
        return;
    }

    let nobody = || false;
    for signal in [libc::SIGTERM, libc::SIGHUP] {
        let app: &Path = &telescope_core::testing::scratch_dir(&format!("terminate-{signal}"));
        write_journal(app, &json!({"platform": "windows", "to_build": 131, "state": "trial", "started": false})).unwrap();
        assert_eq!(recover(app, Duration::ZERO, &nobody, Duration::ZERO), None); // the new version starts
        assert_eq!(read_journal(app).unwrap()["started"], true);
        let mut child = std::process::Command::new(std::env::current_exe().unwrap()).env("TELESCOPE_TERMINATE_APP", app).spawn().unwrap();
        let waited = Instant::now();
        while !app.join("ready").exists() {
            assert!(waited.elapsed() < Duration::from_secs(10), "the child never got ready");
            std::thread::sleep(Duration::from_millis(10));
        }
        while !app.join("child-signals").exists() {
            assert!(waited.elapsed() < Duration::from_secs(10), "the relaunched copy never said");
            std::thread::sleep(Duration::from_millis(10));
        }
        let signals = std::fs::read_to_string(app.join("child-signals")).unwrap();
        for line in signals.lines() {
            let mask = u64::from_str_radix(line.rsplit('\t').next().unwrap(), 16).unwrap();
            let held = (1u64 << (libc::SIGTERM - 1)) | (1u64 << (libc::SIGHUP - 1));
            assert_eq!(mask & held, 0, "a relaunched copy has to hear a logout: {line}");
        }
        // SAFETY: a signal to the child this test started and hasn't reaped
        unsafe { libc::kill(child.id() as i32, signal) };
        assert_eq!(child.wait().unwrap().signal(), Some(signal), "it dies as the signal meant");
        assert!(read_journal(app).is_none(), "confirmed");
        assert_eq!(recover(app, Duration::ZERO, &nobody, Duration::ZERO), None, "nothing to roll back");
        assert_eq!(failed_build(app), None);
    }

    // With no update in progress it dies the same and leaves no journal (test_sigterm_with_no_update_in_progress_leaves_no_journal_behind)
    let app: &Path = &telescope_core::testing::scratch_dir("terminate-idle");
    let mut child = std::process::Command::new(std::env::current_exe().unwrap()).env("TELESCOPE_TERMINATE_APP", app).spawn().unwrap();
    let waited = Instant::now();
    while !app.join("ready").exists() {
        assert!(waited.elapsed() < Duration::from_secs(10), "the child never got ready");
        std::thread::sleep(Duration::from_millis(10));
    }
    // SAFETY: as above
    unsafe { libc::kill(child.id() as i32, libc::SIGHUP) };
    assert_eq!(child.wait().unwrap().signal(), Some(libc::SIGHUP));
    assert!(!app.join(".update.json").exists());
    println!("test result: ok. 3 passed");
}

#[cfg(not(unix))]
fn main() {}
