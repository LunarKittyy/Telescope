// `telescope-rs build-info`: what the package checks ask the program. A test build is a source build, so it says dev.
use serde_json::Value;
use std::process::Command;

#[test]
fn it_prints_the_stamp_as_json() {
    let out = Command::new(env!("CARGO_BIN_EXE_telescope-rs")).arg("build-info").output().unwrap();
    assert!(out.status.success());
    let info: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(info["version"], telescope_core::version::version());
    assert_eq!(info["channel"], "dev");
    assert_eq!(info["build"], 0);
    assert_eq!(info["commit"], "");
}
