// Pictures of what round 1 changed, in both looks, for a look by eye:
// CARD_SHOTS=dir cargo nextest run -p telescope-app --test r1_shots, each as NAME-new.png and NAME-classic.png at 2x.
use i_slint_backend_testing::ElementHandle;
use serde_json::{json, Value as Json};
use slint::platform::{PointerEventButton, WindowEvent};
use slint::ComponentHandle;
use slint_interpreter::{Compiler, Value};
use std::path::{Path, PathBuf};
use telescope_core::plugin::Plugin;
use telescope_core::ui::SlintSource;
use telescope_effects::chroma_key::ChromaKeyPlugin;
use telescope_effects::colour_grading::ColourGradingPlugin;
use telescope_effects::desk::DeskPlugin;
use telescope_effects::transforms::TransformsPlugin;
use telescope_shell::offscreen;
use telescope_shell::source::to_property;

fn out() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var("CARD_SHOTS").ok()?);
    std::fs::create_dir_all(&dir).unwrap();
    Some(dir)
}

fn spin_on<F: std::future::Future>(f: F) -> F::Output {
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    let mut f = std::pin::pin!(f);
    loop {
        if let std::task::Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
    }
}

/// A plugin's card source with the Theme global let out and its natural height readable.
fn card_of(plugin: &mut dyn Plugin, group: &str) -> (String, String, Json) {
    let card = plugin.card().expect("a card").ui;
    let SlintSource::Text(source) = card.source else { panic!("source text") };
    let head = format!("export component {} inherits", card.component);
    let at = source.find(&head).expect("the card's component");
    let open = at + source[at..].find('{').unwrap() + 1;
    let source = format!("{}\n    out property <length> shot-height: root.preferred-height;\n    hue: Theme.plugin-hue(\"{group}\");{}\nexport {{ Theme }}", &source[..open], &source[open..]);
    (source, card.component, card.props)
}

type Card = (String, String, Json);
type Then<'a> = &'a dyn Fn(&slint_interpreter::ComponentInstance);

/// The card in both looks at `width` px, `then` run on it once shown (a click, say), 2x.
fn shoot(dir: &Path, name: &str, card: &Card, width: f32, props: &[Json], then: Then) {
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
        ui.window().set_size(slint::LogicalSize::new(width, 2400.0));
        ui.show().unwrap();
        offscreen::pump();
        let height = match ui.get_property("shot-height") {
            Ok(Value::Number(h)) => h as f32,
            other => panic!("no preferred height: {other:?}"),
        };
        // Room below for a bubble that opens out of the card
        let room = if name.contains("picker") { 470.0 } else { 40.0 };
        ui.window().set_size(slint::LogicalSize::new(width, height.ceil() + room));
        then(&ui);
        offscreen::pump();
        let pic = offscreen::draw(ui.window(), width, height.ceil() + room, 2.0).expect("a picture");
        pic.save_png(&dir.join(format!("{name}-{}.png", if classic { "classic" } else { "new" }))).unwrap();
        ui.hide().unwrap();
    }
}

fn nothing(_: &slint_interpreter::ComponentInstance) {}

fn click(ui: &slint_interpreter::ComponentInstance, e: &ElementHandle) {
    let (p, s) = (e.absolute_position(), e.size());
    let position = slint::LogicalPosition::new(p.x + s.width / 2.0, p.y + s.height / 2.0);
    ui.window().dispatch_event(WindowEvent::PointerMoved { position });
    ui.window().dispatch_event(WindowEvent::PointerPressed { position, button: PointerEventButton::Left });
    ui.window().dispatch_event(WindowEvent::PointerReleased { position, button: PointerEventButton::Left });
    offscreen::pump();
}

