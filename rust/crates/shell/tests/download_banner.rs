// The pack download's banner in the Plugins tab, driven through Slint's testing backend: nothing reaches the desktop.
use i_slint_backend_testing::ElementHandle;
use slint::ComponentHandle;
use slint_interpreter::Value;
use telescope_core::config::{ConfigFile, CONFIG_FILENAME};
use telescope_shell::plugins_tab::{download_banner, DownloadBanner};
use telescope_shell::{settings, window};

const TITLE: &str = "Downloading the vision pack";

// Progress while it runs, the download's own error once it failed, nothing otherwise.
#[test]
fn the_banner_follows_the_download() {
    assert_eq!(download_banner(TITLE, 47, false, 0, 0, ""), None);
    let b0 = download_banner(TITLE, 47, true, 0, 0, "").unwrap();
    assert!(b0.detail.contains("47 MB"));
    assert_eq!(download_banner(TITLE, 47, true, 0, 0, ""), Some(DownloadBanner { title: TITLE.into(), percent: None, error: String::new(), detail: b0.detail.clone() }));
    assert_eq!(download_banner(TITLE, 47, true, 40, 100, "").unwrap().percent, Some(40));
    assert_eq!(download_banner(TITLE, 47, true, 100, 100, "").unwrap().percent, Some(99), "all the bytes in is not yet unpacked and checked");
    let failed = download_banner(TITLE, 47, false, 40, 100, "No network").unwrap();
    assert_eq!(failed.error, "The download didn't work. No network");
    assert_eq!(failed.percent, None);
}

// The window gives the banner room over the restart bar and takes it back when the download is over.
#[test]
fn the_banner_opens_in_the_plugins_tab_and_shuts_again() {
    i_slint_backend_testing::init_no_event_loop();
    let dir = telescope_core::testing::scratch_dir("download-banner");
    let w = window::build(ConfigFile::at(dir.join(CONFIG_FILENAME)), |bus| telescope_shell::plugins::builtins(None, bus), None, None).unwrap();
    settings::open(&w, "plugins");
    let s = settings::current().unwrap();
    s.ui.window().set_size(slint::LogicalSize::new(1000.0, 700.0));
    telescope_shell::motion::set(&s.ui, telescope_shell::motion::Motion::Full);
    let found = || ElementHandle::find_by_accessible_label(&s.ui, TITLE).count();
    assert_eq!(s.ui.get_property("download-shown").unwrap(), Value::Bool(false));
    assert_eq!(found(), 0);
    s.ui.set_property("download-title", Value::String(TITLE.into())).unwrap();
    s.ui.set_property("download-percent", Value::Number(40.0)).unwrap();
    s.ui.set_property("download-shown", Value::Bool(true)).unwrap();
    for _ in 0..40 {
        let _ = found();
        i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(16));
    }
    assert!(found() > 0, "the banner is up");
    s.ui.set_property("download-shown", Value::Bool(false)).unwrap();
    for _ in 0..40 {
        let _ = found();
        i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(16));
    }
    assert_eq!(found(), 0, "and gone again");
    settings::forget();
}
