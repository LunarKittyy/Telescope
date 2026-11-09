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
    let on = json!({"active": true, "on": true, "page": 0, "auto-ready": true});
    shoot(&dir, "grading-header", &card, 406.0, &[on.clone()], &nothing);
    shoot(&dir, "grading-header-narrow", &card, 387.0, &[on.clone()], &nothing);
    shoot(&dir, "grading-header-icon-only", &card, 336.0, &[on.clone()], &nothing);
    shoot(&dir, "grading-header-busy", &card, 406.0, &[on.clone(), json!({"auto-busy": true, "auto-label": "Looking\u{2026}"})], &nothing);
    shoot(&dir, "grading-header-nostream", &card, 406.0, &[on.clone(), json!({"auto-ready": false})], &nothing);
    shoot(&dir, "grading-header-nostream-narrow", &card, 387.0, &[on, json!({"auto-ready": false})], &nothing);
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
    shoot(&dir, "background-picture-problem", &card, 406.0, &[base, json!({"mode": 2, "picture-text": "gone.png", "picture-problem": "Can't open that picture"})], &nothing);
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
    // Where the drawn row has something that isn't the card's own colour: the runs of it, in px of the 2x picture. The meter is its
    // bar, then the clip dot; the slider is its track, then the readout.
    let pic = offscreen::draw(w.window(), 420.0, 330.0, 2.0).expect("a picture");
    let runs = |name: &str| {
        let e = ElementHandle::find_by_element_type_name(&w, name).next().unwrap_or_else(|| panic!("no {name}"));
        let y = ((e.absolute_position().y + e.size().height / 2.0) * 2.0) as usize;
        let row: Vec<[u8; 3]> = (0..pic.width as usize).map(|x| { let i = (y * pic.width as usize + x) * 4; [pic.rgba[i], pic.rgba[i + 1], pic.rgba[i + 2]] }).collect();
        // The card's colour, read just inside its left edge
        let card = row[2 * 16 + 4];
        let differs = |c: &[u8; 3]| c.iter().zip(card).any(|(a, b)| a.abs_diff(b) > 3);
        let mut runs: Vec<(usize, usize)> = Vec::new();
        for (x, c) in row.iter().enumerate() {
            if differs(c) {
                match runs.last_mut() {
                    Some(last) if x - last.1 <= 4 => last.1 = x + 1,
                    _ => runs.push((x, x + 1)),
                }
            }
        }
        runs
    };
    let (meter, slider) = (runs("LevelMeter"), runs("Slider"));
    // Past the row's icon and before its readout (the 2x picture's x from 100 to 700): the bar and the track start at one x, and the
    // clip dot, the last thing of the meter, ends where the track does
    let inside = |r: Vec<(usize, usize)>| r.into_iter().filter(|r| r.0 >= 100 && r.0 < 700).collect::<Vec<_>>();
    let (meter, slider) = (inside(meter), inside(slider));
    assert_eq!(meter[0].0, slider[0].0, "starts: {meter:?} {slider:?}");
    assert!((meter.last().unwrap().1 as i64 - slider.last().unwrap().1 as i64).abs() <= 1, "ends: {meter:?} {slider:?}");
}

fn lens(id: &str, number: &str, rest: &str, name: &str) -> Json {
    json!({"id": id, "number": number, "unit": "x", "rest": rest, "name": name})
}

/// The lens row as the card sets it for `n` lenses.
fn lens_props(n: usize) -> Json {
    let all = [lens("ultra", "0.5", "Ultra", "Ultra"), lens("wide", "0.6", "Wide", "Wide"), lens("main", "1", "Main", "Main"), lens("tele", "2", "Zoom", "Zoom"), lens("tele5", "5", "Tele", "Tele")];
    let lenses: Vec<Json> = all[..n].to_vec();
    let names: Vec<Json> = lenses.iter().map(|l| l["name"].clone()).collect();
    json!({"lenses": lenses, "lens-names": names, "lens-columns": n.max(1), "current-lens": if n > 0 { 1 } else { -1 }})
}

