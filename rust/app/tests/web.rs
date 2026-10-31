// Telescope's own downloads are checked by the system's certificate store (tests/test_web.py, telescope/web.py): security software that
// inspects HTTPS with its own root still works, and a fresh Windows that hasn't fetched a root yet isn't a failed download. A certificate that can't be
// checked is told apart from being offline where each download's failure is worded (fetch_error's tests in link and audio, updates.rs's NetError).
use std::path::Path;

/// (file, what it downloads): the downloads Python's web.urlopen carries.
const DOWNLOADS: [(&str, &str); 5] = [
    ("crates/system/src/updates.rs", "the update check and the update itself"),
    ("crates/link/src/adb_download.rs", "adb"),
    ("crates/audio/src/vb_cable.rs", "VB-Cable"),
    ("crates/vision/src/pack.rs", "the vision pack"),
    ("crates/plugin-host/src/updates.rs", "plugin updates"),
];

// Windows has no store a test can add to, so there this is what's left of the real test below
#[test]
fn every_download_goes_through_the_system_certificate_check() {
    for (file, what) in DOWNLOADS {
        let source = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(file)).unwrap();
        assert!(source.contains("RootCerts::PlatformVerifier"), "{what} ({file}) isn't checked by the system's certificate store");
    }
}

#[cfg(target_os = "linux")]
mod against_a_real_server {
    use std::sync::Arc;
    use std::time::Duration;
    use telescope_core::web::{PageHandler, Reply, Request, Serve, ServerStats, Web, WebServer};
    use telescope_link::adb_download::{self, Fetch as _};
    use telescope_link::web::SystemWeb;
    use telescope_plugin_host::updates::Fetch as _;
    use telescope_system::updates::{NetError, Opener as _};

    const WAIT: Duration = Duration::from_secs(5);

    struct Hello;

    impl PageHandler for Hello {
        fn handle(&self, _request: &Request, _stats: &ServerStats) -> Reply {
            Reply::text(200, "hello")
        }
    }

    /// Each download's real agent asked for url: None when the answer came back, else whether it failed on the certificate and what it said.
    fn every_download(url: &str) -> Vec<(&'static str, Option<(bool, String)>)> {
        let about = |why: String| {
            let lower = why.to_lowercase();
            (lower.contains("certificate") || lower.contains("invalidcertificate"), why)
        };
        vec![
            ("adb", adb_download::Web.open(url, WAIT).err().map(|e| (matches!(e, adb_download::FetchError::Certificate(_)), e.to_string()))),
            ("VB-Cable", telescope_audio::vb_cable::web(url, WAIT).err().map(|e| (matches!(e, telescope_audio::vb_cable::FetchError::Certificate(_)), format!("{e:?}")))),
            ("vision pack", telescope_vision::pack::web(url, WAIT).err().map(|e| about(e.to_string()))),
            ("plugin updates", telescope_plugin_host::updates::Https.get(url, &[]).err().map(about)),
            ("update check", telescope_system::updates::Web { timeout: WAIT }.open(url).err().map(|e| match e {
                NetError::Certificate => (true, "certificate".to_string()),
                NetError::Other(why) => (false, why),
            })),
        ]
    }

    fn server(name: &str, hosts: &[&str]) -> (std::path::PathBuf, Box<dyn WebServer>) {
        let folder = telescope_core::testing::scratch_dir(name);
        let hosts: Vec<String> = hosts.iter().map(|h| h.to_string()).collect();
        let certificate = SystemWeb.certificate(&folder, &hosts).unwrap();
        let serve = Serve { port: 0, host: "127.0.0.1".into(), label: "Download".into() };
        (certificate.cert.clone(), SystemWeb.serve(&certificate, &serve, Arc::new(Hello)).unwrap())
    }

    // test_web.py::test_the_context_checks_certificates_and_hostnames and test_it_uses_the_system_store_when_truststore_is_there, with the store
    // being the file the system's verifier reads on Linux
    #[test]
    fn every_download_checks_the_certificate_and_the_name_against_the_system_store() {
        let (cert, server_here) = server("web-here", &["127.0.0.1"]);
        let url = format!("https://127.0.0.1:{}/file", server_here.port());
        let (elsewhere, server_there) = server("web-there", &["10.9.8.7"]);
        std::env::remove_var("SSL_CERT_DIR");

        // Nobody vouches for it: the store has another certificate
        std::env::set_var("SSL_CERT_FILE", &elsewhere);
        for (what, failed) in every_download(&url) {
            let (certificate, why) = failed.unwrap_or_else(|| panic!("{what} took a certificate nobody vouches for"));
            assert!(certificate, "{what}: {why}");
        }

        // In the system's store it's good, which is how an inspecting network's root gets through
        std::env::set_var("SSL_CERT_FILE", &cert);
        for (what, failed) in every_download(&url) {
            assert_eq!(failed, None, "{what}");
        }

        // A good certificate for another name
        std::env::set_var("SSL_CERT_FILE", &elsewhere);
        let url = format!("https://127.0.0.1:{}/file", server_there.port());
        for (what, failed) in every_download(&url) {
            let (certificate, why) = failed.unwrap_or_else(|| panic!("{what} took a certificate for another name"));
            assert!(certificate, "{what}: {why}");
        }
        server_here.stop();
        server_there.stop();
    }
}
