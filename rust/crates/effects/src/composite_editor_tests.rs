//! test_composite.py's editor tests, by the same names. The window reports the pointer in px of a picture this big.

use super::*;
use crate::composite::{MAX_NAME, MAX_ZOOM};

const AREA: (f64, f64) = (800.0, 450.0);

fn open() -> Rig {
    let mut r = rig();
    r.two_streams(0.0);
    r.plugin.shared.hub().main_frame = Some((solid([255, 0, 0], (90, 160)), monotonic_ns() + 3600 * SECOND));
    r.plugin.open_editor(&mut r.host);
    r
}

impl Rig {
    fn ui(&mut self, callback: &str, args: Value) {
        let args = args.as_array().cloned().unwrap_or_default();
        self.plugin.on_ui(&mut self.host, &UiEvent { ui: "editor".into(), callback: callback.into(), args });
    }

    fn pointer(&mut self, kind: &str, at: (f64, f64), right: bool, mods: (bool, bool, bool)) {
        self.ui("pointer", json!([kind, at.0 * AREA.0, at.1 * AREA.1, AREA.0, AREA.1, right, mods.0, mods.1, mods.2]));
    }

    /// Press at start, move to end with (ctrl, shift, alt) held, let go.
    fn drag(&mut self, start: (f64, f64), end: (f64, f64), mods: (bool, bool, bool)) {
        self.pointer("down", start, false, (false, false, false));
        self.pointer("move", end, false, mods);
        self.pointer("up", end, false, (false, false, false));
    }

    fn picture(&mut self, tiles: Vec<Tile>) {
        self.plugin.set_layout(&mut self.host, Layout::of(tiles));
    }

    fn tile(&self, i: usize) -> Tile {
        self.plugin.layout().tiles[i].clone()
    }

    fn props(&self) -> Value {
        self.plugin.editor_props()
    }

    fn sources(&self) -> Vec<String> {
        self.plugin.layout().tiles.iter().map(|t| t.source_id.clone()).collect()
    }

    /// A name in the prompt the button opened.
    fn name(&mut self, button: &str, typed: &str) {
        self.ui(button, json!([]));
        assert_eq!(self.props()["asking-name"], json!(true));
        self.ui("name-entered", json!([typed]));
    }
}

fn near(a: f64, b: f64, by: f64) -> bool {
    (a - b).abs() <= by
}

const NO_KEYS: (bool, bool, bool) = (false, false, false);
const CTRL: (bool, bool, bool) = (true, false, false);
const SHIFT: (bool, bool, bool) = (false, true, false);
const ALT: (bool, bool, bool) = (false, false, true);

#[test]
fn a_closed_editor_is_freed() {
    let mut r = open();
    assert!(r.plugin.editor_open());
    r.ui("closed", json!([]));
    assert!(!r.plugin.editor_open());
    assert!(r.host.calls.iter().any(|c| c.starts_with("open_window editor")));
}

#[test]
fn the_card_opens_the_editor_and_the_close_button_closes_it() {
    let mut r = rig();
    r.plugin.on_ui(&mut r.host, &UiEvent { ui: "card".into(), callback: "edit-clicked".into(), args: vec![] });
    assert!(r.plugin.editor_open());
    r.ui("close-clicked", json!([]));
    assert!(r.host.calls.iter().any(|c| c == "close_window editor"));
}

#[test]
fn dragging_a_stream_moves_it_and_snaps_to_the_middle() {
    let mut r = open();
    r.picture(vec![Tile::boxed("a", "", 0.0, 0.0, 1.0, 1.0), Tile::boxed("b", "", 0.1, 0.1, 0.3, 0.3)]);
    r.pointer("down", (0.2, 0.2), false, NO_KEYS);
    assert_eq!(r.props()["selected"], json!(1));
    r.pointer("move", (0.448, 0.2), false, NO_KEYS); // its centre ends up just short of the middle
    assert_eq!(r.props()["guide-xs"], json!([0.5]));
    r.pointer("up", (0.448, 0.2), false, NO_KEYS);
    let tile = r.tile(1);
    assert!(near(tile.x + tile.w / 2.0, 0.5, 1e-9));
    assert_eq!(r.props()["guide-xs"], json!([]));
}

