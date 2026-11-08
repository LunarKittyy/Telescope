//! Pictures of the redesigned cards for a look by eye: CARD_SHOTS=dir cargo nextest run -p telescope-app --test card_shots.
//! Each scene is drawn at 2x in the new look and in Classic, and again with the dot variant of the change mark.

use serde_json::{json, Value as Json};
use slint_interpreter::{Compiler, ComponentHandle, Value};
use std::path::{Path, PathBuf};
use telescope_core::plugin::Plugin;
use telescope_core::ui::SlintSource;
use telescope_effects::desk::DeskPlugin;
use telescope_effects::framing::FramingPlugin;
use telescope_effects::name_tag::NameTagPlugin;
use telescope_effects::notes::NotesPlugin;
use telescope_shell::offscreen;

fn spin_on<F: std::future::Future>(f: F) -> F::Output {
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    let mut f = std::pin::pin!(f);
    loop {
        if let std::task::Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
    }
}

/// A JSON value as Slint source text.
fn literal(v: &Json) -> String {
    match v {
        Json::String(s) => format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n")),
        Json::Array(a) => format!("[{}]", a.iter().map(literal).collect::<Vec<_>>().join(", ")),
        other => other.to_string(),
    }
}

struct Scene {
    name: &'static str,
    props: Json,
}

fn shoot(dir: &Path, plugin: &mut dyn Plugin, scenes: Vec<Scene>) {
    offscreen::install();
    let group = plugin.info().category;
    let card = plugin.card().expect("a card").ui;
    let SlintSource::Text(source) = card.source.clone() else { panic!("source text") };
    for scene in scenes {
        let mut props = card.props.as_object().cloned().unwrap_or_default();
        props.extend(scene.props.as_object().cloned().unwrap_or_default());
        let set: String = props.iter().map(|(k, v)| format!("{k}: {}; ", literal(v))).collect();
        for (look, classic) in [("new", false), ("classic", true)] {
            for dot in [false, true] {
                let doc = format!(
                    "import {{ Theme }} from \"@ui-kit/ui-kit.slint\";\nexport {{ Theme }}\nimport {{ TipLayer }} from \"@ui-kit/ui-kit.slint\";\nimport {{ {c} }} from \"plugin/card.slint\";\n\
                     export component W inherits Window {{\n    width: 406px;\n    height: card.preferred-height;\n    background: Theme.ground;\n    out property <length> h: card.preferred-height;\n    \
                     card := {c} {{ width: parent.width; hue: Theme.plugin-hue({group:?}); {set}}}\n    TipLayer {{ }}\n}}",
                    c = card.component
                );
                let source = source.clone();
                let mut compiler = Compiler::default();
                compiler.set_library_paths([(telescope_ui_kit::LIBRARY.to_string(), telescope_ui_kit::library_path())].into_iter().collect());
                compiler.set_file_loader(move |path: &Path| {
                    let found = path.ends_with("plugin/card.slint").then(|| Ok(source.clone()));
                    Box::pin(async move { found })
                });
                let result = spin_on(compiler.build_from_source(doc, PathBuf::from("/root/wrapper.slint")));
                let errors: Vec<String> = result.diagnostics().filter(|d| d.level() == slint_interpreter::DiagnosticLevel::Error).map(|d| d.to_string()).collect();
                assert!(errors.is_empty(), "{}: {}", scene.name, errors.join("\n"));
                let w = result.component("W").unwrap().create().unwrap();
                w.set_global_property("Theme", "classic", Value::Bool(classic)).unwrap();
                w.set_global_property("Theme", "change-dot", Value::Bool(dot)).unwrap();
                w.show().unwrap();
                let Ok(Value::Number(h)) = w.get_property("h") else { panic!("no height") };
                let pic = offscreen::draw(w.window(), 406.0, h as f32, 2.0).expect("a picture");
                let file = format!("{}-{look}{}.png", scene.name, if dot { "-dot" } else { "" });
                pic.save_png(&dir.join(file)).unwrap();
                w.hide().unwrap();
            }
        }
    }
}

