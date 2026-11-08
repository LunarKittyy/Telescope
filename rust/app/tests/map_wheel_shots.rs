// Pictures of the Transforms card (the zoom and pan map) and the Colour grading card (the wheels), open and closed, in both looks:
// CARD_SHOTS=dir cargo nextest run -p telescope-app --test map_wheel_shots, each as NAME-new.png and NAME-classic.png at 2x.
use i_slint_backend_testing::ElementHandle;
use serde_json::{json, Value as Json};
use slint::ComponentHandle;
use slint_interpreter::Value;
use telescope_core::plugin::Plugin;
use telescope_core::ui::SlintSource;
use telescope_effects::colour_grading::{wheel_colour, ColourGradingPlugin};
use telescope_effects::transforms::TransformsPlugin;
use telescope_shell::offscreen;
use telescope_shell::source::to_property;

const WIDTH: f32 = 406.0;

/// A card's source with the Theme global let out, so a picture can be asked for in either look.
fn card_of(plugin: &mut dyn Plugin, group: &str) -> (String, String, Json) {
    let card = plugin.card().expect("a card").ui;
    let SlintSource::Text(source) = card.source else { panic!("source text") };
    let head = format!("export component {} inherits", card.component);
    let at = source.find(&head).expect("the card's component");
    let open = at + source[at..].find('{').unwrap() + 1;
    let source = format!("{}\n    out property <length> shot-height: root.preferred-height;\n    hue: Theme.plugin-hue(\"{group}\");{}\nexport {{ Theme }}", &source[..open], &source[open..]);
    (source, card.component, card.props)
}

fn shoot(dir: &std::path::Path, name: &str, card: &(String, String, Json), props: &[Json]) {
    shoot_with(dir, name, card, props, &[]);
}