/// A card straight from its own .slint file (no plugin around it), as row_shots does.
fn file_card(dir: &Path, name: &str, file: &str, component: &str, props: &str, size: (f32, f32)) {
    offscreen::install();
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join(file);
    let doc = format!(
        "import {{ {component} }} from \"{}\";\nimport {{ Theme }} from \"@ui-kit/ui-kit.slint\";\nexport {{ Theme }}\nexport component W inherits Window {{\n    width: {}px;\n    height: {}px;\n    background: Theme.ground;\n    {component} {{ x: {}px; y: {}px; width: parent.width - {}px; {} {props} }}\n}}",
        path.display(),
        size.0,
        size.1,
        if component == "Footer" { 0 } else { 16 },
        if component == "Footer" { 8 } else { 16 },
        if component == "Footer" { 0 } else { 32 },
        if component == "Footer" { String::new() } else {
            // As tall as the card is, so the window's spare height doesn't spread its rows
            let fit = if props.contains("height:") { "" } else { "height: self.preferred-height;" };
            let hue = if file.contains("microphone") { "sound" } else { "streaming" };
            format!("{fit} hue: Theme.plugin-hue(\"{hue}\");")
        }
    );
    let mut compiler = Compiler::default();
    compiler.set_library_paths([(telescope_ui_kit::LIBRARY.to_string(), telescope_ui_kit::library_path())].into_iter().collect());
    let result = spin_on(compiler.build_from_source(doc, PathBuf::from("/root/wrapper.slint")));
    let errors: Vec<String> = result.diagnostics().filter(|d| d.level() == slint_interpreter::DiagnosticLevel::Error).map(|d| d.to_string()).collect();
    assert!(errors.is_empty(), "{}", errors.join("\n"));
    let def = result.component("W").unwrap();
    for classic in [false, true] {
        let w = def.create().unwrap();
        w.set_global_property("Theme", "classic", Value::Bool(classic)).unwrap();
        w.show().unwrap();
        offscreen::pump();
        let pic = offscreen::draw(w.window(), size.0, size.1, 2.0).expect("a picture");
        pic.save_png(&dir.join(format!("{name}-{}.png", if classic { "classic" } else { "new" }))).unwrap();
    }
}

#[test]
fn transforms_and_grading_shots() {
    let Some(dir) = out() else { return };
    let card = card_of(&mut TransformsPlugin::new(), "camera");
    let zoomed = json!({"active": true, "zoom": 2.5, "pan-x": 0.4, "pan-y": -0.3, "pan-enabled": true, "zoom-text": "2.5\u{d7}"});
    shoot(&dir, "transforms-rest", &card, 406.0, &[json!({"active": true})], &nothing);
    shoot(&dir, "transforms-zoomed", &card, 406.0, &[zoomed], &nothing);
    shoot(&dir, "transforms-flipped", &card, 406.0, &[json!({"active": true, "flips": [true, false], "rotation": 90})], &nothing);
    let grading = card_of(&mut ColourGradingPlugin::new(), "look");
    shoot(&dir, "grading-compare", &grading, 406.0, &[json!({"active": true, "on": true, "compare": true, "split": 40, "split-text": "40%"})], &nothing);
    shoot(&dir, "grading-lut", &grading, 406.0, &[json!({"active": true, "on": true, "page": 2})], &nothing);
}

#[test]
fn desk_shots() {
    let Some(dir) = out() else { return };
    let card = card_of(&mut DeskPlugin::new(), "camera");
    let props = json!({"active": true, "clean-items": ["Off", "Contrast", "Black and white"], "clean-current": 1, "shape-items": ["Auto", "A4", "Letter", "Square", "Fill"], "shape-current": 0});
    shoot(&dir, "desk", &card, 406.0, &[props], &nothing);
}

#[test]
fn chroma_key_picker_shots() {
    let Some(dir) = out() else { return };
    let card = card_of(&mut ChromaKeyPlugin::new(), "look");
    let base = json!({"active": true, "on": true, "behind": 0, "colour-hex": "#1e2228", "channel-value": [30, 34, 40], "colour-h": 0.6, "colour-s": 0.25, "colour-v": 0.16});
    shoot(&dir, "chroma-card", &card, 406.0, &[base.clone()], &nothing);
    let open = |ui: &slint_interpreter::ComponentInstance| {
        let field = ElementHandle::find_by_element_type_name(ui, "ColourField").next().expect("the colour field");
        click(ui, &field);
    };
    shoot(&dir, "chroma-picker", &card, 406.0, &[base.clone()], &open);
    let values = |ui: &slint_interpreter::ComponentInstance| {
        open(ui);
        let sw = ElementHandle::find_by_accessible_label(ui, "Values").find(|e| e.accessible_checkable() == Some(true)).expect("the values switch");
        click(ui, &sw);
        // let the unfold run
        for _ in 0..30 {
            offscreen::pump();
            std::thread::sleep(std::time::Duration::from_millis(16));
        }
    };
    shoot(&dir, "chroma-picker-values", &card, 406.0, &[base], &values);
}