#[test]
fn dragging_a_corner_keeps_the_shape() {
    let mut r = open();
    r.picture(vec![Tile::boxed("b", "", 0.1, 0.1, 0.4, 0.2)]);
    r.plugin.select(0);
    r.drag((0.5, 0.3), (0.7, 0.32), NO_KEYS);
    let tile = r.tile(0);
    assert!(near(tile.w, 0.6, 0.01) && near(tile.w / tile.h, 2.0, 1e-9) && !tile.stretch);
}

#[test]
fn shift_stretches_and_alt_crops_from_the_editor() {
    let mut r = open();
    r.picture(vec![Tile::boxed("b", "", 0.1, 0.1, 0.4, 0.4)]);
    r.plugin.select(0);
    r.drag((0.5, 0.3), (0.7, 0.3), SHIFT);
    let tile = r.tile(0);
    assert!(tile.stretch && near(tile.w, 0.6, 0.01) && near(tile.h, 0.4, 1e-9));
    r.picture(vec![Tile::boxed("b", "", 0.1, 0.1, 0.4, 0.4)]);
    r.drag((0.1, 0.3), (0.2, 0.3), ALT);
    let tile = r.tile(0);
    assert!(near(tile.crop_l, 0.25, 0.02) && near(tile.x, 0.2, 0.01) && !tile.stretch);
}

#[test]
fn ctrl_stops_snapping() {
    let mut r = open();
    r.picture(vec![Tile::boxed("a", "", 0.0, 0.0, 1.0, 1.0), Tile::boxed("b", "", 0.1, 0.1, 0.3, 0.3)]);
    r.drag((0.2, 0.2), (0.448, 0.2), CTRL);
    let tile = r.tile(1);
    assert!(near(tile.x + tile.w / 2.0, 0.498, 0.003) && !near(tile.x + tile.w / 2.0, 0.5, 1e-6));
    r.plugin.select(1);
    let edge = tile.x + tile.w;
    r.drag((edge, 0.4), (0.745, 0.4), CTRL); // the right edge, near 0.75
    let tile = r.tile(1);
    assert!(!near(tile.x + tile.w, 0.75, 1e-4));
}

#[test]
fn scrolling_over_a_stream_zooms_into_it() {
    let mut r = open();
    r.picture(vec![Tile::boxed("b", "", 0.1, 0.1, 0.4, 0.4)]);
    r.ui("wheel", json!([0.3 * AREA.0, 0.3 * AREA.1, AREA.0, AREA.1, 240.0]));
    assert!(near(r.tile(0).zoom, 1.21, 1e-9));
    assert_eq!(r.props()["zoom"], json!(121.0));
    for _ in 0..30 {
        r.ui("wheel", json!([0.3 * AREA.0, 0.3 * AREA.1, AREA.0, AREA.1, 240.0]));
    }
    assert_eq!(r.tile(0).zoom, MAX_ZOOM);
}

#[test]
fn the_streams_list_puts_streams_in_and_takes_them_out() {
    let mut r = open();
    let tiles = vec![Tile::boxed("a", "", 0.0, 0.0, 1.0, 1.0), Tile::boxed("gone", "Old phone", 0.0, 0.0, 1.0, 1.0)];
    r.picture(tiles.clone());
    r.picture(tiles); // twice: the list is made afresh, not added to
    let texts: Vec<String> = r.props()["streams"].as_array().unwrap().iter().map(|s| s["text"].as_str().unwrap().to_string()).collect();
    assert_eq!(texts, ["Phone A", "Phone B", "Old phone (not streaming)"]);
    r.ui("stream-toggled", json!([1, true]));
    assert_eq!(r.sources(), ["a", "gone", "b"]);
    assert_eq!(r.props()["selected"], json!(2));
    r.ui("stream-toggled", json!([0, false]));
    assert_eq!(r.sources(), ["gone", "b"]);
}

#[test]
fn big_and_small_makes_the_stream_that_started_first_the_big_one() {
    let mut r = rig();
    let now = monotonic_ns();
    *r.store.frames.lock().unwrap() = vec![
        stream("z-camera", "Z", numbered(solid([0, 0, 0], (90, 160)), 1, now)),
        stream("a-camera", "A", numbered(solid([0, 255, 0], (90, 160)), 2, now)),
    ];
    r.plugin.open_editor(&mut r.host);
    assert_eq!(r.plugin.streams().iter().map(|s| s.0.as_str()).collect::<Vec<_>>(), ["z-camera", "a-camera"]);
    r.ui("start-from", json!([1]));
    assert_eq!(r.sources()[0], "z-camera");
    assert_eq!(r.tile(0).w, 1.0);
}

