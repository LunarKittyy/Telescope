// The log says which stream a "Status:" line is about.
mod rig;

use rig::Rig;

fn notes() -> Vec<String> {
    telescope_core::diagnostics::events().lines(10_000).into_iter().filter(|l| l.contains("NOTE")).collect()
}

#[test]
fn a_status_line_names_its_stream_and_output() {
    let r = Rig::new("log-names");
    r.start();
    r.worker(0).says("ok", "Streaming 4 x 4 at 24 fps to Telescope");
    r.pump();
    r.add("oneplus");
    r.pump();
    r.worker(1).says("ok", "Streaming 4 x 4 at 24 fps to Telescope 2");
    r.pump();
    let lines = notes();
    assert!(lines.iter().any(|l| l.contains("Pixel -> Telescope: Status: Streaming")), "{lines:?}");
    assert!(lines.iter().any(|l| l.contains("OnePlus -> Telescope #2: Status: Streaming")), "{lines:?}");
}