#[test]
fn file_card_shots() {
    let Some(dir) = out() else { return };
    let missing = "RNNoise is missing from this copy of Telescope. Reinstalling brings it back.";
    let mic = "active: true; here: true; on: true; nr-enabled: false; gain: 3; gain-text: \"+3 dB\"; meter: { peak: 0.5, rms: 0.35, hold: 0.62, lit: \"\", muted: false, readout: \"-18 dB\", red: false };";
    file_card(&dir, "mic-rnnoise-missing", "crates/audio/ui/microphone.slint", "MicrophoneCard", &format!("{mic} nr-note: \"{missing}\";"), (420.0, 330.0));
    file_card(&dir, "mic-plain", "crates/audio/ui/microphone.slint", "MicrophoneCard", &format!("{mic} nr: true; auto-level: true;"), (420.0, 330.0));
    let stream = "active: true; res-items: [\"1080p\", \"720p\"]; res-enabled: true; fps-items: [\"30 fps\", \"60 fps\"]; fps-gone: [false, false]; ar-items: [\"16:9\", \"4:3\"]; ar-enabled: true; bitrate: 20; bitrate-readout: \"24 Mbps\"; quality-readout: \"85\"; light-tip: \"H.264\"; heavy-tip: \"MJPEG\";";
    file_card(&dir, "stream-light", "crates/outputs/ui/stream_output.slint", "StreamOutputCard", stream, (420.0, 380.0));
    file_card(&dir, "stream-heavy", "crates/outputs/ui/stream_output.slint", "StreamOutputCard", &format!("{stream} h264: false;"), (420.0, 380.0));
    let (cells, modules) = telescope_core::qr::encode("https://192.168.1.20:8767/#t1");
    let cells = cells.iter().map(|c| c.to_string()).collect::<Vec<_>>().join(", ");
    let browser = format!("shown: true; new-link-enabled: true; url: \"https://192.168.1.20:8767/#t1\"; url-short: \"192.168.1.20:8767\"; qr-cells: [{cells}]; qr-size: {modules}; note-title: \"Browser says it isn't private?\"; note-text: \"x\"; size-index: 1; fps-index: 2;");
    file_card(&dir, "browser-waiting", "crates/link/ui/browser_camera.slint", "BrowserCameraCard", &format!("{browser} status-text: \"Waiting for a browser\";"), (420.0, 520.0));
    file_card(&dir, "browser-connected", "crates/link/ui/browser_camera.slint", "BrowserCameraCard", &format!("{browser} status-text: \"Pixel 8 \u{b7} 720p 30 fps\"; status-kind: \"status_ok\"; addresses: [\"192.168.1.20\", \"10.0.0.4\"]; show-addresses: true; address-index: 0;"), (420.0, 520.0));
    // The preview card from its own file, its import of the window's marks made absolute
    let file = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../crates/shell/ui/cards/preview.slint");
    let marks = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../crates/shell/ui/preview.slint");
    let text = std::fs::read_to_string(file).unwrap().replace("\"../../shell/preview.slint\"", &format!("\"{}\"", marks.display()));
    let head = "export component PreviewCard inherits Rectangle {";
    let at = text.find(head).unwrap() + head.len();
    let source = format!("{}\n    out property <length> shot-height: 340px;\n    height: 340px;{}\nexport {{ Theme }}", &text[..at], &text[at..]);
    let preview: Card = (source, "PreviewCard".into(), json!({}));
    let chin = json!({"stage-text": "Camera off", "camera-off": true, "camera-on": false, "camera-enabled": true, "toggle-text": "Hide", "skeleton-shown": true, "skeleton-on": true, "lenses-enabled": true, "smooth-on": true});
    shoot(&dir, "preview-camera-off", &preview, 520.0, &[chin], &nothing);
    let idle = json!({"stage-text": "Start streaming to see the picture", "camera-on": true, "camera-enabled": true, "toggle-text": "Hide", "lenses-enabled": true});
    shoot(&dir, "preview-idle", &preview, 520.0, &[idle], &nothing);
    // The bar under the window, streaming and not
    file_card(&dir, "footer-streaming", "crates/shell/ui/frame.slint", "Footer", "status: \"Streaming 1920 \u{d7} 1080 at 30 fps to Phone Camera\"; camera: \"Phone Camera\"; size: \"1920 \u{d7} 1080\"; fps: \"30\"; net: \"12.4 Mbps\";", (900.0, 56.0));
    file_card(&dir, "footer-slow", "crates/shell/ui/frame.slint", "Footer", "status: \"Streaming 1920 \u{d7} 1080 at 30 fps to Phone Camera\"; camera: \"Phone Camera\"; size: \"1920 \u{d7} 1080\"; fps: \"18\"; fps-color: Theme.warn; net: \"3.1 Mbps\"; net-color: Theme.warn;", (900.0, 56.0));
    file_card(&dir, "footer-idle", "crates/shell/ui/frame.slint", "Footer", "status: \"Not streaming\"; fps: \"-\"; net: \"-\";", (900.0, 56.0));
}

