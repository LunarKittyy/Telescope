// What compiling the settings page costs at start, and what the process holds with the compiled page kept, dropped and trimmed.
// A measurement, not a check: `cargo test -p telescope-app --release --test settings_memory -- --ignored --nocapture`.
#![cfg(target_os = "linux")]
mod rig;

use rig::Rig;
use telescope_core::plugin::Plugin;
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
fn the_settings_page_compile_time_and_memory() {
    let r = Rig::with("settings-memory", |_, _| cards());
    r.w.process();
    let built = rss_mb();
    telescope_system::memory::release_freed_memory();
    let built_trimmed = rss_mb();
    let start = std::time::Instant::now();
    settings::prepare(&r.w);
    let took = start.elapsed();
    let held = rss_mb();
    telescope_system::memory::release_freed_memory();
    let held_trimmed = rss_mb();
    // Dropping the page also drops what the compiler built it from
    settings::forget();
    let dropped = rss_mb();
    telescope_system::memory::release_freed_memory();
    let dropped_trimmed = rss_mb();
    eprintln!("compile at start: {:.0} ms", took.as_secs_f64() * 1000.0);
    eprintln!("RSS after the window is built: {built:.0} MB, after a trim {built_trimmed:.0} MB");
    eprintln!("RSS with the compiled page held: {held:.0} MB, after a trim {held_trimmed:.0} MB");
    eprintln!("RSS with the compiled page dropped: {dropped:.0} MB, after a trim {dropped_trimmed:.0} MB");
}
