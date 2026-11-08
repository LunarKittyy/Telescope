// Pictures of the cards, dialogs and bars round 2 changed, new look only, for a look by eye:
// CARD_SHOTS=dir cargo nextest run -p telescope-app --test r2_shots, each as NAME.png at 2x.
use serde_json::{json, Value as Json};
use slint::ComponentHandle;
use slint_interpreter::{Compiler, Value};
use std::path::{Path, PathBuf};
use telescope_core::plugin::Plugin;
use telescope_core::ui::SlintSource;
use telescope_effects::background::BackgroundPlugin;
use telescope_effects::chroma_key::ChromaKeyPlugin;
use telescope_effects::colour_grading::ColourGradingPlugin;
use telescope_effects::face_exposure::FaceExposurePlugin;
use telescope_effects::fun_pack::FunPackPlugin;
use telescope_effects::name_tag::NameTagPlugin;
use telescope_shell::plugins::camera_control::CameraControl;
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

type Card = (String, String, Json);
type Then<'a> = &'a dyn Fn(&slint_interpreter::ComponentInstance);

fn nothing(_: &slint_interpreter::ComponentInstance) {}

/// Lets what animates settle.
fn settle() {
    for _ in 0..30 {
        offscreen::pump();
        std::thread::sleep(std::time::Duration::from_millis(16));
    }
}

/// A plugin's card source with the Theme global let out and its natural height readable.
fn card_of(plugin: &mut dyn Plugin, group: &str) -> Card {
    let card = plugin.card().expect("a card").ui;
    let SlintSource::Text(source) = card.source else { panic!("source text") };
    let head = format!("export component {} inherits", card.component);
    let at = source.find(&head).expect("the card's component");
    let open = at + source[at..].find('{').unwrap() + 1;
    let source = format!("{}\n    out property <length> shot-height: root.preferred-height;\n    hue: Theme.plugin-hue(\"{group}\");{}\nexport {{ Theme }}", &source[..open], &source[open..]);
    (source, card.component, card.props)
}

/// The card at `width` px, `then` run on it once shown, 2x.
fn shoot(dir: &Path, name: &str, card: &Card, width: f32, props: &[Json], then: Then) {
    offscreen::install();
    let (source, component, base) = card;
    let def = telescope_shell::compile::compile("card.slint", source, vec![], component).expect("the card compiles");
    let ui = def.create().unwrap();
    for part in std::iter::once(base).chain(props) {
        for (key, value) in part.as_object().unwrap() {
            let set = ui.set_property(key, to_property(&ui, key, value));
            // Run on the old sources for the before pictures, a property of the new look is not there to set
            if std::env::var("R2_BEFORE").is_err() {
                set.unwrap_or_else(|e| panic!("{key}: {e:?}"));
            }
        }
    }
    ui.window().set_size(slint::LogicalSize::new(width, 2400.0));
    ui.show().unwrap();
    offscreen::pump();
    let height = match ui.get_property("shot-height") {
        Ok(Value::Number(h)) => h as f32,
        other => panic!("no preferred height: {other:?}"),
    };
    ui.window().set_size(slint::LogicalSize::new(width, height.ceil() + 24.0));
    then(&ui);
    settle();
    let pic = offscreen::draw(ui.window(), width, height.ceil() + 24.0, 2.0).expect("a picture");
    pic.save_png(&dir.join(format!("{name}.png"))).unwrap();
    ui.hide().unwrap();
}

