// Pictures for the 12:26 fixes, for a look by eye: FIX_SHOTS=dir cargo nextest run -p telescope-app --test fix0912_shots
// The Stats card with a hovered row (its star shows, the others' room stays), and the Preview card's button row at a wide and a narrow card.
use serde_json::json;
use slint::platform::WindowEvent;
use slint::ComponentHandle;
use slint_interpreter::Value;
use std::path::PathBuf;
use telescope_core::plugin::Plugin;
use telescope_core::ui::SlintSource;
use telescope_shell::offscreen;
use telescope_shell::source::to_property;

fn out() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var("FIX_SHOTS").ok()?);
    std::fs::create_dir_all(&dir).unwrap();
    Some(dir)
}

#[test]
fn the_stats_card_with_a_hovered_row() {
    let Some(dir) = out() else { return };
    offscreen::install();
    let mut stats = telescope_setup::stats::StatsPlugin::new();
    let card = stats.card().expect("a card").ui;
    let SlintSource::Text(source) = card.source else { panic!("source text") };
    let head = format!("export component {} inherits", card.component);
    let at = source.find(&head).expect("the card's component");
    let open = at + source[at..].find('{').unwrap() + 1;
    let source = format!("{}\n    out property <length> shot-height: root.preferred-height;\n    hue: Theme.plugin-hue(\"streaming\");{}\nexport {{ Theme }}", &source[..open], &source[open..]);
    let def = telescope_shell::compile::compile("card.slint", &source, vec![], &card.component).expect("the card compiles");
    let graph = "M 0.5 16 L 10.5 9 L 20.5 12 L 30.5 4 L 40.5 8 L 59.5 10 ";
    let row = |label: &str, value: &str, note: &str, graph: &str, pin: &str, pinned: bool| json!({"head": false, "icon": "", "label": label, "value": value, "note": note, "kind": "", "graph": graph, "pin": pin, "pinned": pinned, "locked": false});
    let rows = json!([
        {"head": true, "icon": "video", "label": "Frames", "value": "", "note": "", "kind": "", "graph": "", "pin": "", "pinned": false, "locked": false},
        row("Frames in", "29.8 fps", "asked for 30 fps", graph, "stats.frames_in", false),
        row("Frames out", "30.0 fps", "1,204 frames", graph, "fps", true),
        row("Repeated", "0.0 fps", "", "", "stats.repeated", false),
        {"head": true, "icon": "connection", "label": "Link", "value": "", "note": "", "kind": "", "graph": "", "pin": "", "pinned": false, "locked": false},
        row("Connection", "USB", "TCP", "", "stats.connection", false),
        row("Reading the virtual camera", "Nothing", "", "", "stats.reading_camera", true),
        {"head": true, "icon": "plug", "label": "Each plugin's frame step", "value": "", "note": "", "kind": "", "graph": "", "pin": "", "pinned": false, "locked": false},
        row("QR reader", "0.0 ms", "slowest 0.0 ms", "", "stats.step_qr_reader", false),
        row("MCP", "0.2 ms", "slowest 0.3 ms", graph, "stats.step_mcp", false),
    ]);
    for (name, hover_y) in [("stats-rest", None), ("stats-hover-note", Some(150.0)), ("stats-hover-text", Some(312.0))] {
        let ui = def.create().unwrap();
        ui.set_property("rows", to_property(&ui, "rows", &rows)).unwrap();
        ui.set_property("view", Value::Number(1.0)).unwrap();
        // The card is told its two labels by the plugin; a harness that doesn't leaves two empty tiles
        ui.set_property("view-standard", Value::String("Standard".into())).unwrap();
        ui.set_property("view-nerd", Value::String("Nerd".into())).unwrap();
        ui.window().set_size(slint::LogicalSize::new(360.0, 900.0));
        ui.show().unwrap();
        offscreen::pump();
        offscreen::skip(std::time::Duration::from_millis(800));
        let height = match ui.get_property("shot-height") { Ok(Value::Number(h)) => h as f32, other => panic!("{other:?}") };
        ui.window().set_size(slint::LogicalSize::new(360.0, height.ceil() + 20.0));
        offscreen::pump();
        offscreen::draw(ui.window(), 360.0, height.ceil() + 20.0, 1.0).unwrap();
        if let Some(y) = hover_y {
            ui.window().dispatch_event(WindowEvent::PointerMoved { position: slint::LogicalPosition::new(200.0, y) });
            offscreen::pump();
            offscreen::skip(std::time::Duration::from_millis(300));
        }
        let pic = offscreen::draw(ui.window(), 360.0, height.ceil() + 20.0, 2.0).unwrap();
        pic.save_png(&dir.join(format!("{name}.png"))).unwrap();
        ui.hide().unwrap();
    }
}

#[test]
fn the_preview_buttons_with_smooth_on_and_off_at_two_widths() {
    let Some(dir) = out() else { return };
    offscreen::install();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    let marks = root.join("crates/shell/ui/preview.slint");
    let text = std::fs::read_to_string(root.join("crates/shell/ui/cards/preview.slint")).unwrap().replace("\"../../shell/preview.slint\"", &format!("\"{}\"", marks.display()));
    let head = "export component PreviewCard inherits Rectangle {";
    let at = text.find(head).unwrap() + head.len();
    let source = format!("{}\n    out property <length> shot-height: 340px;\n    height: 340px;{}\nexport {{ Theme }}", &text[..at], &text[at..]);
    let def = telescope_shell::compile::compile("card.slint", &source, vec![], "PreviewCard").expect("the card compiles");
    for (width, on) in [(520.0f32, false), (520.0, true), (330.0, true), (320.0, false)] {
        let ui = def.create().unwrap();
        for (key, value) in json!({"stage-text": "Start streaming to see the picture", "camera-on": true, "camera-enabled": true, "toggle-text": "Hide", "skeleton-shown": true, "lenses-enabled": true, "smooth-on": on}).as_object().unwrap() {
            ui.set_property(key, to_property(&ui, key, value)).unwrap_or_else(|e| panic!("{key}: {e:?}"));
        }
        ui.window().set_size(slint::LogicalSize::new(width, 340.0));
        ui.show().unwrap();
        offscreen::pump();
        offscreen::skip(std::time::Duration::from_millis(600));
        let pic = offscreen::draw(ui.window(), width, 340.0, 2.0).unwrap();
        pic.save_png(&dir.join(format!("preview-buttons-{}px-smooth-{}.png", width as u32, if on { "on" } else { "off" }))).unwrap();
        ui.hide().unwrap();
    }
}

#[test]
fn the_specimen_sheet_for_a_look_at_every_segmented() {
    let Some(dir) = out() else { return };
    assert_eq!(telescope_shell::specimen::render(&dir.join("specimen")), Ok(4));
}
