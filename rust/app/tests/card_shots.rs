// Pictures of the eight redesigned cards in their main states, in both looks, for a look by eye:
// CARD_SHOTS=dir cargo nextest run -p telescope-app --test card_shots, each as NAME-new.png and NAME-classic.png at 2x (NAME-dot-* with the dot change mark).
use serde_json::{json, Value as Json};
use slint::ComponentHandle;
use slint_interpreter::Value;
use telescope_core::plugin::Plugin;
use telescope_core::ui::SlintSource;
use telescope_effects::desk::DeskPlugin;
use telescope_effects::framing::FramingPlugin;
use telescope_effects::name_tag::NameTagPlugin;
use telescope_effects::notes::NotesPlugin;
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
    shoot_marked(dir, name, card, props, false);
}

/// `dot`: the rows changed from their default are marked with a dot, not a lit icon (Theme.mark-dot).
fn shoot_marked(dir: &std::path::Path, name: &str, card: &(String, String, Json), props: &[Json], dot: bool) {
    offscreen::install();
    let (source, component, base) = card;
    let def = telescope_shell::compile::compile("card.slint", source, vec![], component).expect("the card compiles");
    for classic in [false, true] {
        let ui = def.create().unwrap();
        ui.set_global_property("Theme", "classic", Value::Bool(classic)).unwrap();
        ui.set_global_property("Theme", "mark-dot", Value::Bool(dot)).unwrap();
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
        pic.save_png(&dir.join(format!("{name}{}-{}.png", if dot { "-dot" } else { "" }, if classic { "classic" } else { "new" }))).unwrap();
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

static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The language the cards are drawn in from here on: the texts are translated as each card is compiled. Held while the test draws.
fn language(code: &str) -> std::sync::MutexGuard<'static, ()> {
    let held = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    telescope_core::i18n::use_folder(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../locale"));
    telescope_core::i18n::load(Some(code));
    held
}

fn out() -> Option<std::path::PathBuf> {
    let dir = std::path::PathBuf::from(std::env::var("CARD_SHOTS").ok()?);
    std::fs::create_dir_all(&dir).unwrap();
    Some(dir)
}

#[test]
fn camera_shots() {
    let Some(dir) = out() else { return };
    let _english = language("en");
    let camera = card_of(&mut CameraControl::new(), "camera");
    shoot(&dir, "camera-auto", &camera, &[camera_props()]);
    let manual = json!({
        "exposure-mode": 1, "iso-enabled": true, "shutter-enabled": true, "wb-mode": 1, "wb-enabled": true, "kelvin": 3800, "tint": -40,
        "tint-text": "-40", "kelvin-text": "3800 K", "focus-mode": 2, "focus-enabled": true, "focus-pos": 400, "focus-text": "1.2 m",
        "ae-comp": 3, "ae-text": "+0.5 EV", "torch": {"checked": true, "enabled": true, "tip": ""}, "noise-reduction": 2,
    });
    shoot(&dir, "camera-manual", &camera, &[camera_props(), manual.clone()]);
    shoot_marked(&dir, "camera-manual", &camera, &[camera_props(), manual], true);
    // The widest stops a dial shows, in the middle
    shoot(&dir, "camera-fast", &camera, &[camera_props(), json!({"exposure-mode": 1, "iso-enabled": true, "shutter-enabled": true,
        "iso-stops": ["3200", "6400", "12800"], "iso-index": 1, "shutter-stops": ["1/2000", "1/4000", "1/8000"], "shutter-index": 2})]);
    shoot(&dir, "camera-waiting", &camera, &[json!({
        "active": true, "lenses": [], "info": "", "lens-placeholder": "Start streaming to load lenses", "lens-columns": 3,
        "low-light": {"checked": false, "enabled": false, "tip": "Start streaming to use low light"},
    })]);
}

#[test]
fn phone_shots() {
    let Some(dir) = out() else { return };
    let _english = language("en");
    let phone = card_of(&mut MonitoringPlugin::new(), "streaming");
    shoot(&dir, "phone-streaming", &phone, &[json!({"active": true, "battery-level": 74, "battery-has-level": true, "battery-value": "74%",
        "battery-icon": "battery-charging", "temp-level": 38.0, "temp-has-level": true, "temp-value": "38 °C", "temp-note": "Cool"})]);
    shoot(&dir, "phone-empty", &phone, &[json!({"active": true})]);
}

#[test]
fn photos_shots() {
    let Some(dir) = out() else { return };
    let _english = language("en");
    let photos = card_of(&mut PhotosPlugin::new(), "camera");
    shoot(&dir, "photos-single", &photos, &[json!({"active": true, "shown": true, "take-enabled": true, "open-enabled": true,
        "mode-enabled": true, "format-enabled": true, "mode-options": ["Normal", "Night", "HDR"], "format-options": ["JPEG", "HEIC", "RAW (DNG)"],
        "shots-offered": [true, true, true, true], "distortion-enabled": true, "exposure-options": ["Like the video", "0.5 s", "1 s"], "best-enabled": true,
        "slot-key": "", "quality-enabled": true})]);
    shoot(&dir, "photos-burst", &photos, &[json!({"active": true, "shown": true, "take-enabled": false, "open-enabled": false, "status": "Saved IMG_20260101.jpg",
        "mode-enabled": true, "format-enabled": true, "mode-options": ["Normal", "Night", "HDR"], "format-options": ["JPEG", "HEIC", "RAW (DNG)"],
        "shots-offered": [true, true, true, true], "shots-index": 1, "slot-key": "burst", "slot-label": "How many", "slot-options": ["2", "3", "4", "5"],
        "slot-index": 3, "distortion-enabled": true, "distortion-index": 1, "exposure-options": ["Like the video", "0.5 s", "1 s"], "best-enabled": true, "best": true})]);
    // A DSLR: its own ISO and shutter lists as dials above, no quality
    shoot(&dir, "photos-dslr", &photos, &[json!({"active": true, "shown": true, "take-enabled": true, "iso-options": ["100", "200", "400", "800"], "iso-index": 2,
        "shutter-options": ["1/1000", "1/40", "30 s"], "shutter-index": 1, "format-enabled": true, "format-options": ["JPEG", "RAW (DNG)"], "quality-enabled": false,
        "quality-tip": "The camera keeps its own JPEG quality", "shots-offered": [true, false, false, false]})]);
    shoot(&dir, "photos-idle", &photos, &[json!({"active": true, "shown": true, "take-enabled": false})]);
}

#[test]
fn connection_shots() {
    let Some(dir) = out() else { return };
    let _english = language("en");
    let card = connection_card();
    shoot(&dir, "connection-checking", &card, &[json!({"status-text": "Checking", "status-kind": "status_dim", "route-shown": true})]);
    shoot(&dir, "connection-update", &card, &[json!({"status-text": "Phone app needs an update", "status-kind": "status_warn", "route-shown": true,
        "route-current": 1, "update-shown": true, "update-text": "Update over USB", "note-text": "The Telescope app on Pixel is older than this one."})]);
    shoot(&dir, "connection-streaming", &card, &[json!({"status-text": "Streaming over Wi-Fi", "status-kind": "", "route-shown": true, "route-current": 2,
        "using-shown": true, "using-text": "Wi-Fi \u{b7} 10.0.0.77"})]);
    shoot(&dir, "connection-none", &card, &[json!({"status-text": "No device yet", "status-kind": "status_dim"})]);
    shoot(&dir, "connection-german", &card, &[json!({"status-text": "Smartphone-App benötigt ein Update", "status-kind": "status_warn", "route-shown": true,
        "update-shown": true, "update-text": "Per USB aktualisieren"})]);
}

/// German, where the words run long: a stress test for text that is cut off.
#[test]
fn german_shots() {
    let Some(dir) = out() else { return };
    let _german = language("de");
    let camera = card_of(&mut CameraControl::new(), "camera");
    shoot(&dir, "de-camera", &camera, &[camera_props(), json!({"exposure-mode": 1, "iso-enabled": true, "shutter-enabled": true})]);
    let phone = card_of(&mut MonitoringPlugin::new(), "streaming");
    shoot(&dir, "de-phone", &phone, &[json!({"active": true, "battery-level": 74, "battery-has-level": true, "battery-value": "74 %",
        "battery-icon": "battery-charging", "temp-level": 38.0, "temp-has-level": true, "temp-value": "38 \u{b0}C", "batt-text": "20 %", "temp-text": "45 \u{b0}C"})]);
    let photos = card_of(&mut PhotosPlugin::new(), "camera");
    shoot(&dir, "de-photos", &photos, &[json!({"active": true, "shown": true, "take-enabled": true, "mode-enabled": true, "format-enabled": true,
        "mode-options": ["Normal", "Vollaufl\u{f6}sung"], "format-options": ["JPEG", "RAW und JPEG"], "shots-offered": [true, true, true, true],
        "slot-options": ["2", "3"], "exposure-options": ["Wie das Video", "0,5 s"], "distortion-enabled": true})]);
    let card = connection_card();
    shoot(&dir, "de-connection", &card, &[json!({"status-text": "Smartphone-App benötigt ein Update", "status-kind": "status_warn", "route-shown": true,
        "update-shown": true, "update-text": "\u{dc}ber USB aktualisieren"})]);
    let desk_de = card_of(&mut DeskPlugin::new(), "camera");
    shoot(&dir, "de-desk", &desk_de, &[json!({"on": true, "active": true, "clean-items": ["Aus", "Kontrast", "Schwarzweiss"], "shape-items": ["Automatisch", "A4", "Letter", "Quadratisch", "Füllen"], "stretch": [120, 90], "stretch-text": ["120 %", "90 %"], "reset-shown": true})]);
    let framing_de = card_of(&mut FramingPlugin::new(), "vision");
    shoot(&dir, "de-framing", &framing_de, &[json!({"on": true, "active": true, "framing": 20, "speed": 3, "room": 4, "framing-text": "Hüfte", "speed-text": "Gemütlich", "room-text": "Schritt", "fits": "Gemütlich\nSchwungvoll\nUmherstreifen"})]);
    let notes = card_of(&mut NotesPlugin::new(), "on_screen");
    shoot(&dir, "de-notes", &notes, &[json!({"active": true, "text": "Guten Morgen zusammen. Danke, dass ihr dabei seid.", "size": 80, "size-text": "80", "scroll": 1, "scroll-items": ["Von Hand", "Automatisch"],
        "show-auto": true, "send-text": "An das Telefon senden", "clear-text": "Vom Telefon entfernen", "send-enabled": true, "clear-enabled": true, "nav-enabled": true, "speed": 6, "speed-text": "6",
        "set-items": ["Notizen 1", "Notizen 2"]})]);
    let tag = card_of(&mut NameTagPlugin::new(), "on_screen");
    shoot(&dir, "de-nametag", &tag, &[json!({"on": true, "name": "Alexandra Rivera-Schmidt", "pronouns": "sie/ihr", "second": "Leiterin Produktentwicklung", "position-current": 2, "style-current": 3, "size-current": 2,
        "style-items": ["Leiste", "Kapsel", "Helle Box", "Schatten"], "position-items": ["Unten links", "Unten Mitte", "Unten rechts", "Oben links", "Oben rechts"], "size-items": ["Klein", "Mittel", "Gro\u{df}"]})]);
}

#[test]
fn desk_shots() {
    let Some(dir) = out() else { return };
    let _english = language("en");
    let desk = card_of(&mut DeskPlugin::new(), "camera");
    let on = json!({"on": true, "active": true});
    shoot(&dir, "desk-default", &desk, &[on.clone()]);
    shoot(&dir, "desk-changed", &desk, &[json!({"on": true, "active": true, "stretch": [120, 90], "stretch-text": ["120%", "90%"], "reset-shown": true, "clean-current": 1, "shape-current": 2})]);
    shoot_marked(&dir, "desk-changed", &desk, &[json!({"on": true, "active": true, "stretch": [120, 90], "stretch-text": ["120%", "90%"], "reset-shown": true, "clean-current": 1, "shape-current": 2})], true);
    shoot(&dir, "desk-picking", &desk, &[json!({"on": true, "active": true, "picking": true, "stretch": [100, 100]})]);
    shoot(&dir, "desk-fill", &desk, &[json!({"on": true, "active": true, "shape-current": 4, "stretch-enabled": false})]);
    shoot(&dir, "desk-unpaired", &desk, &[json!({"on": false, "active": false})]);
}

#[test]
fn framing_shots() {
    let Some(dir) = out() else { return };
    let _english = language("en");
    let framing = card_of(&mut FramingPlugin::new(), "vision");
    let on = json!({"on": true, "active": true});
    shoot(&dir, "framing-default", &framing, &[on.clone()]);
    shoot(&dir, "framing-changed", &framing, &[json!({"on": true, "active": true, "framing": 20, "speed": 8, "room": 4, "hand": true, "framing-text": "Waist", "speed-text": "Snappy", "room-text": "Step"})]);
    shoot_marked(&dir, "framing-changed", &framing, &[json!({"on": true, "active": true, "framing": 20, "speed": 8, "room": 4, "hand": true, "framing-text": "Waist", "speed-text": "Snappy", "room-text": "Step"})], true);
    shoot(&dir, "framing-unpaired", &framing, &[json!({"on": false, "active": false})]);
    shoot(&dir, "framing-status", &framing, &[json!({"on": true, "active": true, "vision-text": "Needs a one-time download (about 47 MB).", "vision-download": true})]);
}

#[test]
fn notes_shots() {
    let Some(dir) = out() else { return };
    let _english = language("en");
    let notes = card_of(&mut NotesPlugin::new(), "on_screen");
    shoot(&dir, "notes-empty", &notes, &[json!({"active": true})]);
    shoot(&dir, "notes-hand", &notes, &[json!({"active": true, "text": "Good morning, everyone. Thanks for joining the call, let's start with the plan for the quarter.\n\nThen questions.", "size": 80, "size-text": "80", "scroll": 1, "show-auto": false, "send-enabled": true, "clear-enabled": true, "nav-enabled": true})]);
    shoot_marked(&dir, "notes-hand", &notes, &[json!({"active": true, "text": "Good morning, everyone. Thanks for joining the call, let's start with the plan for the quarter.\n\nThen questions.", "size": 80, "size-text": "80", "scroll": 1, "show-auto": false, "send-enabled": true, "clear-enabled": true, "nav-enabled": true})], true);
    shoot(&dir, "notes-auto", &notes, &[json!({"active": true, "text": "Good morning, everyone.", "size-text": "44", "speed": 6, "speed-text": "6", "send-enabled": true, "clear-enabled": true, "nav-enabled": true, "playing": true, "play-text": "Pause", "set-items": ["Notes 1", "Notes 2"]})]);
    shoot(&dir, "notes-unpaired", &notes, &[json!({"active": false})]);
}

#[test]
fn name_tag_shots() {
    let Some(dir) = out() else { return };
    let _english = language("en");
    let tag = card_of(&mut NameTagPlugin::new(), "on_screen");
    shoot(&dir, "nametag-empty", &tag, &[json!({"on": false})]);
    shoot(&dir, "nametag-filled", &tag, &[json!({"on": true, "name": "Alex Rivera", "pronouns": "they/them", "second": "Designer", "position-current": 2, "style-current": 3, "size-current": 2})]);
    shoot_marked(&dir, "nametag-filled", &tag, &[json!({"on": true, "name": "Alex Rivera", "pronouns": "they/them", "second": "Designer", "position-current": 2, "style-current": 3, "size-current": 2})], true);
    shoot(&dir, "nametag-pill", &tag, &[json!({"on": true, "name": "Alex Rivera", "pronouns": "", "second": "", "position-current": 3, "style-current": 1, "size-current": 0})]);
}

/// Start and Stop streaming in both looks, drawn at 2x, and the same size in both states.
#[test]
fn start_stop_shots() {
    let Some(dir) = out() else { return };
    let _english = language("en");
    offscreen::install();
    let source = r#"
import { StartButton } from "shell/frame.slint";
import { Theme } from "@ui-kit/ui-kit.slint";
export { Theme }
export component W inherits Window {
    in property <bool> streaming;
    in property <string> label;
    width: 240px;
    height: 56px;
    background: Theme.chrome;
    out property <length> button-width: b.width;
    out property <length> button-height: b.height;
    b := StartButton { x: 20px; y: 20px; text: label; streaming: streaming; }
}
"#;
    let def = telescope_shell::compile::compile("start.slint", source, vec![], "W").expect("the button compiles");
    for classic in [false, true] {
        let mut sizes = Vec::new();
        for streaming in [false, true] {
            let ui = def.create().unwrap();
            ui.set_global_property("Theme", "classic", Value::Bool(classic)).unwrap();
            ui.set_property("streaming", Value::Bool(streaming)).unwrap();
            ui.set_property("label", Value::String(if streaming { "Stop streaming" } else { "Start streaming" }.into())).unwrap();
            ui.show().unwrap();
            offscreen::pump();
            let get = |n: &str| match ui.get_property(n) {
                Ok(Value::Number(v)) => v,
                other => panic!("{n}: {other:?}"),
            };
            sizes.push((get("button-width"), get("button-height")));
            let pic = offscreen::draw(ui.window(), 240.0, 56.0, 2.0).expect("a picture");
            pic.save_png(&dir.join(format!("start-{}-{}.png", if streaming { "stop" } else { "go" }, if classic { "classic" } else { "new" }))).unwrap();
            ui.hide().unwrap();
        }
        assert_eq!(sizes[0], sizes[1], "Start and Stop are the same size (classic: {classic})");
    }
}