/// A component straight from its own .slint file, in a window of `size` with `then` run once it is shown.
fn file_widget(dir: &Path, name: &str, file: &str, component: &str, props: &str, size: (f32, f32), then: Then) {
    // The properties `then` sets on the component are passed up through the window
    let alias = match component {
        "PhonePicker" => "in-out property <bool> phones-open <=> c.phones-open;",
        "ConnectionCard" => "in-out property <bool> add-open <=> c.add-open;",
        "MicrophoneCard" => "in-out property <int> confirm-asked <=> c.confirm-asked;",
        _ => "",
    };
    offscreen::install();
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join(file);
    let bare = component == "Footer" || component == "PhonePicker";
    let doc = format!(
        "import {{ {component} }} from \"{}\";\nimport {{ Theme, WindowBox }} from \"@ui-kit/ui-kit.slint\";\nexport {{ Theme }}\nexport component W inherits Window {{\n    width: {}px;\n    height: {}px;\n    background: Theme.ground;\n    init => {{ WindowBox.width = self.width; WindowBox.height = self.height; }}\n    {alias}\n    c := {component} {{ x: {}px; y: {}px; {} {props} }}\n}}",
        path.display(),
        size.0,
        size.1,
        if bare { 0 } else { 16 },
        if bare { 8 } else { 16 },
        if bare { String::new() } else {
            let hue = if file.contains("microphone") { "sound" } else { "streaming" };
            format!("width: parent.width - 32px; height: self.preferred-height; hue: Theme.plugin-hue(\"{hue}\");")
        }
    );
    let mut compiler = Compiler::default();
    compiler.set_library_paths([(telescope_ui_kit::LIBRARY.to_string(), telescope_ui_kit::library_path())].into_iter().collect());
    let result = spin_on(compiler.build_from_source(doc, PathBuf::from("/root/wrapper.slint")));
    let errors: Vec<String> = result.diagnostics().filter(|d| d.level() == slint_interpreter::DiagnosticLevel::Error).map(|d| d.to_string()).collect();
    assert!(errors.is_empty(), "{}", errors.join("\n"));
    let def = result.component("W").unwrap();
    let w = def.create().unwrap();
    w.show().unwrap();
    offscreen::pump();
    then(&w);
    settle();
    let pic = offscreen::draw(w.window(), size.0, size.1, 2.0).expect("a picture");
    pic.save_png(&dir.join(format!("{name}.png"))).unwrap();
    w.hide().unwrap();
}

#[test]
fn grading_header_shots() {
    let Some(dir) = out() else { return };
    let card = card_of(&mut ColourGradingPlugin::new(), "look");
    let on = json!({"active": true, "on": true, "page": 0});
    shoot(&dir, "grading-header", &card, 406.0, &[on.clone()], &nothing);
    shoot(&dir, "grading-header-narrow", &card, 300.0, &[on.clone()], &nothing);
    shoot(&dir, "grading-header-busy", &card, 406.0, &[on.clone(), json!({"auto-busy": true, "auto-label": "Looking\u{2026}"})], &nothing);
    shoot(&dir, "grading-header-nostream", &card, 406.0, &[on, json!({"auto-label": "No stream"})], &nothing);
    shoot(&dir, "grading-lut", &card, 406.0, &[json!({"active": true, "on": true, "page": 2})], &nothing);
}

#[test]
fn microphone_shots() {
    let Some(dir) = out() else { return };
    let mic = "active: true; here: true; on: true; gain: 3; gain-text: \"+3 dB\"; meter: { peak: 0.5, rms: 0.35, hold: 0.62, lit: \"\", muted: false, readout: \"-18 dB\", red: false };";
    let size = (420.0, 330.0);
    let f = "crates/audio/ui/microphone.slint";
    file_widget(&dir, "mic-on", f, "MicrophoneCard", &format!("{mic} nr: true; auto-level: true; mute-text: \"Mute\";"), size, &nothing);
    file_widget(&dir, "mic-muted", f, "MicrophoneCard", "active: true; here: true; on: true; muted: true; mute-text: \"Unmute\"; meter: { peak: 0, rms: 0, hold: -1, lit: \"\", muted: true, readout: \"Muted\", red: true };", size, &nothing);
    file_widget(&dir, "mic-clip", f, "MicrophoneCard", "active: true; here: true; on: true; meter: { peak: 1, rms: 0.9, hold: 0.97, lit: \"clip\", muted: false, readout: \"-1 dB\", red: true };", size, &nothing);
    file_widget(&dir, "mic-idle", f, "MicrophoneCard", "active: true; here: true; on: true; meter: { peak: 0, rms: 0, hold: -1, lit: \"\", muted: false, readout: \"\", red: false };", size, &nothing);
    file_widget(&dir, "mic-rnnoise-missing", f, "MicrophoneCard", &format!("{mic} nr-enabled: false; nr-note: \"RNNoise is missing from this copy of Telescope.\";"), size, &nothing);
    file_widget(&dir, "mic-vbcable-dialog", f, "MicrophoneCard", &format!("{mic} origin: \"VB-Audio Virtual Cable by VB-Audio\"; donationware: \"Donationware\"; donate-text: \"Donate\";"), (520.0, 420.0), &|ui| {
        let _ = ui.set_property("confirm-asked", Value::Number(1.0));
    });
}

