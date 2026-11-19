// The Spout send card is in the window once the plugin is on, and the plugin can switch it off and on like any card.
mod rig;

use rig::{Listener, Rig};
use serde_json::json;
use telescope_core::event::Event;
use telescope_core::plugin::Plugin;
use telescope_outputs::spout_send::SpoutSendPlugin;

fn rig(name: &str) -> Rig {
    Rig::seeded(name, json!({"plugins": {"on": ["spout_send"]}}), |_, heard| vec![Box::new(Listener::new("panel", heard)) as Box<dyn Plugin>, Box::new(SpoutSendPlugin::new(None))])
}

#[test]
fn the_card_is_there_for_the_stream_in_focus() {
    let r = rig("spout-send-card");
    r.start();
    r.pump();
    assert!(!r.w.card_shown("spout_send"), "nothing to share before a stream is picked");
    r.emit(Event::SourceSelected { source_id: "pixel".into() });
    assert!(r.w.card_shown("spout_send"), "a Spout send card in the window");
}

#[test]
fn the_plugin_can_switch_its_card_off() {
    let r = rig("spout-send-card-enabled");
    for prop in ["enabled", "blocked", "idle"] {
        assert!(r.w.set_card_property("spout_send", "card", prop, &json!(prop != "enabled")), "the card has {prop}");
    }
}