/// Every 16 ms of the lens row changing from `from` lenses to `to`, as film/NAME-NN.png (the top of the card, 2x).
#[test]
fn lens_row_film() {
    let Some(dir) = out() else { return };
    let film = dir.join("lens-film");
    std::fs::create_dir_all(&film).unwrap();
    offscreen::install();
    let (source, component, base) = card_of(&mut CameraControl::new(), "camera");
    let def = telescope_shell::compile::compile("card.slint", &source, vec![], &component).expect("the card compiles");
    let set = |ui: &slint_interpreter::ComponentInstance, props: &Json| {
        for (key, value) in props.as_object().unwrap() {
            ui.set_property(key, to_property(ui, key, value)).unwrap_or_else(|e| panic!("{key}: {e:?}"));
        }
    };
    // None is the dim placeholders before the phone has said
    for (name, from, to) in [("3to2", 3, 2), ("3to5", 3, 5), ("5to2", 5, 2), ("none-to-3", 0, 3), ("none-to-5", 0, 5), ("2to5-midway-back", 2, 5)] {
        let ui = def.create().unwrap();
        set(&ui, &base);
        set(&ui, &json!({"active": true}));
        set(&ui, &lens_props(from));
        ui.window().set_size(slint::LogicalSize::new(406.0, 2400.0));
        ui.show().unwrap();
        offscreen::pump();
        let Value::Number(h) = ui.get_property("shot-height").unwrap() else { panic!("height") };
        let (w, h) = (406.0_f32, (h as f32).ceil() + 24.0);
        ui.window().set_size(slint::LogicalSize::new(w, h));
        settle();
        let shoot_row = |n: usize| {
            let pic = offscreen::draw(ui.window(), w, h, 2.0).expect("a picture");
            // The card's header is cut off: the lens row sits under it
            let (x0, y0, cw, ch) = (0usize, 100usize, pic.width as usize, 130usize);
            let mut rgba = Vec::with_capacity(cw * ch * 4);
            for y in y0..y0 + ch {
                rgba.extend_from_slice(&pic.rgba[(y * pic.width as usize + x0) * 4..(y * pic.width as usize + x0 + cw) * 4]);
            }
            offscreen::Picture { width: cw as u32, height: ch as u32, rgba }.save_png(&film.join(format!("{name}-{n:02}.png"))).unwrap();
        };
        shoot_row(0);
        // The clock is held and moved by hand, 16 ms at a time, so every film is the same
        let _held = offscreen::freeze_clock();
        // Time the drawing took has passed since the last tick: tick now, so the change starts from this moment
        offscreen::skip(std::time::Duration::ZERO);
        set(&ui, &lens_props(to));
        for n in 1..=24 {
            offscreen::skip(std::time::Duration::from_millis(16));
            if name == "2to5-midway-back" && n == 7 {
                // Changes its mind half way: back to two
                set(&ui, &lens_props(2));
            }
            shoot_row(n);
        }
        ui.hide().unwrap();
    }
}

/// A card's source from the .slint files its plugin joins (as the plugin does), for a film of it.
fn card_from_files(files: &[&str], component: &str, group: &str, props: Json) -> Card {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../crates");
    let source: String = files.iter().map(|f| std::fs::read_to_string(root.join(f)).unwrap()).collect::<Vec<_>>().join("\n");
    let head = format!("export component {component} inherits");
    let at = source.find(&head).expect("the card's component");
    // The interpreter instantiates the last component of a file: a second one after the card (Cinema's settings) is left off
    let end = source[at + head.len()..].find("\nexport component ").map_or(source.len(), |i| at + head.len() + i);
    let source = &source[..end];
    let open = at + source[at..].find('{').unwrap() + 1;
    let source = format!("{}\n    out property <length> shot-height: root.preferred-height;\n    hue: Theme.plugin-hue(\"{group}\");{}\nexport {{ Theme }}", &source[..open], &source[open..]);
    (source, component.to_string(), props)
}