fn thumb(ui: &slint_interpreter::ComponentInstance) {
    // A small gradient standing in for a picture
    let mut pixels = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(96, 54);
    for (i, p) in pixels.make_mut_slice().iter_mut().enumerate() {
        let (x, y) = ((i % 96) as u8, (i / 96) as u8);
        *p = slint::Rgba8Pixel { r: 40 + x, g: 90 + y * 2, b: 200 - x, a: 255 };
    }
    let _ = ui.set_property("picture-thumb", Value::Image(slint::Image::from_rgba8(pixels)));
}

#[test]
fn background_shots() {
    let Some(dir) = out() else { return };
    let card = card_of(&mut BackgroundPlugin::new(), "look");
    let base = json!({"active": true});
    shoot(&dir, "background-off", &card, 406.0, &[base.clone(), json!({"mode": 0})], &nothing);
    shoot(&dir, "background-blur", &card, 406.0, &[base.clone(), json!({"mode": 1})], &nothing);
    shoot(&dir, "background-picture-empty", &card, 406.0, &[base.clone(), json!({"mode": 2, "picture-text": ""})], &nothing);
    shoot(&dir, "background-picture-set", &card, 406.0, &[base.clone(), json!({"mode": 2, "picture-text": "beach.png", "has-picture": true})], &thumb);
    shoot(&dir, "background-picture-problem", &card, 406.0, &[base, json!({"mode": 2, "picture-text": "gone.png", "picture-problem": "Telescope can't open that picture."})], &nothing);
    let chroma = card_of(&mut ChromaKeyPlugin::new(), "look");
    shoot(&dir, "chroma-picture", &chroma, 406.0, &[json!({"active": true, "on": true, "behind": 1, "picture-text": "beach.png", "has-picture": true})], &thumb);
}

#[test]
fn face_exposure_shots() {
    let Some(dir) = out() else { return };
    let card = card_of(&mut FaceExposurePlugin::new(), "camera");
    shoot(&dir, "face-exposure", &card, 406.0, &[json!({"active": true, "on": true})], &nothing);
    shoot(&dir, "face-exposure-held", &card, 406.0, &[json!({"active": true, "on": true, "held": true, "held-note": "Manual", "follow-tip": "Go back to following your face."})], &nothing);
}

const PHONES: &str = "rows: [{id: \"p1\", name: \"Pixel 8\", sub: \"Wi-Fi\", renamable: true, rename-label: \"Rename Pixel 8\", remove-label: \"Remove Pixel 8\", confirm-title: \"Remove Pixel 8?\", confirm: \"It will need pairing again.\"}, {id: \"p2\", name: \"Browser on iPad\", sub: \"Browser\", renamable: false, rename-label: \"\", remove-label: \"Remove Browser on iPad\", confirm-title: \"\", confirm: \"\"}]; computer-name: \"luna-desktop\"; names: [\"Pixel 8\"]; current: 0;";