/// `images`: pictures for image properties, which JSON has no way to carry.
fn shoot_with(dir: &std::path::Path, name: &str, card: &(String, String, Json), props: &[Json], images: &[(&str, slint::Image)]) {
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
        for (key, image) in images {
            ui.set_property(key, Value::Image(image.clone())).unwrap_or_else(|e| panic!("{key}: {e:?}"));
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

/// The card with the pointer held down on the control found by `at` (given the card and the pad's or wheel's box: left, top, width,
/// height) and drawn that way, for the held look.
fn shoot_held(dir: &std::path::Path, name: &str, element: &str, nth: usize, card: &(String, String, Json), props: &[Json], images: &[(&str, slint::Image)], at: &dyn Fn((f64, f64, f64, f64)) -> (f64, f64), shift: bool) {
    use slint::platform::{Key, PointerEventButton, WindowEvent};
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
        for (key, image) in images {
            ui.set_property(key, Value::Image(image.clone())).unwrap();
        }
        ui.window().set_size(slint::LogicalSize::new(WIDTH, 2000.0));
        ui.show().unwrap();
        offscreen::pump();
        let height = match ui.get_property("shot-height") {
            Ok(Value::Number(h)) => h as f32,
            other => panic!("no preferred height: {other:?}"),
        };
        let found = ElementHandle::find_by_element_type_name(&ui, element).nth(nth).expect("the control");
        let (pos, size) = (found.absolute_position(), found.size());
        let spot = at((pos.x as f64, pos.y as f64, size.width as f64, size.height as f64));
        let position = slint::LogicalPosition::new(spot.0 as f32, spot.1 as f32);
        if shift {
            ui.window().dispatch_event(WindowEvent::KeyPressed { text: Key::Shift.into() });
        }
        ui.window().dispatch_event(WindowEvent::PointerMoved { position });
        ui.window().dispatch_event(WindowEvent::PointerPressed { position, button: PointerEventButton::Left });
        // a little way along, so a drag has begun
        let moved = slint::LogicalPosition::new(position.x + 2.0, position.y + 1.0);
        ui.window().dispatch_event(WindowEvent::PointerMoved { position: moved });
        offscreen::pump();
        let pic = offscreen::draw(ui.window(), WIDTH, height.ceil(), 2.0).expect("a picture");
        pic.save_png(&dir.join(format!("{name}-{}.png", if classic { "classic" } else { "new" }))).unwrap();
        ui.window().dispatch_event(WindowEvent::PointerReleased { position: moved, button: PointerEventButton::Left });
        if shift {
            ui.window().dispatch_event(WindowEvent::KeyReleased { text: Key::Shift.into() });
        }
        ui.hide().unwrap();
    }
}

fn out() -> Option<std::path::PathBuf> {
    let dir = std::path::PathBuf::from(std::env::var("CARD_SHOTS").ok()?);
    std::fs::create_dir_all(&dir).unwrap();
    Some(dir)
}


fn zoomed(zoom: f64, pan_x: f64, pan_y: f64) -> Json {
    json!({"active": true, "zoom": zoom, "pan-x": pan_x, "pan-y": pan_y, "pan-enabled": zoom > 1.0, "zoom-text": format!("{zoom:.1}\u{d7}")})
}

#[test]
fn map_shots() {
    let Some(dir) = out() else { return };
    let card = card_of(&mut TransformsPlugin::new(), "camera");
    shoot(&dir, "map-closed", &card, &[json!({"active": true})]);
    shoot(&dir, "map-zoomed", &card, &[zoomed(2.5, 0.4, -0.3)]);
    // A portrait frame sits in the same row
    shoot(&dir, "map-portrait", &card, &[zoomed(2.0, -0.5, 0.5), json!({"map-aspect": 0.5625})]);
    // Someone else (auto framing) has the view: the map follows it, nothing to drag
    shoot(&dir, "map-taken", &card, &[json!({"active": true, "zoom": 2.0, "pan-x": 0.3, "pan-y": 0.0, "pan-enabled": false, "zoom-free": false})]);
}

#[test]
fn map_held_shots() {
    let Some(dir) = out() else { return };
    let card = card_of(&mut TransformsPlugin::new(), "camera");
    // 2.5x with a pan of (0.4, -0.3): the crop's middle is at (0.5 + 0.4 * 0.3, 0.5 - 0.3 * 0.3) of the frame
    let (cx, cy) = (0.5 + 0.4 * 0.3, 0.5 - 0.3 * 0.3);
    shoot_held(&dir, "map-held", "Pad", 0, &card, &[zoomed(2.5, 0.4, -0.3)], &[], &|(x, y, w, h)| (x + cx * w, y + cy * h), false);
}

/// The wheels' pucks somewhere, the colours they add and the master readouts.
fn graded() -> Json {
    let pucks = [(-0.55, 0.35), (0.25, -0.4), (0.7, 0.15), (0.0, 0.0)];
    json!({
        "active": true, "on": true,
        "wheel-x": pucks.iter().map(|p| p.0).collect::<Vec<_>>(),
        "wheel-y": pucks.iter().map(|p| p.1).collect::<Vec<_>>(),
        "wheel-colour": pucks.iter().map(|p| wheel_colour(p.0, p.1, 1.6)).collect::<Vec<_>>(),
        "master-text": ["+0.05", "-0.20", "1.10", "0.00"],
        "master-value": [0.05, -0.2, 1.1, 0.0],
    })
}

#[test]
fn grading_wheels_shots() {
    let Some(dir) = out() else { return };
    let card = card_of(&mut ColourGradingPlugin::new(), "look");
    shoot(&dir, "wheels-rest", &card, &[json!({"active": true, "on": true})]);
    shoot(&dir, "wheels-graded", &card, &[graded()]);
    shoot(&dir, "wheels-off", &card, &[json!({"active": false, "on": true})]);
    // Held: the puck takes its halo, and with Shift a ring of ink and a dot
    let centre = |(x, y, w, h): (f64, f64, f64, f64)| (x + w / 2.0, y + h / 2.0);
    shoot_held(&dir, "wheels-held", "ColourWheel", 1, &card, &[graded()], &[], &centre, false);
    shoot_held(&dir, "wheels-held-fine", "ColourWheel", 1, &card, &[graded()], &[], &centre, true);
}