/// Frames of a card going from props `a` to props `b` and back, at 0, 32, 64, 96, 160 and 480 ms, as NAME-open-N.png and NAME-close-N.png
/// (the whole card, 2x, clock held and moved by hand).
fn unfold_film(dir: &Path, name: &str, card: &Card, base: Json, a: Json, b: Json) {
    offscreen::install();
    let (source, component, props) = card;
    let def = telescope_shell::compile::compile("card.slint", source, vec![], component).expect("the card compiles");
    let set = |ui: &slint_interpreter::ComponentInstance, parts: &[&Json]| {
        for part in parts {
            for (key, value) in part.as_object().unwrap() {
                // A property a card doesn't have (`on` on a card with no switch) is left out
                if let Err(e) = ui.set_property(key, to_property(ui, key, value)) {
                    assert!(matches!(e, slint_interpreter::SetPropertyError::NoSuchProperty), "{name} {key}: {e:?}");
                }
            }
        }
    };
    let width = 406.0_f32;
    let ui = def.create().unwrap();
    set(&ui, &[props, &base, &a]);
    ui.window().set_size(slint::LogicalSize::new(width, 2400.0));
    ui.show().unwrap();
    offscreen::pump();
    let height = |ui: &slint_interpreter::ComponentInstance| match ui.get_property("shot-height") {
        Ok(Value::Number(h)) => h as f32,
        other => panic!("{name}: no preferred height: {other:?}"),
    };
    // The taller of the two states sets the window, so neither is cut off
    set(&ui, &[&b]);
    settle();
    let tall = height(&ui).max({
        set(&ui, &[&a]);
        settle();
        height(&ui)
    });
    let h = tall.ceil() + 24.0;
    ui.window().set_size(slint::LogicalSize::new(width, h));
    settle();
    for (way, from, to) in [("open", &a, &b), ("close", &b, &a)] {
        set(&ui, &[from]);
        settle();
        let _held = offscreen::freeze_clock();
        offscreen::skip(std::time::Duration::ZERO);
        let frame = |n: usize| {
            offscreen::draw(ui.window(), width, h, 2.0).expect("a picture").save_png(&dir.join(format!("{name}-{way}-{n}.png"))).unwrap();
        };
        frame(0);
        set(&ui, &[to]);
        let mut at = 0;
        for (n, ms) in [32u64, 64, 96, 160, 480].into_iter().enumerate() {
            offscreen::skip(std::time::Duration::from_millis(ms - at));
            at = ms;
            frame(n + 1);
        }
    }
    ui.hide().unwrap();
}

