// The real program started the way main.py is in test_main.py.

// test_main.py::test_commands_and_duplicate_launches_leave_a_staged_build_alone: a command leaves the mic source a fresh start clears
#[cfg(target_os = "linux")]
#[test]
fn commands_leave_what_the_running_copy_owns_alone() {
    use std::os::unix::fs::PermissionsExt;
    let dir = telescope_core::testing::scratch_dir("commands");
    let (bin, called) = (dir.join("bin"), dir.join("pactl-called"));
    std::fs::create_dir_all(&bin).unwrap();
    let pactl = bin.join("pactl");
    std::fs::write(&pactl, format!("#!/bin/sh\ntouch '{}'\n", called.display())).unwrap();
    std::fs::set_permissions(&pactl, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());
    for args in [&["--list-actions"][..], &["--action", "microphone.mute", "mode=toggle"]] {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_telescope-rs")).args(args).env("PATH", &path)
            .env(telescope_core::config::DEV_PROFILE_ENV, dir.join("profile")).output().unwrap();
        // Nothing is running in this profile, so it says so and stops
        assert_eq!(out.status.code(), Some(1), "{}", String::from_utf8_lossy(&out.stderr));
        assert!(!called.exists(), "{args:?}");
    }
}