#[test]
fn starting_from_a_layout_and_saving_it() {
    let mut r = open();
    r.ui("start-from", json!([0])); // side by side
    let middles: Vec<f64> = r.plugin.layout().tiles.iter().map(|t| ((t.y + t.h / 2.0) * 1000.0).round() / 1000.0).collect();
    assert_eq!(middles, [0.5, 0.5]);
    r.name("save-clicked", "Talk");
    assert_eq!(r.plugin.names(), ["Talk"]);
    assert_eq!((r.props()["layout-current"].clone(), r.props()["saved"].clone()), (json!(0), json!(true)));
}

#[test]
fn renaming_onto_another_layouts_name_asks_first() {
    let mut r = open();
    r.plugin.save_layout(&mut r.host, "One");
    r.plugin.set_layout(&mut r.host, Layout::of(vec![Tile::new("a")]));
    r.plugin.save_layout(&mut r.host, "Two");
    r.name("rename-clicked", "One");
    assert_eq!(r.props()["confirming"], json!(true));
    r.ui("replace-answered", json!([false]));
    assert_eq!(r.plugin.names(), ["One", "Two"]);
    assert_eq!(r.plugin.layout().name, "Two");
    r.name("rename-clicked", "One");
    r.ui("replace-answered", json!([true]));
    assert_eq!(r.plugin.names(), ["One"]);
    assert_eq!(r.plugin.layout().tiles.len(), 1);
}

#[test]
fn a_name_that_only_matches_once_tidied_up_still_asks_first() {
    let mut r = open();
    let long = "A".repeat(MAX_NAME);
    r.plugin.save_layout(&mut r.host, &long);
    r.plugin.set_layout(&mut r.host, Layout::of(vec![Tile::new("a")]));
    r.plugin.save_layout(&mut r.host, "Two");
    for (typed, button) in [(format!("{long} too"), "save-clicked"), (format!("<{long}>"), "rename-clicked")] {
        r.name(button, &typed);
        assert_eq!(r.props()["confirming"], json!(true), "{typed}");
        r.ui("replace-answered", json!([false]));
        assert_eq!(r.plugin.names(), [long.clone(), "Two".to_string()]);
    }
}

#[test]
fn the_picked_streams_border_corners_and_order_change_from_the_side() {
    let mut r = open();
    r.picture(vec![Tile::boxed("a", "", 0.0, 0.0, 1.0, 1.0), Tile::boxed("b", "", 0.6, 0.6, 0.3, 0.3)]);
    r.plugin.select(1);
    r.ui("border-moved", json!([6.0]));
    r.ui("radius-moved", json!([30.0]));
    let tile = r.tile(1);
    assert_eq!((tile.border, tile.radius), (6, 30));
    assert_eq!(r.props()["border-text"], json!("6 px"));
    r.ui("restack", json!([-1]));
    assert_eq!(r.sources(), ["b", "a"]);
    assert_eq!(r.props()["selected"], json!(0));
    assert_eq!((r.props()["back-enabled"].clone(), r.props()["front-enabled"].clone()), (json!(false), json!(true)));
}

#[test]
fn the_colours_change_with_the_sliders_under_their_swatch() {
    let mut r = open();
    r.ui("colour-clicked", json!(["background"]));
    r.ui("channel-moved", json!([0, 255.0]));
    assert_eq!(r.plugin.layout().background, format!("#ff{}", &DEFAULT_BACKGROUND[3..]));
    r.plugin.select(0);
    r.ui("colour-clicked", json!(["border"]));
    r.ui("channel-moved", json!([2, 0.0]));
    assert_eq!(r.tile(0).border_colour, "#ffff00");
}

#[test]
fn the_preview_draws_while_the_window_is_open() {
    let mut r = open();
    r.plugin.on_event(&mut r.host, &Event::TimerFired { owner: NAME.into(), token: CompositePlugin::EDITOR_TIMER.into() });
    assert!(r.host.images.contains_key(&("editor".to_string(), "picture".to_string())));
    r.ui("closed", json!([]));
    r.plugin.on_event(&mut r.host, &Event::TimerFired { owner: NAME.into(), token: CompositePlugin::EDITOR_TIMER.into() });
    assert!(r.host.scheduled.is_empty(), "no more after it closes");
}