#[test]
fn unfolding_rows_film() {
    let Some(dir) = out() else { return };
    let film = dir.join("unfold");
    std::fs::create_dir_all(&film).unwrap();
    let on = json!({"active": true, "on": true});
    let fx = |files: &[&str], c: &str, g: &str| card_from_files(files, c, g, json!({}));
    let vision: &[&str] = &["effects/ui/parts.slint", "effects/ui/vision_status.slint"];
    let join = |extra: &str| {
        let mut v = vision.to_vec();
        v.push(Box::leak(extra.to_string().into_boxed_str()));
        v
    };
    unfold_film(&film, "chroma-behind", &fx(&["effects/ui/parts.slint", "effects/ui/chroma_key.slint"], "ChromaKeyCard", "look"), on.clone(), json!({"behind": 0}), json!({"behind": 1}));
    unfold_film(&film, "chroma-status", &fx(&["effects/ui/parts.slint", "effects/ui/chroma_key.slint"], "ChromaKeyCard", "look"), on.clone(), json!({"status": ""}), json!({"status": "Picked from the preview", "status-kind": "status_warn"}));
    unfold_film(&film, "photos-status", &fx(&["effects/ui/parts.slint", "effects/ui/photos.slint"], "PhotosCard", "camera"), on.clone(), json!({"status": ""}), json!({"status": "Saved to Pictures", "status-kind": "status_ok"}));
    unfold_film(&film, "photos-exposure", &fx(&["effects/ui/parts.slint", "effects/ui/photos.slint"], "PhotosCard", "camera"), on.clone(), json!({"iso-options": [], "shutter-options": []}), json!({"iso-options": ["100", "200", "400"], "shutter-options": ["1/60", "1/125"]}));
    unfold_film(&film, "framing-vision", &fx(&join("effects/ui/framing.slint"), "FramingCard", "vision"), on.clone(), json!({"vision-text": ""}), json!({"vision-text": "Needs a download", "vision-download": true}));
    unfold_film(&film, "face-held", &fx(&join("effects/ui/face_exposure.slint"), "FaceExposureCard", "camera"), on.clone(), json!({"held": false, "vision-text": ""}), json!({"held": true, "held-note": "Manual", "follow-tip": "Go back to following your face.", "vision-text": ""}));
    unfold_film(&film, "face-vision", &fx(&join("effects/ui/face_exposure.slint"), "FaceExposureCard", "camera"), on.clone(), json!({"held": false, "vision-text": ""}), json!({"vision-text": "Looking for a face", "vision-download": false}));
    unfold_film(&film, "background-blur", &fx(&join("effects/ui/background.slint"), "BackgroundCard", "look"), on.clone(), json!({"mode": 0}), json!({"mode": 1}));
    unfold_film(&film, "privacy-vision", &fx(&join("effects/ui/privacy_blur.slint"), "PrivacyBlurCard", "vision"), on.clone(), json!({"vision-text": ""}), json!({"vision-text": "Needs a download", "vision-download": true}));
    unfold_film(&film, "cinema-level", &fx(&["effects/ui/parts.slint", "effects/ui/cinema.slint"], "CinemaCard", "preview"), json!({"active": true}), json!({"level-note-shown": false}), json!({"level-note-shown": true, "level-note": "This phone has no tilt sensor."}));
    unfold_film(&film, "notes-status", &fx(&["effects/ui/parts.slint", "effects/ui/notes.slint"], "NotesCard", "on_screen"), on.clone(), json!({"status": ""}), json!({"status": "Sent to the phone", "status-kind": "status_ok"}));
    unfold_film(&film, "desk-status", &fx(&["effects/ui/parts.slint", "effects/ui/desk.slint"], "DeskCard", "extras"), on.clone(), json!({"status": ""}), json!({"status": "Sheet unclear", "status-kind": "status_warn"}));
    unfold_film(&film, "qr-status", &fx(&["effects/ui/parts.slint", "effects/ui/qr_reader.slint"], "QrReaderCard", "vision"), on.clone(), json!({"status": "", "copy-shown": false}), json!({"status": "Copied", "copy-shown": true, "copy-text": "Copy again"}));
    unfold_film(&film, "composite-rows", &fx(&["effects/ui/parts.slint", "effects/ui/composite.slint"], "CompositeCard", "on_screen"), on.clone(), json!({"edit-shown": false, "status": ""}), json!({"edit-shown": true, "status": "Needs two streams"}));
    let mic = card_from_files(&["audio/ui/microphone.slint"], "MicrophoneCard", "sound", json!({}));
    unfold_film(&film, "mic-here", &mic, json!({"active": true, "on": true}), json!({"here": false}), json!({"here": true}));
    unfold_film(&film, "mic-status", &mic, json!({"active": true, "on": true, "here": true}), json!({"status-text": "", "fix-shown": false}), json!({"status-short": "Stopped", "status-text": "The virtual microphone stopped taking audio.", "status-kind": "status_err", "fix-shown": true, "fix-text": "Install VB-Cable"}));
    let out_card = card_from_files(&["outputs/ui/stream_output.slint"], "StreamOutputCard", "streaming", json!({}));
    unfold_film(&film, "stream-format", &out_card, json!({"active": true}), json!({"h264": true}), json!({"h264": false}));
    let browser = card_from_files(&["link/ui/browser_camera.slint"], "BrowserCameraCard", "streaming", json!({"shown": true}));
    unfold_film(&film, "browser-warning", &browser, json!({}), json!({"mic-text": "", "hint-text": ""}), json!({"mic-text": "No microphone: the browser blocked it", "hint-text": ""}));
}

#[test]
fn desk_tile_shots() {
    let Some(dir) = out() else { return };
    let card = card_of(&mut telescope_effects::desk::DeskPlugin::new(), "extras");
    shoot(&dir, "desk", &card, 406.0, &[json!({"active": true, "on": true, "clean-current": 1, "shape-current": 0})], &nothing);
    shoot(&dir, "desk-shape-a4", &card, 406.0, &[json!({"active": true, "on": true, "clean-current": 2, "shape-current": 2})], &nothing);
}
