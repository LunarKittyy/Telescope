//! test_diagnostics.py, test by test.

use super::*;
use crate::testing::scratch_dir;
use log::Log;

fn state(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

/// A line without its date and time.
fn body(line: &str) -> &str {
    line.splitn(3, ' ').nth(2).unwrap()
}

#[test]
fn sanitize_removes_addresses_and_secrets() {
    for text in [
        "GET http://192.168.1.20:8765/v1/state failed",
        "phone at 10.0.0.5 didn't answer",
        "reached fe80::1c2:3aff:fe44:5566 over wifi",
        "Authorization: Bearer abc123def456",
        "token=Zx9_kq2-LmN8pQrS7tUvWxYz0aBcDeF",
        "pairing payload had nonce: 55aa55aa",
    ] {
        let out = sanitize(text);
        for secret in ["192.168", "10.0.0.5", "fe80", "abc123def456", "Zx9_kq2", "55aa55aa", "http://"] {
            assert!(!out.contains(secret), "{text} -> {out}");
        }
    }
}

#[test]
fn sanitize_keeps_ordinary_text() {
    let text = "12:30:45 stream dropped after 3 frames at 1920x1080, version 0.5.0";
    assert_eq!(sanitize(text), text);
}

#[test]
fn sanitize_hides_home() {
    let home = scratch_dir("diag").join("someone");
    let text = format!("can't read {}/.config/telescope/config.json", home.display());
    assert_eq!(sanitize_at(&text, Some(&home)), "can't read ~/.config/telescope/config.json");
}

#[test]
fn repeats_collapse_into_a_count() {
    let events = EventLog::new();
    for _ in 0..40 {
        events.note("Status: Reconnecting");
    }
    assert_eq!(events.lines(REPORT_LINES).last().unwrap(), "    (repeated 39 more times)");
    events.note("Status: Streaming");
    let lines = events.lines(REPORT_LINES);
    assert_eq!(lines.len(), 3);
    assert!(lines[0].ends_with("NOTE Status: Reconnecting"));
    assert_eq!(lines[1], "    (repeated 39 more times)");
    assert!(lines[2].ends_with("NOTE Status: Streaming"));
}

#[test]
fn ticking_status_collapses_into_a_count() {
    let events = EventLog::new();
    events.note("Status: Starting the phone's camera\u{2026}");
    events.note("Status: Starting the phone's camera...");
    for i in 1..13 {
        events.note(&format!("Status: Waiting for the phone's camera... ({i}s)"));
    }
    let lines = events.lines(REPORT_LINES);
    assert_eq!(lines.len(), 4);
    assert!(lines[0].ends_with("NOTE Status: Starting the phone's camera..."));
    assert_eq!(lines[1], "    (repeated 1 more times)");
    assert!(lines[2].ends_with("NOTE Status: Waiting for the phone's camera..."));
}

#[test]
fn writes_to_the_file_and_reads_it_back_after_a_restart() {
    let path = scratch_dir("diag").join("logs").join("telescope.log");
    let first = EventLog::with_file(&path, MAX_BYTES);
    first.note("Status: Streaming");
    first.add("CRASH", "RuntimeError: boom");
    first.close();

    let second = EventLog::with_file(&path, MAX_BYTES);
    second.add("START", "Telescope 0.5.0");
    let lines = second.lines(REPORT_LINES);
    assert_eq!(lines.iter().map(|l| body(l)).collect::<Vec<_>>(), ["NOTE Status: Streaming", "CRASH RuntimeError: boom", "START Telescope 0.5.0"]);
    assert!(fs::read_to_string(&path).unwrap().contains("boom"));
    second.close();
}

#[test]
fn rotates_and_keeps_one_previous_file() {
    let dir = scratch_dir("diag");
    let path = dir.join("telescope.log");
    let events = EventLog::with_file(&path, 2000);
    for i in 0..200 {
        events.note(&format!("event {i}"));
    }
    events.close();
    assert!(fs::metadata(&path).unwrap().len() < 2200);
    assert!(dir.join("telescope.log.1").exists());
    assert!(!dir.join("telescope.log.2").exists());
    let reread = EventLog::with_file(&path, MAX_BYTES);
    assert!(reread.lines(REPORT_LINES).last().unwrap().ends_with("event 199"));
    reread.close();
}

#[test]
fn a_log_that_cant_be_rotated_keeps_being_written() {
    let dir = scratch_dir("diag");
    let path = dir.join("telescope.log");
    // Something that can't be replaced by the log: a folder with a file in it, where the previous log goes
    fs::create_dir_all(dir.join("telescope.log.1")).unwrap();
    fs::write(dir.join("telescope.log.1").join("held"), "x").unwrap();
    let events = EventLog::with_file(&path, 200);
    for i in 0..20 {
        events.note(&format!("event {i}"));
    }
    events.close();
    assert!(fs::read_to_string(&path).unwrap().contains("event 19"));
}

#[test]
fn report_is_bounded() {
    let events = EventLog::with_file(&scratch_dir("diag").join("telescope.log"), MAX_BYTES);
    for i in 0..1000 {
        events.note(&format!("event {i} {}", "x".repeat(1000)));
    }
    let lines = events.lines(REPORT_LINES);
    assert_eq!(lines.len(), REPORT_LINES);
    // Fits a GitHub issue field
    assert!(lines.iter().map(|l| l.len() + 1).sum::<usize>() < 65536);
    events.close();
}

#[test]
fn without_a_file_it_keeps_the_last_lines_in_memory() {
    let events = EventLog::new();
    for i in 0..500 {
        events.note(&format!("event {i}"));
    }
    let lines = events.lines(REPORT_LINES);
    assert_eq!(lines.len(), REPORT_LINES);
    assert!(lines.last().unwrap().ends_with("event 499"));
}

#[test]
fn file_lines_are_sanitized() {
    let path = scratch_dir("diag").join("telescope.log");
    let events = EventLog::with_file(&path, MAX_BYTES);
    events.note("Can't reach http://192.168.1.4:8766/v1/ping");
    events.close();
    assert!(!fs::read_to_string(&path).unwrap().contains("192.168"));
}

#[test]
fn link_log_makes_and_repoints_the_shortcut() {
    let dir = scratch_dir("diag");
    let target = dir.join("tmp").join("telescope.log");
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::write(&target, "").unwrap();
    let link = dir.join("app").join("telescope.log");
    fs::create_dir_all(link.parent().unwrap()).unwrap();
    link_log(&link, &target);
    if !fs::symlink_metadata(&link).is_ok_and(|m| m.file_type().is_symlink()) {
        eprintln!("no symlink rights here");
        return;
    }
    assert_eq!(fs::canonicalize(&link).unwrap(), fs::canonicalize(&target).unwrap());
    let elsewhere = dir.join("elsewhere.log");
    fs::write(&elsewhere, "").unwrap();
    // The temp folder moved: repoint
    link_log(&link, &elsewhere);
    assert!(fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
    assert_eq!(fs::canonicalize(&link).unwrap(), fs::canonicalize(&elsewhere).unwrap());
}

#[test]
fn link_log_leaves_a_real_file_alone() {
    let dir = scratch_dir("diag");
    let link = dir.join("telescope.log");
    fs::write(&link, "mine").unwrap();
    link_log(&link, &dir.join("other.log"));
    assert_eq!(fs::read_to_string(&link).unwrap(), "mine");
}

fn record(events: &EventLog, level: log::Level, target: &str, text: &str) {
    events.log(&log::Record::builder().args(format_args!("{text}")).level(level).target(target).build());
}

#[test]
fn keeps_telescope_info_but_only_warnings_from_libraries() {
    let events = EventLog::new();
    record(&events, log::Level::Debug, "telescope::test_diag", "too detailed");
    record(&events, log::Level::Info, "telescope::test_diag", "migrated config");
    record(&events, log::Level::Info, "zeroconf::test_diag", "library chatter");
    record(&events, log::Level::Warn, "zeroconf::test_diag", "library trouble");
    record(&events, log::Level::Error, "telescope::test_diag", "decode failed");
    let lines = events.lines(REPORT_LINES);
    assert_eq!(lines.len(), 3);
    assert!(lines[0].contains("INFO telescope::test_diag: migrated config"));
    assert!(lines[1].contains("WARNING zeroconf::test_diag: library trouble"));
    assert!(lines[2].contains("ERROR telescope::test_diag: decode failed"));
}

#[test]
fn describe_exception_has_no_full_path() {
    let text = describe("disk full", file!(), line!());
    assert!(text.starts_with("panic: disk full at diagnostics_tests.rs:"), "{text}");
    assert!(!text.contains('/') && !text.contains('\\'));
    assert!(describe("x", r"C:\Users\someone\src\main.rs", 3).ends_with("at main.rs:3"));
}

#[test]
fn install_records_thread_crashes_and_keeps_old_hooks() {
    let dir = scratch_dir("diag");
    let events: &'static EventLog = Box::leak(Box::new(EventLog::new()));
    let seen = std::sync::Arc::new(Mutex::new(Vec::<String>::new()));
    let original = std::panic::take_hook();
    let recorder = seen.clone();
    std::panic::set_hook(Box::new(move |info| {
        // The hook is the process's: a test panicking on another thread at this moment lands here too
        let said = describe_panic(info);
        if said.contains("worker died") {
            recorder.lock().unwrap().push(said);
        }
    }));
    start(events, &dir.join("tmp"), Some(&dir.join("app")), "0.5.0 dev");
    fs::create_dir_all(dir.join("app")).unwrap();
    let thread = std::thread::spawn(|| panic!("worker died"));
    assert!(thread.join().is_err());
    let _ = std::panic::take_hook();
    std::panic::set_hook(original);
    events.close();
    assert_eq!(seen.lock().unwrap().len(), 1, "the hook that was there before still runs");
    let log = fs::read_to_string(dir.join("tmp").join("telescope.log")).unwrap();
    assert!(log.contains("START Telescope 0.5.0 dev"), "{log}");
    assert!(log.contains("CRASH panic: worker died"), "{log}");
    let link = dir.join("app").join("telescope.log");
    assert!(fs::symlink_metadata(&link).is_ok_and(|m| m.file_type().is_symlink()) || !link.exists());
}

#[test]
fn report() {
    let events = EventLog::new();
    events.note("Status: Can't reach the phone at 192.168.1.4");
    let text = report_for(&events, "0.5.0 dev", "0123456789", &state(&[("Connection", "usb"), ("Phone app", "0.5.0")]), "wayland");
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines[0].starts_with("Telescope "));
    assert_eq!(lines[0], "Telescope 0.5.0 dev (0123456)");
    assert!(lines.contains(&"Connection: usb"));
    assert!(lines.contains(&"Phone app: 0.5.0"));
    assert!(lines.iter().any(|l| l.contains("Slint wayland")));
    assert!(lines.contains(&format!("Log (last {REPORT_LINES} lines):").as_str()));
    assert!(!text.contains("192.168"));
}

#[test]
fn report_without_problems() {
    let text = report_for(&EventLog::new(), "0.5.0 dev", "", &[], "");
    assert_eq!(text.lines().last().unwrap(), "Log: empty");
}

#[test]
fn windows_11_is_named_from_the_build_number() {
    for (release, build, shown) in [("10", 26200, "Windows 11"), ("10", 19045, "Windows 10"), ("11", 26200, "Windows 11")] {
        let version = format!("10.0.{build}");
        assert_eq!(windows_line(release, build, &version), format!("OS: {shown} (10.0.{build})"));
    }
}

#[test]
fn the_log_folder_is_per_user_on_linux_and_in_telescope_on_windows() {
    let temp = Path::new("/tmp");
    assert_eq!(log_dir_for(None, temp, false, "luna"), Path::new("/tmp/telescope-luna"));
    assert_eq!(log_dir_for(None, temp, true, "luna"), Path::new("/tmp/Telescope"));
    assert_eq!(log_dir_for(Some("/work/profile".into()), temp, false, "luna"), Path::new("/work/profile/logs"));
}

#[test]
fn the_os_name_comes_from_os_release() {
    assert_eq!(pretty_name("NAME=Nobara\nPRETTY_NAME=\"Nobara Linux 44\"\n").as_deref(), Some("Nobara Linux 44"));
    assert_eq!(pretty_name("NAME=x\n"), None);
}

#[test]
fn the_log_folder_is_where_the_python_app_kept_it_on_linux_and_windows() {
    // What the Python app's log_dir() gave, with the temp folder at /the/temp and the user luna
    let cases = [(false, false, "/the/temp/telescope-luna"), (true, false, "/the/temp/Telescope"), (false, true, "/work/profile/logs")];
    for (windows, dev, expected) in cases {
        let ours = log_dir_for(dev.then(|| "/work/profile".into()), Path::new("/the/temp"), windows, "luna");
        assert_eq!(ours.to_string_lossy().replace('\\', "/"), expected, "windows {windows}, dev {dev}");
    }
}

#[test]
fn sanitize_gives_what_the_python_app_gave() {
    // What diagnostics.sanitize() made of each, with the home folder at /home/someone
    let samples: [(&str, &str); 30] = [
        ("GET http://192.168.1.20:8765/v1/state failed", "GET <url> failed"),
        ("see HTTPS://Example.com/a?b=c, then", "see <url> then"),
        ("ftp+x://host/path next", "<url> next"),
        ("phone at 10.0.0.5:8766 didn't answer", "phone at <ip> didn't answer"),
        ("1.2.3 and 1.2.3.4.5 and 999.999.999.999", "1.2.3 and <ip>.5 and <ip>"),
        ("v1.2.3.4x", "v1.2.3.4x"),
        ("a.b.c.d", "a.b.c.d"),
        ("reached fe80::1c2:3aff:fe44:5566 over wifi", "reached <ip> over wifi"),
        ("1:2:3:4:5:6:7:8 here", "<ip> here"),
        ("1:2:3:4:5:6:7:8:9 here", "1:2:3:4:5:6:7:8:9 here"),
        ("12:30:45 time", "12:30:45 time"),
        ("a :: b", "a <ip> b"),
        ("std::vec::Vec and Foo::bar", "std::vec::Vec and Foo::bar"),
        ("x::1 and ::1 and 1::", "x::1 and <ip> and <ip>"),
        ("fe80::1zz", "fe80::1zz"),
        ("abc:def: ok", "abc:def: ok"),
        ("AB:CD::EF!", "<ip>!"),
        ("Authorization: Bearer abc123def456", "Authorization: Bearer <redacted>"),
        ("token=Zx9_kq2-LmN8pQrS7tUvWxYz0aBcDeF", "token=<redacted>"),
        ("pairing payload had nonce: 55aa55aa", "pairing payload had nonce: <redacted>"),
        ("SECRET \"quoted\", rest", "SECRET \"<redacted>\", rest"),
        ("bearer\t\tx", "bearer\t\tx"),
        ("token: 'a b'", "token: '<redacted> b'"),
        ("key QWERTYUIOPASDFGHJKLZXCVBNM1234== end", "key <redacted> end"),
        ("short ABCDEFGHIJKLMNOPQRSTUV end", "short ABCDEFGHIJKLMNOPQRSTUV end"),
        ("path /usr/local/share/telescope/very/long/path/name/here", "path /<redacted>"),
        ("Caf\u{e9} 192.168.0.1 \u{fc}n\u{ef}", "Caf\u{e9} <ip> \u{fc}n\u{ef}"),
        ("x\u{2026}y 10.1.1.1/24", "x\u{2026}y <ip>/24"),
        ("/home/someone/.config and /home/someone2", "~/.config and /home/<user>"),
        ("", ""),
    ];
    for (text, want) in samples {
        assert_eq!(sanitize_at(text, Some(Path::new("/home/someone"))), want, "{text:?}");
    }
}

/// What Python's sanitize() did with text around zero-width, combining and other odd characters (recorded from the Python app).
fn recorded() -> serde_json::Value {
    serde_json::from_str(include_str!("../fixtures/sanitize.json")).unwrap()
}

#[test]
fn sanitize_gives_what_python_recorded() {
    let fixture = recorded();
    let home = Path::new(fixture["home"].as_str().unwrap());
    let cases = fixture["cases"].as_array().unwrap();
    let wrong: Vec<String> = cases.iter().filter_map(|case| {
        let (text, want) = (case[0].as_str().unwrap(), case[1].as_str().unwrap());
        let got = sanitize_at(text, Some(home));
        (got != want).then(|| format!("{text:?}: want {want:?}, got {got:?}"))
    }).collect();
    assert!(wrong.is_empty(), "{} of {} differ, first:\n{}", wrong.len(), cases.len(), wrong.iter().take(5).cloned().collect::<Vec<_>>().join("\n"));
}

#[test]
fn word_digit_and_space_characters_are_the_ones_python_names() {
    let fixture = recorded();
    let listed = |key: &str| -> Vec<(u32, u32)> {
        fixture[key].as_array().unwrap().iter().map(|r| (r[0].as_u64().unwrap() as u32, r[1].as_u64().unwrap() as u32)).collect()
    };
    let (word, digit, space) = (listed("word"), listed("digit"), listed("space"));
    let has = |ranges: &[(u32, u32)], c: char| ranges.iter().any(|(lo, hi)| (*lo..=*hi).contains(&(c as u32)));
    let mut wrong = Vec::new();
    for c in (0..=char::MAX as u32).filter_map(char::from_u32) {
        let is_space = matches!(project(c, View::Letters), b'\t' | b'\n' | 0x0b | 0x0c | b'\r' | b' ');
        if is_word(c) != has(&word, c) || is_digit(c) != has(&digit, c) || is_space != has(&space, c) {
            wrong.push(format!("U+{:04X}", c as u32));
        }
    }
    assert!(wrong.is_empty(), "{} differ from Python's {}, first: {:?}", wrong.len(), fixture["unicode"], &wrong[..wrong.len().min(10)]);
}

#[test]
fn a_zero_width_joiner_doesnt_hide_a_secret() {
    assert_eq!(sanitize_at("code=\u{200d}AbCdEfGhIjKlMnOpQrStUvWx", None), "code=\u{200d}<redacted>");
    assert_eq!(sanitize_at("at \u{200d}10.0.0.5\u{200d}", None), "at \u{200d}<ip>\u{200d}");
    assert_eq!(sanitize_at("see x\u{200d}http://host/p", None), "see x\u{200d}<url>");
}
