// The header button stops every stream, so it says so once there's more than one.
mod rig;

use rig::Rig;
use slint::ComponentHandle;
use telescope_shell::offscreen;

fn on_air(name: &str) -> Rig {
    let r = Rig::new(name);
    r.start();
    r.pump();
    r
}

fn labels(r: &Rig) -> (String, String, String) {
    (r.text("start-text"), r.text("start-short"), r.text("start-tip"))
}

fn shot(r: &Rig, name: &str) {
    let Ok(dir) = std::env::var("STOP_ALL_SHOTS") else { return };
    std::fs::create_dir_all(&dir).unwrap();
    let pic = offscreen::draw(r.w.ui.window(), 1000.0, 700.0, 2.0).expect("a picture");
    pic.save_png(&std::path::Path::new(&dir).join(format!("{name}.png"))).unwrap();
}

#[test]
fn one_stream_says_stop_and_two_say_stop_all() {
    let r = on_air("stop-all-label");
    assert_eq!(labels(&r), ("Stop streaming".into(), "Stop".into(), String::new()));
    shot(&r, "header-one-stream");

    r.add("oneplus");
    r.pump();
    assert_eq!(r.streaming().len(), 2);
    assert_eq!(labels(&r), ("Stop all streams".into(), "Stop all".into(), "Stop all streams".into()));
    assert!(!r.text("start-tip-detail").is_empty());
    shot(&r, "header-two-streams");

    r.w.ui.invoke("tile-stop", &[slint_interpreter::Value::String("oneplus".into())]).unwrap();
    r.pump();
    assert_eq!(labels(&r), ("Stop streaming".into(), "Stop".into(), String::new()));
}
