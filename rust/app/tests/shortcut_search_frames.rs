// Typing in the shortcut editor's search: no frame may show the dialog in a state it settles out of a moment later.
mod rig;

use rig::Rig;
use serde_json::json;
use slint::platform::WindowEvent;
use slint::ComponentHandle;
use telescope_core::plugin::{Plugin, PluginInfo};
use telescope_core::ui::ActionSpec;
use telescope_shell::{offscreen, settings};
use telescope_system::shortcuts::{RemoteActions, ShortcutsPlugin};

/// Controls with none, one and two settings, so a search changes how many rows the dialog has.
struct Controls;

impl Plugin for Controls {
    fn info(&self) -> PluginInfo {
        PluginInfo { name: "controls".into(), ..Default::default() }
    }
    fn actions(&mut self) -> Vec<ActionSpec> {
        let number = |name: &str| json!({"name": name, "label": name, "kind": "number", "default": 1, "minimum": 0, "maximum": 10, "decimals": 0, "suffix": ""});
        let action = |id: &str, label: &str, params: Vec<serde_json::Value>| ActionSpec {
            id: format!("controls.{id}"), label: label.into(), group: "Camera".into(), params, repeats: false, on_release: false, current: None, keywords: String::new(),
        };
        vec![
            action("alpha", "Alpha torch", vec![]),
            action("bravo", "Bravo zoom", vec![number("Amount")]),
            action("carol", "Carol focus", vec![number("Amount"), number("Speed")]),
            action("delta", "Delta exposure", vec![number("Amount")]),
            action("echo", "Echo mute", vec![]),
        ]
    }
}

const WIDTH: f32 = 860.0;
const HEIGHT: f32 = 640.0;

#[test]
fn typing_in_the_search_never_shows_a_frame_that_settles_differently() {
    let rig = Rig::with("search-frames", |_, _| {
        vec![Box::new(Controls) as Box<dyn Plugin>, Box::new(ShortcutsPlugin::with_backend(RemoteActions::default(), Box::new(|_| None)))]
    });
    settings::forget();
    settings::open(&rig.w, "shortcuts");
    let win = settings::current().expect("the settings window");
    assert!(win.call("shortcuts", "shortcuts", "add"));
    rig.w.process();
    offscreen::draw(win.ui.window(), WIDTH, HEIGHT, 1.0);
    // The dialog pops in over a moment: let it get there
    offscreen::skip(std::time::Duration::from_millis(500));
    offscreen::draw(win.ui.window(), WIDTH, HEIGHT, 1.0);
    // A plain generator: the same letters each run, none of them special
    let mut seed = 0x2545_f491u32;
    let mut letter = || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        ["a", "b", "c", "d", "e", "l", "m", "o", "r", "t", "u", "x", "z"][seed as usize % 13]
    };
    let mut found = Vec::new();
    for at in 0..120 {
        let key = letter();
        win.ui.window().dispatch_event(WindowEvent::KeyPressed { text: key.into() });
        win.ui.window().dispatch_event(WindowEvent::KeyReleased { text: key.into() });
        rig.w.process();
        let first = offscreen::frame(WIDTH, HEIGHT, 1.0).expect("a frame");
        let settled = offscreen::draw(win.ui.window(), WIDTH, HEIGHT, 1.0).expect("a frame");
        let differing = first.rgba.chunks(4).zip(settled.rgba.chunks(4)).filter(|(a, b)| a != b).count();
        // The caret is what's allowed to differ
        if differing > 40 {
            found.push((at, key, differing));
            if let Some(folder) = std::env::var_os("TELESCOPE_KEEP_FRAMES").map(std::path::PathBuf::from) {
                let _ = first.save_png(&folder.join(format!("search-{at}-first.png")));
                let _ = settled.save_png(&folder.join(format!("search-{at}-settled.png")));
            }
        }
        // Long words find nothing, so it starts over now and then
        if at % 6 == 5 {
            win.ui.window().dispatch_event(WindowEvent::KeyPressed { text: slint::platform::Key::Escape.into() });
            win.ui.window().dispatch_event(WindowEvent::KeyReleased { text: slint::platform::Key::Escape.into() });
            rig.w.process();
            offscreen::draw(win.ui.window(), WIDTH, HEIGHT, 1.0);
        }
    }
    settings::forget();
    assert!(found.is_empty(), "frames that didn't match what they settled into (key number, key, pixels): {found:?}");
}
