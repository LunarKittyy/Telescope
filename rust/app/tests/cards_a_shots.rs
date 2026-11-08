// Pictures of the Connection, Camera, Phone and Photos cards in their main states, in both looks, for a look by eye:
// SHOTS=dir cargo test -p telescope-app --test cards_a_shots, each as NAME-new.png and NAME-classic.png at 2x.
use serde_json::{json, Value as Json};
use slint::ComponentHandle;
use slint_interpreter::Value;
use telescope_core::plugin::Plugin;
use telescope_core::ui::SlintSource;
use telescope_effects::photos::PhotosPlugin;
use telescope_setup::monitoring::MonitoringPlugin;
use telescope_shell::offscreen;
use telescope_shell::plugins::camera_control::CameraControl;
use telescope_shell::source::to_property;

const WIDTH: f32 = 406.0;

/// A card's source with the Theme global let out, so a picture can be asked for in either look.
fn card_of(plugin: &mut dyn Plugin, group: &str) -> (String, String, Json) {
    let card = plugin.card().expect("a card").ui;
    let SlintSource::Text(source) = card.source else { panic!("source text") };
    // Its natural height, which the window would give it, is read from a property added at its top
    let head = format!("export component {} inherits", card.component);
    let at = source.find(&head).expect("the card's component");
    let open = at + source[at..].find('{').unwrap() + 1;
    let source = format!("{}\n    out property <length> shot-height: root.preferred-height;\n    hue: Theme.plugin-hue(\"{group}\");{}\nexport {{ Theme }}", &source[..open], &source[open..]);
    (source, card.component, card.props)
}

fn shoot(dir: &std::path::Path, name: &str, card: &(String, String, Json), props: &[Json]) {
    offscreen::install();
    let (source, component, base) = card;
    let def = telescope_shell::compile::compile("card.slint", source, vec![], component).expect("the card compiles");
    for classic in [false, true] {
        let ui = def.create().unwrap();
        ui.set_global_property("Theme", "classic", Value::Bool(classic)).unwrap();
        for part in std::iter::once(base).chain(props) {
            for (key, value) in part.as_object().unwrap() {
                ui.set_property(key, to_property(&ui, key, value)).unwrap_or_else(|e| panic!("{key}: {e:?}"));
            }
        }
        ui.window().set_size(slint::LogicalSize::new(WIDTH, 2000.0));
        ui.show().unwrap();
        offscreen::pump();
        let height = match ui.get_property("shot-height") {
            Ok(Value::Number(h)) => h as f32,
            other => panic!("no preferred height: {other:?}"),
        };
        let pic = offscreen::draw(ui.window(), WIDTH, height.ceil(), 2.0).expect("a picture");
        pic.save_png(&dir.join(format!("{name}-{}.png", if classic { "classic" } else { "new" }))).unwrap();
        ui.hide().unwrap();
    }
}

fn camera_props() -> Json {
    json!({
        "active": true,
        "lenses": [
            {"id": "wide", "number": "0.6", "unit": "x", "rest": "Wide", "name": "Wide"},
            {"id": "main", "number": "1", "unit": "x", "rest": "Main", "name": "Main"},
            {"id": "tele", "number": "2", "unit": "x", "rest": "Zoom", "name": "Zoom"},
        ],
        "current-lens": 1, "lens-columns": 3, "lens-placeholder": "", "info": "Full manual control",
        "iso-stops": ["100", "125", "160", "200", "250", "320", "400", "500", "640", "800", "1000"], "iso-index": 6,
        "shutter-stops": ["1/15", "1/20", "1/25", "1/30", "1/40", "1/50", "1/60", "1/80", "1/100", "1/125", "1/160"], "shutter-index": 6,
        "ae-comp": 0, "ae-text": "+0.0 EV", "kelvin": 5500, "kelvin-text": "5500 K", "tint": 10, "tint-text": "+10",
        "focus-disabled": [false, false, false], "focus-pos": 1000, "focus-text": "∞",
        "ois": {"checked": true, "enabled": true, "tip": ""}, "low-light": {"checked": false, "enabled": true, "tip": ""},
        "black-level-lock": {"checked": false, "enabled": true, "tip": ""}, "torch": {"checked": false, "enabled": true, "tip": ""},
        "noise-reduction": 1, "sharpening": 1,
    })
}

/// The Connection card has no plugin to ask for it here, so it is taken from its file.
fn connection_card() -> (String, String, Json) {
    let source = include_str!("../../crates/link/ui/connection.slint");
    // Only the last component a file exports can be asked for by name, so the card is cut out of it
    let source = &source[..source.find("// The phone picker and the Your phones button").unwrap()];
    let head = "export component ConnectionCard inherits Card {";
    let at = source.find(head).unwrap() + head.len();
    let source = format!("{}\n    out property <length> shot-height: root.preferred-height;\n    hue: Theme.plugin-hue(\"streaming\");{}\nexport {{ Theme }}", &source[..at], &source[at..]);
    (source, "ConnectionCard".into(), json!({}))
}

