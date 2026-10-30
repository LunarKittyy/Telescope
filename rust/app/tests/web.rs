// Telescope's own downloads are checked by the system's certificate store (tests/test_web.py, telescope/web.py): security software that
// inspects HTTPS with its own root still works, and a fresh Windows that hasn't fetched a root yet isn't a failed download. Every place
// that downloads builds its agent with RootCerts::PlatformVerifier, which also checks the server's name. A certificate that can't be checked
// is told apart from being offline where each download's failure is worded (fetch_error's tests in link and audio, updates.rs's NetError).
use std::path::Path;

/// (file, what it downloads): the downloads Python's web.urlopen carries.
const DOWNLOADS: [(&str, &str); 5] = [
    ("crates/system/src/updates.rs", "the update check and the update itself"),
    ("crates/link/src/adb_download.rs", "adb"),
    ("crates/audio/src/vb_cable.rs", "VB-Cable"),
    ("crates/vision/src/pack.rs", "the vision pack"),
    ("crates/plugin-host/src/updates.rs", "plugin updates"),
];

#[test]
fn every_download_goes_through_the_system_certificate_check() {
    for (file, what) in DOWNLOADS {
        let source = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(file)).unwrap();
        assert!(source.contains("RootCerts::PlatformVerifier"), "{what} ({file}) isn't checked by the system's certificate store");
    }
}
