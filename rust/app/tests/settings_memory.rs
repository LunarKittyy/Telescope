// What the process holds at start with Settings never opened, what the first open costs, and what dropping the window would give back.
// A measurement, not a check: `cargo test -p telescope-app --release --test settings_memory -- --ignored --nocapture`.
#![cfg(target_os = "linux")]
mod rig;

use rig::Rig;
use telescope_core::plugin::Plugin;
use slint::ComponentHandle;
use telescope_shell::settings;

fn cards() -> Vec<Box<dyn Plugin>> {
    vec![
        Box::new(telescope_effects::transforms::TransformsPlugin::new()),
        Box::new(telescope_effects::colour_grading::ColourGradingPlugin::new()),
        Box::new(telescope_effects::cinema::CinemaPlugin::new()),
        Box::new(telescope_effects::fun_pack::FunPackPlugin::new()),
        Box::new(telescope_effects::privacy_blur::PrivacyBlurPlugin::new()),
        Box::new(telescope_effects::desk::DeskPlugin::new()),
        Box::new(telescope_effects::name_tag::NameTagPlugin::new()),
        Box::new(telescope_effects::notes::NotesPlugin::new()),
        Box::new(telescope_setup::monitoring::MonitoringPlugin::new()),
        Box::new(telescope_setup::stats::StatsPlugin::new()),
    ]
}

/// The process's resident memory in MB.
fn rss_mb() -> f64 {
    let statm = std::fs::read_to_string("/proc/self/statm").unwrap();
    let pages: f64 = statm.split_whitespace().nth(1).unwrap().parse().unwrap();
    pages * 4096.0 / (1 << 20) as f64
}

#[test]
#[ignore = "measurement"]
fn the_settings_page_costs_nothing_until_it_is_opened() {
    let r = Rig::with("settings-memory", |_, _| cards());
    r.w.process();
    let at_start = rss_mb();
    telescope_system::memory::release_freed_memory();
    let at_start_trimmed = rss_mb();
    assert!(!settings::prepared() && settings::current().is_none(), "nothing of Settings is held at start");
    let start = std::time::Instant::now();
    settings::open(&r.w, "");
    let first_open = start.elapsed();
    let opened = rss_mb();
    telescope_system::memory::release_freed_memory();
    let opened_trimmed = rss_mb();
    // Closing and opening again reuses the window
    settings::window().unwrap().hide().unwrap();
    let start = std::time::Instant::now();
    settings::open(&r.w, "");
    let reopen = start.elapsed();
    eprintln!("RSS at start without opening Settings: {at_start:.0} MB, after a trim {at_start_trimmed:.0} MB");
    eprintln!("first open (compile and window): {:.0} ms", first_open.as_secs_f64() * 1000.0);
    eprintln!("RSS after the first open: {opened:.0} MB, after a trim {opened_trimmed:.0} MB");
    eprintln!("open again: {:.0} ms", reopen.as_secs_f64() * 1000.0);
    // What dropping the window on close would give back, and cost on the next open
    settings::forget();
    let dropped = rss_mb();
    telescope_system::memory::release_freed_memory();
    eprintln!("RSS with Settings dropped: {dropped:.0} MB, after a trim {:.0} MB", rss_mb());
    let start = std::time::Instant::now();
    settings::open(&r.w, "");
    eprintln!("open after a drop: {:.0} ms", start.elapsed().as_secs_f64() * 1000.0);
}