fn out() -> Option<std::path::PathBuf> {
    let dir = std::path::PathBuf::from(std::env::var("SHOTS").ok()?);
    std::fs::create_dir_all(&dir).unwrap();
    Some(dir)
}

#[test]
fn camera_shots() {
    let Some(dir) = out() else { return };
    let camera = card_of(&mut CameraControl::new(), "camera");
    shoot(&dir, "camera-auto", &camera, &[camera_props()]);
    shoot(&dir, "camera-manual", &camera, &[camera_props(), json!({
        "exposure-mode": 1, "iso-enabled": true, "shutter-enabled": true, "wb-mode": 1, "wb-enabled": true, "kelvin": 3800, "tint": -40,
        "tint-text": "-40", "kelvin-text": "3800 K", "focus-mode": 2, "focus-enabled": true, "focus-pos": 400, "focus-text": "1.2 m",
        "ae-comp": 3, "ae-text": "+0.5 EV", "torch": {"checked": true, "enabled": true, "tip": ""}, "noise-reduction": 2,
    })]);
    shoot(&dir, "camera-waiting", &camera, &[json!({
        "active": true, "lenses": [], "info": "", "lens-placeholder": "Start streaming to load lenses", "lens-columns": 3,
        "low-light": {"checked": false, "enabled": false, "tip": "Start streaming to use low light"},
    })]);

}

#[test]
fn phone_shots() {
    let Some(dir) = out() else { return };
    let phone = card_of(&mut MonitoringPlugin::new(), "streaming");
    shoot(&dir, "phone-streaming", &phone, &[json!({"active": true, "battery-level": 74, "battery-has-level": true, "battery-value": "74%",
        "battery-icon": "battery-charging", "temp-level": 38.0, "temp-has-level": true, "temp-value": "38 °C", "temp-note": "Cool"})]);
    shoot(&dir, "phone-empty", &phone, &[json!({"active": true})]);

}

#[test]
fn photos_shots() {
    let Some(dir) = out() else { return };
    let photos = card_of(&mut PhotosPlugin::new(), "camera");
    shoot(&dir, "photos-single", &photos, &[json!({"active": true, "shown": true, "take-enabled": true, "open-enabled": true,
        "mode-enabled": true, "format-enabled": true, "mode-options": ["Normal", "Night", "HDR"], "format-options": ["JPEG", "HEIC", "RAW (DNG)"],
        "shots-offered": [true, true, true, true], "distortion-enabled": true, "exposure-options": ["Like the video", "0.5 s", "1 s"], "best-enabled": true,
        "slot-key": "", "quality-enabled": true})]);
    shoot(&dir, "photos-burst", &photos, &[json!({"active": true, "shown": true, "take-enabled": false, "open-enabled": false, "status": "Saved IMG_20260101.jpg",
        "mode-enabled": true, "format-enabled": true, "mode-options": ["Normal", "Night", "HDR"], "format-options": ["JPEG", "HEIC", "RAW (DNG)"],
        "shots-offered": [true, true, true, true], "shots-index": 1, "slot-key": "burst", "slot-label": "How many", "slot-options": ["2", "3", "4", "5"],
        "slot-index": 3, "distortion-enabled": true, "distortion-index": 1, "exposure-options": ["Like the video", "0.5 s", "1 s"], "best-enabled": true, "best": true})]);
    shoot(&dir, "photos-idle", &photos, &[json!({"active": true, "shown": true, "take-enabled": false})]);
}

#[test]
fn connection_shots() {
    let Some(dir) = out() else { return };
    let card = connection_card();
    shoot(&dir, "connection-checking", &card, &[json!({"status-text": "Checking", "status-kind": "status_dim", "route-shown": true})]);
    shoot(&dir, "connection-update", &card, &[json!({"status-text": "Phone app needs an update", "status-kind": "status_warn", "route-shown": true,
        "route-current": 1, "update-shown": true, "update-text": "Update over USB", "note-text": "The Telescope app on Pixel is older than this one."})]);
    shoot(&dir, "connection-streaming", &card, &[json!({"status-text": "Streaming over Wi-Fi", "status-kind": "", "route-shown": true, "route-current": 2,
        "using-shown": true, "using-text": "Wi-Fi \u{b7} 10.0.0.77"})]);
    shoot(&dir, "connection-none", &card, &[json!({"status-text": "No device yet", "status-kind": "status_dim"})]);
    shoot(&dir, "connection-german", &card, &[json!({"status-text": "Die Telescope-App auf dem Telefon braucht eine Aktualisierung", "status-kind": "status_warn", "route-shown": true,
        "update-shown": true, "update-text": "Per USB aktualisieren"})]);
}
