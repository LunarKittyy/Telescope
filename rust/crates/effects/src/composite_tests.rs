//! test_composite.py's layout maths and saving, by the same names.

use super::*;
use serde_json::json;

const WIDE: f64 = 16.0 / 9.0;

fn approx(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-6 * b.abs().max(1.0)
}

fn approx4(a: (f64, f64, f64, f64), b: (f64, f64, f64, f64)) -> bool {
    approx(a.0, b.0) && approx(a.1, b.1) && approx(a.2, b.2) && approx(a.3, b.3)
}

fn t(sid: &str, x: f64, y: f64, w: f64, h: f64) -> Tile {
    Tile::boxed(sid, "", x, y, w, h)
}

fn streams(list: &[(&str, &str, f64)]) -> Vec<Stream> {
    list.iter().map(|(s, n, a)| (s.to_string(), n.to_string(), *a)).collect()
}

// ── Crop and fit ──────────────────────────────────────────────────────────

#[test]
fn a_box_the_streams_shape_shows_all_of_it() {
    assert_eq!(crop_px(&Tile::new("a"), 1920, 1080, 640, 360), (0, 0, 1920, 1080));
}

#[test]
fn a_box_of_another_shape_cuts_evenly_from_the_middle() {
    assert_eq!(crop_px(&Tile::new("a"), 1920, 1080, 500, 500), (420, 0, 1500, 1080));
    assert_eq!(crop_px(&Tile::new("a"), 1080, 1920, 1600, 900), (0, 656, 1080, 1264));
}

#[test]
fn zooming_in_shows_less_and_the_centre_stays_inside_the_frame() {
    assert_eq!(crop_px(&Tile { zoom: 2.0, ..Tile::new("a") }, 1920, 1080, 640, 360), (480, 270, 1440, 810));
    assert_eq!(crop_px(&Tile { zoom: 2.0, cx: 1.0, cy: 0.0, ..Tile::new("a") }, 1920, 1080, 640, 360), (960, 0, 1920, 540));
}

#[test]
fn fit_box_keeps_the_streams_shape_inside_its_cell() {
    let (x, y, w, h) = fit_box(16.0 / 9.0, (0.0, 0.0, 0.5, 1.0), 16.0 / 9.0);
    assert!((x, w) == (0.0, 0.5) && approx(h, 0.5) && approx(y, 0.25));
    // a phone held upright
    let (x, _y, w, h) = fit_box(9.0 / 16.0, (0.0, 0.0, 1.0, 1.0), 16.0 / 9.0);
    assert!(h == 1.0 && approx(w * 16.0 / 9.0 / h, 9.0 / 16.0) && approx(x, (1.0 - w) / 2.0));
}

#[test]
fn box_px_rounds_and_stays_inside_the_picture() {
    assert_eq!(box_px(&t("a", 0.25, 0.5, 0.5, 0.5), 1920, 1080), (480, 540, 1440, 1080));
    assert_eq!(box_px(&t("a", 0.999, 0.999, 0.0001, 0.0001), 100, 100), (99, 99, 100, 100));
}

// ── Moving, resizing, snapping and order ──────────────────────────────────

#[test]
fn a_moved_box_stays_in_the_picture() {
    let tile = moved(&Tile { w: 0.4, h: 0.3, ..Tile::new("a") }, 0.9, -0.2);
    assert!(approx(tile.x, 0.6) && tile.y == 0.0);
}

#[test]
fn resizing_from_a_corner_moves_only_its_edges() {
    assert!(approx4(resized((0.2, 0.2, 0.4, 0.4), "br", 0.1, 0.05, None), (0.2, 0.2, 0.5, 0.45)));
    assert!(approx4(resized((0.2, 0.2, 0.4, 0.4), "tl", -0.1, 0.1, None), (0.1, 0.3, 0.5, 0.3)));
    assert!(approx4(resized((0.2, 0.2, 0.4, 0.4), "r", 5.0, 0.0, None), (0.2, 0.2, 0.8, 0.4)));
    let (_x, _y, w, _h) = resized((0.2, 0.2, 0.4, 0.4), "l", 1.0, 0.0, None);
    assert!(approx(w, MIN_SIZE));
}

#[test]
fn resizing_with_the_shape_kept_holds_its_aspect_and_fits() {
    let (x, y, w, h) = resized((0.1, 0.1, 0.4, 0.2), "br", 0.2, 0.0, Some(2.0));
    assert!(approx(w / h, 2.0) && (x, y) == (0.1, 0.1) && approx(w, 0.6));
    // past the edge
    let (x, y, w, h) = resized((0.1, 0.1, 0.4, 0.2), "br", 2.0, 2.0, Some(2.0));
    assert!(approx(w / h, 2.0) && x + w <= 1.0 + 1e-9 && y + h <= 1.0 + 1e-9);
    let (x, y, w, h) = resized((0.5, 0.5, 0.4, 0.2), "tl", -0.2, 0.0, Some(2.0));
    assert!(approx(w / h, 2.0) && approx(x + w, 0.9) && approx(y + h, 0.7));
}