#[test]
fn shots() {
    let Ok(dir) = std::env::var("CARD_SHOTS") else { return };
    let dir = PathBuf::from(dir);
    std::fs::create_dir_all(&dir).unwrap();
    let on = json!({"on": true, "active": true});
    let mut desk = DeskPlugin::new();
    shoot(&dir, &mut desk, vec![
        Scene { name: "desk-default", props: on.clone() },
        Scene { name: "desk-changed", props: json!({"on": true, "active": true, "stretch": [120, 90], "stretch-text": ["120%", "90%"], "reset-shown": true, "clean-current": 1, "shape-current": 2}) },
        Scene { name: "desk-picking", props: json!({"on": true, "active": true, "picking": true, "stretch": [100, 100]}) },
        Scene { name: "desk-fill", props: json!({"on": true, "active": true, "shape-current": 4, "stretch-enabled": false}) },
        Scene { name: "desk-unpaired", props: json!({"on": false, "active": false}) },
        Scene { name: "desk-german", props: json!({"on": true, "active": true, "clean-items": ["Aus", "Kontrast", "Schwarzweiss"], "shape-items": ["Automatisch", "A4", "Letter", "Quadrat", "Füllen"], "stretch": [120, 90], "stretch-text": ["120 %", "90 %"], "reset-shown": true}) },
    ]);
    let mut framing = FramingPlugin::new();
    shoot(&dir, &mut framing, vec![
        Scene { name: "framing-default", props: on.clone() },
        Scene { name: "framing-changed", props: json!({"on": true, "active": true, "framing": 20, "speed": 8, "room": 4, "hand": true, "framing-text": "Waist", "speed-text": "Snappy", "room-text": "Step"}) },
        Scene { name: "framing-unpaired", props: json!({"on": false, "active": false}) },
        Scene { name: "framing-german", props: json!({"on": true, "active": true, "framing": 20, "speed": 3, "room": 4, "framing-text": "Hüfte", "speed-text": "Gemütlich", "room-text": "Schritt", "fits": "Gemütlich\nSchwungvoll\nUmherstreifen"}) },
        Scene { name: "framing-status", props: json!({"on": true, "active": true, "vision-text": "Needs a one-time download (about 47 MB).", "vision-download": true}) },
    ]);
    let mut notes = NotesPlugin::new();
    shoot(&dir, &mut notes, vec![
        Scene { name: "notes-empty", props: json!({"active": true}) },
        Scene { name: "notes-hand", props: json!({"active": true, "text": "Good morning, everyone. Thanks for joining the call, let's start with the plan for the quarter.\n\nThen questions.", "size": 80, "size-text": "80", "scroll": 1, "show-auto": false, "send-enabled": true, "clear-enabled": true, "nav-enabled": true}) },
        Scene { name: "notes-auto", props: json!({"active": true, "text": "Good morning, everyone.", "size-text": "44", "speed": 6, "speed-text": "6", "send-enabled": true, "clear-enabled": true, "nav-enabled": true, "playing": true, "play-text": "Pause", "set-items": ["Notes 1", "Notes 2"]}) },
        Scene { name: "notes-unpaired", props: json!({"active": false}) },
    ]);
    let mut tag = NameTagPlugin::new();
    shoot(&dir, &mut tag, vec![
        Scene { name: "nametag-empty", props: json!({"on": false}) },
        Scene { name: "nametag-filled", props: json!({"on": true, "name": "Alex Rivera", "pronouns": "they/them", "second": "Designer", "position-current": 2, "style-current": 3, "size-current": 2}) },
        Scene { name: "nametag-pill", props: json!({"on": true, "name": "Alex Rivera", "pronouns": "", "second": "", "position-current": 3, "style-current": 1, "size-current": 0}) },
    ]);
}
