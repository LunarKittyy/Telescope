// Picking another phone while streams run: everything stops with the old settings up, then the new phone's load.
mod rig;

use rig::{Listener, Rig};
use serde_json::{json, Value};
use telescope_core::plugin::Plugin;
use telescope_shell::shell::Effect;

fn saved(r: &Rig, phone: &str, plugin: &str) -> Value {
    r.w.state.borrow().file.load()["devices"][phone]["plugin_configs"][plugin].clone()
}

fn loaded(r: &Rig, plugin: &str) -> Value {
    r.w.state.borrow().plugin_config(plugin).unwrap()
}

#[test]
fn a_pick_during_a_wake_stops_first_so_each_stream_keeps_its_settings() {
    let r = Rig::with("switch-during-wake", |_, heard| vec![Box::new(Listener::new("panel", heard)) as Box<dyn Plugin>, Box::new(Listener::new("microphone", heard))]);
    r.link.pair("third", "Third");
    r.link.pair("fourth", "Fourth");
    {
        let st = r.w.state.borrow();
        st.file.save(&mut json!({"devices": {
            "pixel": {"plugin_configs": {"panel": {"zoom": 1}, "microphone": {"gain": 1}}},
            "oneplus": {"plugin_configs": {"panel": {"zoom": 2}, "microphone": {"gain": 2}}},
            "fourth": {"plugin_configs": {"panel": {"zoom": 4}, "microphone": {"gain": 4}}},
        }}));
    }
    r.w.state.borrow_mut().switch_device(None, Some("pixel"));
    r.w.process();
    r.start();
    r.add("oneplus");
    r.worker(0).says("ok", "Streaming");
    r.worker(1).says("ok", "Streaming");
    r.pump();
    assert_eq!(r.shown().as_deref(), Some("oneplus"));
    assert_eq!(loaded(&r, "panel")["zoom"], 2);
    assert_eq!(loaded(&r, "microphone")["gain"], 1, "the mic stays with the first stream");

    r.link.hold_wakes(true);
    r.w.with_flow(|flow, host| flow.add_stream(host, "third"));
    *r.link.picked.lock().unwrap() = Some("fourth".into());
    let shown = r.shown();
    assert_eq!(loaded(&r, "panel"), json!({}), "the panels went to the phone waking, which has no settings yet");
    r.w.state.borrow_mut().selected = shown;
    r.w.state.borrow_mut().effects.push(Effect::PickSource(Some("fourth".into())));
    r.w.process();
    r.link.hold_wakes(false);
    r.until("the picked phone streams", |r| r.streaming() == ["fourth"]);

    assert_eq!(saved(&r, "oneplus", "panel")["zoom"], 2);
    assert_eq!(saved(&r, "pixel", "microphone")["gain"], 1, "stopping saved the mic under its stream, before the new phone's settings came");
    assert_eq!(loaded(&r, "panel")["zoom"], 4);
    assert_eq!(loaded(&r, "microphone")["gain"], 4);
    assert_eq!(r.w.state.borrow().file.load()["selected_device"], "fourth");
}