#[test]
fn edges_snap_to_the_picture_and_other_streams() {
    let tiles = vec![t("a", 0.1, 0.1, 0.3, 0.3), t("b", 0.6, 0.2, 0.3, 0.3)];
    let (xs, ys) = snap_lines(&tiles, 1);
    assert!(xs.contains(&0.4) && xs.contains(&0.5) && xs.contains(&0.25) && ys.contains(&0.1));
    let (d, line) = snap(&[0.405, 0.555, 0.705], &xs, SNAP);
    assert!(approx(d, -0.005) && line == Some(0.4));
    assert_eq!(snap(&[0.3], &[0.0, 1.0], SNAP), (0.0, None));
}

#[test]
fn bringing_forward_and_sending_back_change_the_order() {
    let tiles = vec![Tile::new("a"), Tile::new("b"), Tile::new("c")];
    let ids = |ts: Vec<Tile>| ts.into_iter().map(|t| t.source_id).collect::<Vec<_>>();
    assert_eq!(ids(raised(&tiles, 0, 1)), ["b", "a", "c"]);
    assert_eq!(ids(raised(&tiles, 2, -5)), ["c", "a", "b"]);
    assert_eq!(raised(&tiles, 7, 1), tiles);
}

#[test]
fn the_starting_layouts_fill_the_picture_sensibly() {
    let all = streams(&[("a", "A", 16.0 / 9.0), ("b", "B", 16.0 / 9.0), ("c", "C", 9.0 / 16.0)]);
    let side = side_by_side(&all[..2], 16.0 / 9.0);
    assert!(side.iter().all(|t| approx(t.y + t.h / 2.0, 0.5)) && side[0].x + side[0].w < side[1].x);
    let big = big_and_small(&all, 16.0 / 9.0);
    assert!((big[0].w, big[0].h) == (1.0, 1.0) && big[1].border != 0 && big[2].y + big[2].h < big[1].y);
    assert!(big.iter().all(|t| 0.0 <= t.x && t.x + t.w <= 1.0 && 0.0 <= t.y && t.y + t.h <= 1.0));
    assert!(grid(&all, 16.0 / 9.0).len() == 3 && grid(&[], 16.0 / 9.0).is_empty());
}

#[test]
fn big_and_small_shrinks_upright_phones_so_the_stack_fits() {
    let mut all = streams(&[("a", "A", 16.0 / 9.0)]);
    all.extend(["b", "c", "d"].map(|c| (c.to_string(), c.to_string(), 9.0 / 16.0)));
    let small = big_and_small(&all, 16.0 / 9.0)[1..].to_vec();
    assert!(small.iter().all(|t| t.y > 0.0 && t.y + t.h < 1.0));
    // no overlaps
    assert!(small.windows(2).all(|p| p[1].y + p[1].h < p[0].y));
    // still their own shape
    assert!(small.iter().all(|t| approx(t.w / t.h * 16.0 / 9.0, 9.0 / 16.0)));
}

// ── Scaling, stretching and cropping like OBS ─────────────────────────────

#[test]
fn a_plain_drag_scales_the_stream_keeping_its_shape() {
    let tile = t("a", 0.2, 0.2, 0.4, 0.4);
    let corner = dragged(&tile, "br", 0.2, 0.0, "scale", WIDE, None);
    assert!(approx(corner.w / corner.h, 1.0) && approx(corner.w, 0.6) && (corner.x, corner.y) == (0.2, 0.2));
    // from the opposite side, centred the other way
    let side = dragged(&tile, "r", 0.1, 0.0, "scale", WIDE, None);
    assert!(side.x == 0.2 && approx(side.w, 0.5) && approx(side.h, 0.5));
    assert!(approx(side.y + side.h / 2.0, 0.4));
    let top = dragged(&tile, "t", 0.0, -0.1, "scale", WIDE, None);
    assert!(approx(top.y + top.h, 0.6) && approx(top.w, 0.5) && approx(top.x + top.w / 2.0, 0.4));
    // stops where the picture ends
    let huge = dragged(&tile, "l", -5.0, 0.0, "scale", WIDE, None);
    assert!(huge.x >= 0.0 && huge.y >= 0.0 && huge.x + huge.w <= 1.0 + 1e-9 && huge.y + huge.h <= 1.0 + 1e-9);
}

