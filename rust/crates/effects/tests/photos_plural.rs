//! Its own program, since it loads a language for the whole process.

use serde_json::json;
use telescope_core::event::Event;
use telescope_core::host::RequestId;
use telescope_core::plugin::Plugin;
use telescope_core::testing::FakeHost;
use telescope_core::ui::UiEvent;
use telescope_effects::photos::PhotosPlugin;

/// "Saved {name} and {n} more" has plural forms: Russian says снимок, снимка or снимков by the number.
#[test]
fn saved_photos_are_counted_in_the_languages_plural_form() {
    telescope_core::i18n::use_folder(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../locale"));
    assert_eq!(telescope_core::i18n::load(Some("ru")), "ru");
    let (mut plugin, mut host) = (PhotosPlugin::new(), FakeHost::new());
    host.still_caps = Some(json!({"formats": ["jpeg"], "burst_max": 1}));
    host.streaming = true;
    plugin.setup(&mut host);
    plugin.on_event(&mut host, &Event::PhonesChanged { count: 1 });
    plugin.on_event(&mut host, &Event::PhoneStateUpdated { state: json!({}) });
    plugin.on_ui(&mut host, &UiEvent { ui: "card".into(), callback: "take-clicked".into(), args: Vec::new() });
    assert_eq!(host.stills_taken.len(), 1);
    let files = json!(["/tmp/a.jpg", "/tmp/b.jpg", "/tmp/c.jpg", "/tmp/d.jpg", "/tmp/e.jpg", "/tmp/f.jpg"]);
    for id in 0..8 {
        plugin.on_event(&mut host, &Event::RequestDone { request: RequestId(id), ok: true, message: String::new(), data: json!({"ok": true, "files": files}) });
    }
    assert!(host.calls.iter().any(|c| c.contains("a.jpg") && c.contains("5 снимков")), "{:?}", host.calls.iter().rev().take(4).collect::<Vec<_>>());
}