#[test]
fn connection_dialog_shots() {
    let Some(dir) = out() else { return };
    let f = "crates/link/ui/connection.slint";
    file_widget(&dir, "your-devices", f, "PhonePicker", PHONES, (560.0, 520.0), &|ui| {
        let _ = ui.set_property("phones-open", Value::Bool(true));
    });
    file_widget(&dir, "your-devices-empty", f, "PhonePicker", "computer-name: \"luna-desktop\"; names: []; current: -1;", (560.0, 520.0), &|ui| {
        let _ = ui.set_property("phones-open", Value::Bool(true));
    });
    let (cells, modules) = telescope_core::qr::encode("telescope://pair/example");
    let cells = cells.iter().map(|c| c.to_string()).collect::<Vec<_>>().join(", ");
    let card = format!("status-text: \"Connected\"; status-kind: \"status_ok\"; using-text: \"Wi-Fi\"; using-shown: true; route-shown: true; wifi-text: \"192.168.1.20\"; add-subtitle: \"Scan this with the Telescope app\"; qr-cells: [{cells}]; qr-size: {modules}; usb-text: \"Plug the phone in\"; usb-kind: \"status_dim\";");
    file_widget(&dir, "connection-card", f, "ConnectionCard", &card, (420.0, 300.0), &nothing);
    file_widget(&dir, "add-a-phone", f, "ConnectionCard", &card, (560.0, 700.0), &|ui| {
        let _ = ui.set_property("add-open", Value::Bool(true));
    });
    file_widget(&dir, "add-a-phone-usb", f, "ConnectionCard", "status-text: \"No phone\"; add-subtitle: \"Pair a phone\"; usb-text: \"USB debugging is off\"; usb-action: \"Turn it on\"; qr-size: 0; wifi-text: \"This computer isn't on a network\";", (560.0, 520.0), &|ui| {
        let _ = ui.set_property("add-open", Value::Bool(true));
    });
}

#[test]
fn bar_shots() {
    let Some(dir) = out() else { return };
    file_widget(&dir, "bottom-bar-streaming", "crates/shell/ui/frame.slint", "Footer", "status: \"Streaming 1920 \u{d7} 1080 at 30 fps to Phone Camera\"; camera: \"Phone Camera\"; size: \"1920 \u{d7} 1080\"; fps: \"30\"; net: \"12.4 Mbps\";", (900.0, 56.0), &nothing);
    file_widget(&dir, "bottom-bar-idle", "crates/shell/ui/frame.slint", "Footer", "status: \"Not streaming\"; fps: \"-\"; net: \"-\";", (900.0, 56.0), &nothing);
}

#[test]
fn top_bar_shots() {
    let Some(dir) = out() else { return };
    // The whole window, whose top is the top bar: the first 80 px are cut out afterwards
    let scenes = vec![telescope_shell::scenes::Scene::new("window-top", || telescope_shell::scenes::Setup::window(None, json!({}), 1300.0, 900.0))];
    let (n, failed) = telescope_shell::scenes::render(&scenes, &[1.0], &dir).expect("rendered");
    assert!(failed.is_empty(), "{failed:?} {n}");
}

#[test]
fn lens_and_name_tag_shots() {
    let Some(dir) = out() else { return };
    let camera = card_of(&mut CameraControl::new(), "camera");
    let lenses = json!({"active": true, "lens-columns": 3, "current-lens": 1, "lens-names": ["Wide", "Main", "Zoom"], "lenses": [
        {"id": "wide", "number": "0.6", "unit": "x", "rest": "Wide", "name": "Wide"},
        {"id": "main", "number": "1", "unit": "x", "rest": "Main", "name": "Main"},
        {"id": "tele", "number": "2", "unit": "x", "rest": "Zoom", "name": "Zoom"}]});
    shoot(&dir, "camera-lenses", &camera, 406.0, &[lenses.clone()], &nothing);
    shoot(&dir, "camera-lenses-four", &camera, 406.0, &[lenses, json!({"lens-columns": 2, "lens-names": ["Wide", "Main", "Zoom", "Macro"], "lenses": [
        {"id": "wide", "number": "0.6", "unit": "x", "rest": "Wide", "name": "Wide"}, {"id": "main", "number": "1", "unit": "x", "rest": "Main", "name": "Main"},
        {"id": "tele", "number": "2", "unit": "x", "rest": "Zoom", "name": "Zoom"}, {"id": "macro", "number": "3", "unit": "x", "rest": "Macro", "name": "Macro"}]})], &nothing);
    let tag = card_of(&mut NameTagPlugin::new(), "on_screen");
    shoot(&dir, "nametag", &tag, 406.0, &[json!({"style-items": ["Bar", "Pill", "Light", "Shadow"], "style-current": 1, "position-items": ["Bottom left", "Bottom centre", "Bottom right", "Top left", "Top right"], "position-current": 0, "size-items": ["Small", "Medium", "Large"], "size-current": 1})], &nothing);
    let fun = card_of(&mut FunPackPlugin::new(), "extras");
    shoot(&dir, "fun-pack", &fun, 406.0, &[json!({"active": true, "effect-items": ["Off", "Clones", "Cloak"], "effect": 1})], &nothing);
}