#[test]
fn shift_stretches_the_stream_to_the_new_box() {
    // a 16:9 box for a 16:9 stream
    let tile = t("a", 0.2, 0.2, 0.4, 0.4);
    let wide = dragged(&tile, "r", 0.2, 0.0, "stretch", WIDE, Some(WIDE));
    assert!((wide.x, wide.y) == (0.2, 0.2) && approx(wide.w, 0.6) && approx(wide.h, 0.4) && wide.stretch);
    // all of it, squashed
    assert_eq!(crop_px(&wide, 1920, 1080, 1152, 432), (0, 0, 1920, 1080));
}

#[test]
fn stretching_a_centre_cropped_stream_keeps_what_it_showed() {
    // narrower than the stream: its middle is shown
    let tile = t("a", 0.2, 0.2, 0.3, 0.4);
    let before = crop_px(&tile, 1920, 1080, 576, 432);
    let stretched = dragged(&tile, "b", 0.0, 0.0, "stretch", WIDE, Some(WIDE));
    assert!(stretched.stretch && stretched.crop_l > 0.0 && approx(stretched.crop_l, stretched.crop_r));
    assert_eq!(crop_px(&stretched, 1920, 1080, 576, 432), before);
}

#[test]
fn alt_crops_the_side_being_dragged_and_leaves_the_rest_in_place() {
    let tile = t("a", 0.2, 0.2, 0.4, 0.4);
    let left = dragged(&tile, "l", 0.1, 0.0, "crop", WIDE, Some(WIDE));
    assert!(approx(left.x, 0.3) && approx(left.w, 0.3) && approx(left.h, 0.4));
    assert!(approx(left.crop_l, 0.25) && (left.crop_t, left.crop_r, left.crop_b) == (0.0, 0.0, 0.0));
    // the same size as before on screen
    assert_eq!(crop_px(&left, 1920, 1080, 576, 432), (480, 0, 1920, 1080));
    let corner = dragged(&left, "br", -0.1, -0.2, "crop", WIDE, Some(WIDE));
    assert!(approx(corner.crop_l, 0.25) && approx(corner.crop_r, 0.25) && approx(corner.crop_b, 0.5));
    assert!(approx(corner.x, 0.3) && corner.y == 0.2);
}

#[test]
fn cropping_outwards_brings_back_only_what_was_cut_off() {
    let tile = dragged(&t("a", 0.2, 0.2, 0.4, 0.4), "l", 0.1, 0.0, "crop", WIDE, Some(WIDE));
    let back = dragged(&tile, "l", -0.5, 0.0, "crop", WIDE, Some(WIDE));
    assert!(back.crop_l == 0.0 && approx(back.x, 0.2) && approx(back.w, 0.4));
    // nothing more to show
    assert!(approx(dragged(&back, "r", 5.0, 0.0, "crop", WIDE, Some(WIDE)).w, 0.4));
}

#[test]
fn a_cropped_stream_scales_with_its_crop() {
    let cropped = dragged(&t("a", 0.2, 0.2, 0.4, 0.4), "l", 0.1, 0.0, "crop", WIDE, Some(WIDE));
    let bigger = dragged(&cropped, "br", 0.15, 0.0, "scale", WIDE, None);
    assert!(bigger.crop_l == cropped.crop_l && approx(bigger.w / bigger.h, cropped.w / cropped.h));
}

#[test]
fn stretching_or_cropping_a_stream_thats_off_only_moves_the_box() {
    let tile = t("gone", 0.2, 0.2, 0.4, 0.4);
    for how in ["stretch", "crop"] {
        let moved = dragged(&tile, "r", 0.1, 0.0, how, WIDE, None);
        assert!(approx(moved.w, 0.5) && !moved.stretch && moved.crop_r == 0.0);
    }
}

#[test]
fn fitting_its_shape_undoes_stretch_crop_and_zoom() {
    let tile = dragged(&Tile { zoom: 2.0, ..t("a", 0.2, 0.2, 0.4, 0.4) }, "l", 0.1, 0.0, "crop", WIDE, Some(WIDE));
    let tile = dragged(&tile, "t", 0.0, 0.1, "stretch", WIDE, Some(WIDE));
    let f = fitted(&tile, WIDE, WIDE);
    assert_eq!((f.zoom, f.stretch, f.crop_l, f.crop_t, f.crop_r, f.crop_b), (1.0, false, 0.0, 0.0, 0.0, 0.0));
    assert!(approx(f.w / f.h, 1.0));
}

#[test]
fn zooming_stays_inside_the_crop() {
    let tile = Tile { crop_l: 0.5, zoom: 2.0, cx: 0.0, ..Tile::new("a") };
    assert_eq!(crop_px(&tile, 1920, 1080, 960, 1080), (960, 270, 1440, 810));
}

// ── Saving ────────────────────────────────────────────────────────────────