/// The preview next to a card, the way the main window sets them: the same radius, no line round either, one tone step apart.
#[test]
fn preview_beside_a_card_shot() {
    let Some(dir) = out() else { return };
    offscreen::install();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    let marks = root.join("crates/shell/ui/preview.slint");
    let text = std::fs::read_to_string(root.join("crates/shell/ui/cards/preview.slint")).unwrap().replace("\"../../shell/preview.slint\"", &format!("\"{}\"", marks.display()));
    let stage = dir.join("preview_for_shot.slint");
    std::fs::write(&stage, text).unwrap();
    let mic = root.join("crates/audio/ui/microphone.slint");
    let doc = format!(
        "import {{ PreviewCard }} from \"{}\";\nimport {{ MicrophoneCard }} from \"{}\";\nimport {{ Theme }} from \"@ui-kit/ui-kit.slint\";\nexport {{ Theme }}\nexport component W inherits Window {{\n    width: 900px;\n    height: 380px;\n    background: Theme.ground;\n    PreviewCard {{ x: 16px; y: 16px; width: 520px; height: 340px; stage-text: \"Camera off\"; camera-off: true; camera-on: false; camera-enabled: true; toggle-text: \"Hide\"; skeleton-shown: true; lenses-enabled: true; hue: Theme.plugin-hue(\"preview\"); }}\n    MicrophoneCard {{ x: 552px; y: 16px; width: 332px; hue: Theme.plugin-hue(\"sound\"); active: true; here: true; on: true; gain-text: \"0 dB\"; meter: {{ peak: 0.5, rms: 0.35, hold: 0.62, lit: \"\", muted: false, readout: \"-18 dB\", red: false }}; }}\n}}",
        stage.display(),
        mic.display()
    );
    let mut compiler = Compiler::default();
    compiler.set_library_paths([(telescope_ui_kit::LIBRARY.to_string(), telescope_ui_kit::library_path())].into_iter().collect());
    let result = spin_on(compiler.build_from_source(doc, PathBuf::from("/root/wrapper.slint")));
    let errors: Vec<String> = result.diagnostics().filter(|d| d.level() == slint_interpreter::DiagnosticLevel::Error).map(|d| d.to_string()).collect();
    assert!(errors.is_empty(), "{}", errors.join("\n"));
    let def = result.component("W").unwrap();
    for classic in [false, true] {
        let w = def.create().unwrap();
        w.set_global_property("Theme", "classic", Value::Bool(classic)).unwrap();
        w.show().unwrap();
        offscreen::pump();
        let pic = offscreen::draw(w.window(), 900.0, 380.0, 2.0).expect("a picture");
        pic.save_png(&dir.join(format!("preview-beside-card-{}.png", if classic { "classic" } else { "new" }))).unwrap();
    }
    let _ = std::fs::remove_file(stage);
}

/// A few of the approved tooltips as the kit draws them: a card header, a slider with a hint, a title-only one, a disabled reason.
#[test]
fn tooltip_shots() {
    let Some(dir) = out() else { return };
    offscreen::install();
    let doc = r#"
import { TipBody, Theme } from "@ui-kit/ui-kit.slint";
export { Theme }
export component W inherits Window {
    width: 330px;
    height: 250px;
    background: Theme.bg;
    TipBody { x: 10px; y: 10px; width: self.preferred-width; height: self.preferred-height; says: "Auto framing"; detail: "Zoom and pan automatically to keep you in the picture."; }
    TipBody { x: 10px; y: 80px; width: self.preferred-width; height: self.preferred-height; says: "Speed"; detail: "How quickly the framing follows when you move."; hint: "Double-click to reset"; }
    TipBody { x: 10px; y: 150px; width: self.preferred-width; height: self.preferred-height; says: "Mirror"; }
    TipBody { x: 10px; y: 190px; width: self.preferred-width; height: self.preferred-height; says: "ISO"; detail: "Set Exposure to Manual to use this"; }
}"#;
    let def = telescope_shell::compile::compile("tips.slint", doc, vec![], "W").expect("compiles");
    for classic in [false, true] {
        let ui = def.create().unwrap();
        ui.set_global_property("Theme", "classic", Value::Bool(classic)).unwrap();
        ui.show().unwrap();
        offscreen::pump();
        let pic = offscreen::draw(ui.window(), 330.0, 250.0, 2.0).expect("a picture");
        pic.save_png(&dir.join(format!("tips-{}.png", if classic { "classic" } else { "new" }))).unwrap();
        ui.hide().unwrap();
    }
}