#[test]
fn the_microphone_meter_and_gain_slider_share_one_track() {
    use i_slint_backend_testing::ElementHandle;
    offscreen::install();
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../crates/audio/ui/microphone.slint");
    let doc = format!("import {{ MicrophoneCard }} from \"{}\";\nimport {{ Theme }} from \"@ui-kit/ui-kit.slint\";\nexport {{ Theme }}\nexport component W inherits Window {{ width: 420px; height: 330px; MicrophoneCard {{ width: 400px; active: true; here: true; on: true; meter: {{ peak: 0.5, rms: 0.3, hold: 0.6, lit: \"\", muted: false, readout: \"-18 dB\", red: false }}; }} }}", path.display());
    let mut compiler = Compiler::default();
    compiler.set_library_paths([(telescope_ui_kit::LIBRARY.to_string(), telescope_ui_kit::library_path())].into_iter().collect());
    let result = spin_on(compiler.build_from_source(doc, PathBuf::from("/root/wrapper.slint")));
    assert!(result.diagnostics().all(|d| d.level() != slint_interpreter::DiagnosticLevel::Error));
    let w = result.component("W").unwrap().create().unwrap();
    w.show().unwrap();
    settle();
    let edges = |name: &str| {
        let e = ElementHandle::find_by_element_type_name(&w, name).next().unwrap_or_else(|| panic!("no {name}"));
        (e.absolute_position().x, e.absolute_position().x + e.size().width)
    };
    let (meter, slider) = (edges("LevelMeter"), edges("Slider"));
    // The slider's box includes its readout column; the meter's row has the same one, so the tracks line up
    assert!((meter.0 - slider.0).abs() < 0.5, "starts: {meter:?} {slider:?}");
    let readout = Theme_u(28.0) + Theme_u(4.0);
    assert!((meter.1 + readout - slider.1).abs() < 0.5, "ends: {meter:?} {slider:?}");
}

#[allow(non_snake_case)]
fn Theme_u(n: f32) -> f32 {
    n * 2.0
}

/// The main window with the columns' scroll handles showing or not, and the gaps around them measured in the picture.
#[test]
fn window_gutter_shots() {
    let Some(dir) = out() else { return };
    // The scenes grow the window to its content; these draw the window at a size of their own, short enough for the columns to scroll
    let mut scenes = Vec::new();
    for (name, width, height) in [("window-default", 1380.0_f32, 330.0_f32), ("window-wide", 1900.0, 330.0), ("window-bars-hidden", 1380.0, 3000.0)] {
        let dir = dir.clone();
        let then = std::cell::RefCell::new(Some(Box::new(move |w: &std::rc::Rc<telescope_shell::window::Window>| {
            let win = w.ui.window();
            win.set_size(slint::LogicalSize::new(width, height));
            settle();
            let pic = offscreen::draw(win, width, height, 1.0).expect("a picture");
            pic.save_png(&dir.join(format!("{name}.png"))).unwrap();
        }) as telescope_shell::scenes::AfterBuilt));
        scenes.push(telescope_shell::scenes::Scene::new(name, move || {
            let mut s = telescope_shell::scenes::Setup::window(None, json!({}), width, height);
            // Cards in the left rail and the right one, with the stage between
            s.plugins = Some(|bus| {
                let mut plugins = telescope_shell::plugins::builtins(None, bus);
                plugins.push(Box::new(telescope_effects::transforms::TransformsPlugin::new()));
                plugins.push(Box::new(telescope_effects::desk::DeskPlugin::new()));
                plugins.push(Box::new(telescope_effects::framing::FramingPlugin::new()));
                plugins.push(Box::new(ColourGradingPlugin::new()));
                plugins.push(Box::new(NameTagPlugin::new()));
                telescope_shell::plugins::in_order(plugins)
            });
            s.then = then.borrow_mut().take();
            s
        }));
    }
    let (n, failed) = telescope_shell::scenes::render(&scenes, &[1.0], &std::env::temp_dir().join("r2-gutter-unused")).expect("rendered");
    assert!(failed.is_empty(), "{failed:?} {n}");
}