#[test]
fn a_layout_goes_through_settings_unchanged() {
    let laptop = Tile { zoom: 1.5, cx: 0.4, cy: 0.6, border: 4, border_colour: "#ff8800".into(), radius: 24,
                        ..Tile::boxed("browser:x", "Laptop", 0.7, 0.7, 0.25, 0.25) };
    let layout = Layout { name: "Face big".into(), tiles: vec![Tile::boxed("phone:1", "Pixel", 0.0, 0.0, 1.0, 1.0), laptop],
                          background: "#202020".into(), picture: "/home/me/bg.png".into() };
    assert_eq!(clean_layout(&layout_dict(&layout)), Some(layout));
    let cropped = Layout::named("Cropped", vec![Tile { crop_l: 0.1, crop_t: 0.2, crop_r: 0.3, crop_b: 0.05, stretch: true, ..Tile::new("a") }]);
    assert_eq!(clean_layout(&layout_dict(&cropped)), Some(cropped));
}

#[test]
fn a_layout_saved_before_crop_and_stretch_still_loads_and_looks_the_same() {
    let old = json!({"name": "Old", "background": "#202020", "picture": "", "tiles": [
        {"source_id": "a", "name": "A", "x": 0.1, "y": 0.1, "w": 0.3, "h": 0.4, "zoom": 1.5, "cx": 0.4, "cy": 0.6,
         "border": 2, "border_colour": "#ffffff", "radius": 8}]});
    let tile = clean_layout(&old).unwrap().tiles[0].clone();
    assert_eq!((tile.crop_l, tile.crop_t, tile.crop_r, tile.crop_b, tile.stretch), (0.0, 0.0, 0.0, 0.0, false));
    // zoomed around its centre, as before
    assert_eq!(crop_px(&tile, 1920, 1080, 500, 500), (408, 288, 1128, 1008));
}

#[test]
fn broken_crop_values_still_leave_some_of_the_stream() {
    let tile = clean_tile(&json!({"source_id": "a", "crop_l": 0.99, "crop_r": 0.5, "crop_t": -1, "crop_b": "x", "stretch": "yes"})).unwrap();
    assert!(approx(tile.crop_l + tile.crop_r, 1.0 - MIN_SHOWN) && tile.crop_r == 0.0);
    assert_eq!((tile.crop_t, tile.crop_b, tile.stretch), (0.0, 0.0, false));
}

#[test]
fn something_that_isnt_a_layout_is_none_or_empty() {
    for raw in [json!(null), json!([]), json!("x"), json!({"tiles": "nope"})] {
        let layout = clean_layout(&raw);
        assert!(layout.is_none_or(|l| l.tiles.is_empty()), "{raw}");
    }
}

#[test]
fn broken_values_are_fixed_rather_than_dropped() {
    // Python's float("nan") can't be in a JSON settings file: it loads as null, which is what's tested here
    let layout = clean_layout(&json!({"name": format!("<b>x</b>{}", "y".repeat(80)), "background": "red", "tiles": [
        {"source_id": "a", "x": 0.9, "w": 0.5, "h": null, "zoom": 99, "border": -3, "radius": true, "border_colour": "#GGGGGG"},
        {"source_id": "a"}, {"name": "no id"}, {"source_id": "b"}, {"source_id": "c"}, {"source_id": "d"}, {"source_id": "e"}]})).unwrap();
    let tile = &layout.tiles[0];
    assert!(layout.name.chars().count() == MAX_NAME && !layout.name.contains('<'));
    assert_eq!(layout.background, DEFAULT_BACKGROUND);
    assert_eq!((tile.x, tile.w, tile.h, tile.zoom, tile.border, tile.radius, tile.border_colour.as_str()),
               (0.5, 0.5, 1.0, MAX_ZOOM, 0, 0, "#ffffff"));
    // one each, at most four
    assert_eq!(layout.tiles.iter().map(|t| t.source_id.as_str()).collect::<Vec<_>>(), ["a", "b", "c", "d"]);
}

#[test]
fn saved_layouts_need_a_name_each() {
    let raw = json!([{"name": "A"}, {"name": ""}, {"name": "A", "background": "#ffffff"}, {"name": "B"}, "junk"]);
    assert_eq!(clean_layouts(&raw).iter().map(|l| l.name.as_str()).collect::<Vec<_>>(), ["A", "B"]);
    let many: Vec<Value> = (0..50).map(|i| json!({"name": i.to_string()})).collect();
    assert_eq!(clean_layouts(&Value::Array(many)).len(), MAX_LAYOUTS);
}

#[test]
fn names_keep_only_what_prints() {
    assert_eq!(clean_name("  Side by side "), "Side by side");
    assert_eq!(clean_name("<b>a\u{202e}\tb</b>"), "bab/b");
    assert_eq!(bgr("#0000FF"), [255, 0, 0]);
    assert_eq!(bgr("nope"), [0, 0, 0]);
}
