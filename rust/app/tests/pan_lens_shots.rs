// Pictures of the pan map with lens outlines, cropped to the map at 3x: PAN_LENS_SHOTS=dir cargo nextest run -p telescope-app --test pan_lens_shots
use i_slint_backend_testing::ElementHandle;
use serde_json::{json, Value as Json};
use slint::platform::{PointerEventButton, WindowEvent};
use slint::ComponentHandle;
use slint_interpreter::Value;
use telescope_core::plugin::Plugin;
use telescope_core::ui::SlintSource;
use telescope_core::view::{lens_boxes, Geometry, PhoneZoomCaps};
use telescope_effects::transforms::TransformsPlugin;
use telescope_shell::offscreen;
use telescope_shell::source::to_property;

const WIDTH: f32 = 406.0;
const SCALE: f32 = 3.0;

fn card() -> (String, String, Json) {
    let card = TransformsPlugin::new().card().expect("a card").ui;
    let SlintSource::Text(source) = card.source else { panic!("source text") };
    let head = format!("export component {} inherits", card.component);
    let at = source.find(&head).expect("the card's component");
    let open = at + source[at..].find('{').unwrap() + 1;
    let source = format!("{}\n    out property <length> shot-height: root.preferred-height;\n    hue: Theme.plugin-hue(\"camera\");{}\nexport {{ Theme }}", &source[..open], &source[open..]);
    (source, card.component, card.props)
}

fn outlines(lenses: &[f64]) -> Json {
    let caps = PhoneZoomCaps { ratio_max: 10.0, crop_max: 4.0, freeform: true, lens_zooms: lenses.to_vec() };
    json!(lens_boxes(Some(&caps), &Geometry::default()).into_iter().map(|(_, x0, y0, x1, y1)| json!({"x0": x0, "y0": y0, "x1": x1, "y1": y1})).collect::<Vec<_>>())
}

/// The map with this zoom and pan and these lenses; `drag`: the pointer pressed on the crop's middle and moved by that many pixels.
fn shoot(dir: &std::path::Path, name: &str, zoom: f64, pan: (f64, f64), lenses: &[f64], drag: Option<f64>) {
    offscreen::install();
    let (source, component, base) = card();
    let def = telescope_shell::compile::compile("card.slint", &source, vec![], &component).expect("the card compiles");
    let ui = def.create().unwrap();
    ui.set_global_property("Theme", "look", Value::Number(0.0)).unwrap();
    let props = json!({"active": true, "zoom": zoom, "pan-x": pan.0, "pan-y": pan.1, "pan-enabled": zoom > 1.0, "zoom-text": format!("{zoom:.1}\u{d7}"), "lens-outlines": outlines(lenses)});
    for part in [&base, &props] {
        for (key, value) in part.as_object().unwrap() {
            ui.set_property(key, to_property(&ui, key, value)).unwrap_or_else(|e| panic!("{key}: {e:?}"));
        }
    }
    ui.window().set_size(slint::LogicalSize::new(WIDTH, 2000.0));
    ui.show().unwrap();
    offscreen::pump();
    let Ok(Value::Number(height)) = ui.get_property("shot-height") else { panic!("no preferred height") };
    let draw = || offscreen::draw(ui.window(), WIDTH, (height as f32).ceil(), SCALE).expect("a picture");
    draw();
    offscreen::skip(std::time::Duration::from_millis(800));
    let pad = ElementHandle::find_by_element_type_name(&ui, "Pad").next().expect("the pad");
    let (at, size) = (pad.absolute_position(), pad.size());
    if let Some(by) = drag {
        let (cx, cy) = (at.x + size.width * (0.5 + pan.0 as f32 * (1.0 - 1.0 / zoom as f32) / 2.0), at.y + size.height / 2.0);
        let (down, moved) = (slint::LogicalPosition::new(cx, cy), slint::LogicalPosition::new(cx + by as f32, cy));
        ui.window().dispatch_event(WindowEvent::PointerMoved { position: down });
        ui.window().dispatch_event(WindowEvent::PointerPressed { position: down, button: PointerEventButton::Left });
        ui.window().dispatch_event(WindowEvent::PointerMoved { position: moved });
        draw();
        offscreen::skip(std::time::Duration::from_millis(800));
    }
    let pic = draw();
    let pad_px = (at.x * SCALE) as usize;
    let (x0, y0) = (pad_px.saturating_sub(12), (at.y * SCALE) as usize - 12);
    let (w, h) = (((size.width * SCALE) as usize + 24).min(pic.width as usize - x0), (size.height * SCALE) as usize + 24);
    let mut rgba = Vec::new();
    for row in y0..y0 + h {
        rgba.extend_from_slice(&pic.rgba[(row * pic.width as usize + x0) * 4..(row * pic.width as usize + x0 + w) * 4]);
    }
    offscreen::Picture { width: w as u32, height: h as u32, rgba }.save_png(&dir.join(format!("{name}.png"))).unwrap();
    if drag.is_some() {
        let up = slint::LogicalPosition::new(0.0, 0.0);
        ui.window().dispatch_event(WindowEvent::PointerReleased { position: up, button: PointerEventButton::Left });
    }
    ui.hide().unwrap();
}

#[test]
fn pan_lens_shots() {
    let Ok(dir) = std::env::var("PAN_LENS_SHOTS") else { return };
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir).unwrap();
    shoot(&dir, "one-lens-zoom-2", 2.0, (0.3, -0.2), &[3.7], None);
    shoot(&dir, "two-lenses-zoom-2", 2.0, (0.3, -0.2), &[2.0, 5.0], None);
    shoot(&dir, "two-lenses-zoom-1", 1.0, (0.0, 0.0), &[2.0, 5.0], None);
    shoot(&dir, "no-lenses", 2.0, (0.3, -0.2), &[], None);
    shoot(&dir, "one-lens-matched-at-rest", 3.7, (0.0, 0.0), &[3.7], None);
    shoot(&dir, "one-lens-matched-held-snapped", 3.7, (0.0, 0.0), &[3.7], Some(2.0));
    shoot(&dir, "one-lens-matched-held-pulled", 3.7, (0.0, 0.0), &[3.7], Some(7.0));
    shoot(&dir, "one-lens-matched-held-away", 3.7, (0.0, 0.0), &[3.7], Some(24.0));
}
